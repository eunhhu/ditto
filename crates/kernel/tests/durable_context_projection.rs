//! Task 004 keeps this admission matrix in one integration fixture because the
//! contract couples real `state.db`/`context-projection.db` transactions,
//! broadcast ordering, canonical replay, and adversarial authority bounds. The
//! shared fixture makes every pre/post-append count and publication assertion
//! comparable without adding production test hooks or fake stores.

use std::{
    error::Error as _,
    sync::{Arc, Barrier},
    thread,
};

use chrono::{Duration, Utc};
use ditto_context::{
    ContextLens, ContextNode, ContextNodeKind, ContextOrigin, ContextScope, EpistemicStatus,
};
use ditto_context_projection::{
    CONTEXT_NODE_EVENT_VERSION, ContextNodeRecordedPayloadV1, ContextProjection,
    ContextProjectionError, MAX_CONTEXT_NODE_ID_BYTES, MAX_CONTEXT_REFERENCE_ID_BYTES,
    MAX_CONTEXT_SOURCE_EVENT_IDS, MAX_CONTEXT_SUMMARY_BYTES, MAX_CONTEXT_SUPERSEDES,
    MAX_SERIALIZED_CONTEXT_NODE_BYTES,
};
use ditto_event_store::EventStore;
use ditto_kernel::{
    COMMITTED_BUT_PROJECTION_UNAVAILABLE, DittoKernel, KernelConfig, KernelError,
    TrustedContextNodeDraft,
};
use ditto_protocol::{EventActor, EventQuery, EventRecord, NewEvent, event_kind};
use rusqlite::Connection;
use serde_json::json;
use tempfile::TempDir;
use tokio::sync::broadcast::error::TryRecvError;

const SESSION_A: &str = "session-a";
const SESSION_B: &str = "session-b";
const TASK_A: &str = "task-a";
const TASK_B: &str = "task-b";

struct Fixture {
    root: TempDir,
    config: KernelConfig,
    data_dir: std::path::PathBuf,
    kernel: DittoKernel,
    store: EventStore,
    projection: ContextProjection,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("temporary fixture directory");
        let data_dir = root.path().join("data");
        let capabilities =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../capabilities");
        let config = KernelConfig::new(&data_dir, capabilities);
        let kernel = DittoKernel::open(config.clone()).expect("open fixture kernel");
        let store = EventStore::open(data_dir.join("state.db")).expect("open fixture event store");
        let projection = ContextProjection::open_in(&data_dir).expect("open fixture projection");
        Self {
            root,
            config,
            data_dir,
            kernel,
            store,
            projection,
        }
    }

    fn close(self) -> (TempDir, KernelConfig) {
        let Self {
            root,
            config,
            kernel,
            store,
            projection,
            ..
        } = self;
        drop((kernel, store, projection));
        (root, config)
    }
}

fn append_source(
    fixture: &Fixture,
    session_id: &str,
    task_id: Option<&str>,
    actor: EventActor,
    label: &str,
) -> EventRecord {
    fixture
        .store
        .append(NewEvent {
            session_id: Some(session_id.to_owned()),
            task_id: task_id.map(str::to_owned),
            actor,
            kind: format!("fixture.source.{label}"),
            payload: json!({"label": label}),
            causation_id: None,
            correlation_id: Some(task_id.unwrap_or(session_id).to_owned()),
            span_id: None,
        })
        .expect("append fixture source")
}

fn node(
    id: impl Into<String>,
    scope: ContextScope,
    origin: ContextOrigin,
    epistemic: EpistemicStatus,
    source_event_ids: Vec<String>,
    supersedes: Vec<String>,
    summary: impl Into<String>,
) -> ContextNode {
    ContextNode {
        id: id.into(),
        kind: ContextNodeKind::Claim,
        summary: summary.into(),
        origin,
        epistemic,
        scope,
        lens: ContextLens::Task,
        confidence: 0.9,
        source_event_ids,
        supersedes,
        valid_from: None,
        valid_until: None,
    }
}

fn task_user_node(
    id: impl Into<String>,
    source_event_id: &str,
    summary: impl Into<String>,
) -> ContextNode {
    node(
        id,
        ContextScope::Task,
        ContextOrigin::User,
        EpistemicStatus::Asserted,
        vec![source_event_id.to_owned()],
        Vec::new(),
        summary,
    )
}

fn assert_same_event(left: &EventRecord, right: &EventRecord) {
    assert_eq!(
        serde_json::to_value(left).expect("serialize left event"),
        serde_json::to_value(right).expect("serialize right event")
    );
}

fn observe_exactly_once(
    receiver: &mut tokio::sync::broadcast::Receiver<EventRecord>,
    expected: &EventRecord,
) {
    let observed = receiver.try_recv().expect("pre-subscribed event");
    assert_same_event(&observed, expected);
    assert!(matches!(receiver.try_recv(), Err(TryRecvError::Empty)));
}

fn assert_no_publication(receiver: &mut tokio::sync::broadcast::Receiver<EventRecord>) {
    assert!(matches!(receiver.try_recv(), Err(TryRecvError::Empty)));
}

fn assert_committed_projection_outcome(
    error: &KernelError,
    expected_detail: &str,
    forbidden_raw_detail: &str,
) -> EventRecord {
    assert_eq!(
        error.outcome_code(),
        Some(COMMITTED_BUT_PROJECTION_UNAVAILABLE)
    );
    let (event, detail) = match error {
        KernelError::CommittedButProjectionUnavailable { event, source } => {
            (event.as_ref().clone(), source.detail())
        }
        other => panic!("expected committed projection outcome, got {other:?}"),
    };
    assert_eq!(detail, expected_detail);
    assert!(!detail.contains(forbidden_raw_detail));
    assert!(!error.to_string().contains(forbidden_raw_detail));
    let exposed_source = error.source().expect("sanitized error source");
    assert_eq!(exposed_source.to_string(), detail);
    assert!(exposed_source.source().is_none());
    event
}

fn reject_without_append(
    fixture: &Fixture,
    receiver: &mut tokio::sync::broadcast::Receiver<EventRecord>,
    draft: TrustedContextNodeDraft,
) -> KernelError {
    let before = fixture
        .kernel
        .event_count()
        .expect("event count before rejection");
    let error = fixture
        .kernel
        .admit_context_node(draft)
        .expect_err("draft must be rejected before append");
    assert_eq!(
        fixture
            .kernel
            .event_count()
            .expect("event count after rejection"),
        before
    );
    assert_ne!(
        error.outcome_code(),
        Some(COMMITTED_BUT_PROJECTION_UNAVAILABLE)
    );
    assert_no_publication(receiver);
    error
}

fn assert_durable_bytes_exceeded(
    error: KernelError,
    expected_field: &'static str,
    expected_actual: usize,
    expected_maximum: usize,
) {
    assert!(matches!(
        error,
        KernelError::ContextProjection(ContextProjectionError::DurableBytesExceeded {
            field,
            actual,
            maximum,
        }) if field == expected_field
            && actual == expected_actual
            && maximum == expected_maximum
    ));
}

fn assert_durable_list_out_of_range(
    error: KernelError,
    expected_field: &'static str,
    expected_actual: usize,
    expected_minimum: usize,
    expected_maximum: usize,
) {
    assert!(matches!(
        error,
        KernelError::ContextProjection(ContextProjectionError::DurableListOutOfRange {
            field,
            actual,
            minimum,
            maximum,
        }) if field == expected_field
            && actual == expected_actual
            && minimum == expected_minimum
            && maximum == expected_maximum
    ));
}

fn admit_and_observe(
    fixture: &Fixture,
    receiver: &mut tokio::sync::broadcast::Receiver<EventRecord>,
    draft: TrustedContextNodeDraft,
) -> EventRecord {
    let event = fixture
        .kernel
        .admit_context_node(draft)
        .expect("trusted admission succeeds");
    observe_exactly_once(receiver, &event);
    event
}

#[test]
fn trusted_context_admission_checkpoints_before_publish_and_derives_greatest_seq_causation() {
    let fixture = Fixture::new();
    let first = append_source(&fixture, SESSION_A, None, EventActor::User, "first");
    let second = append_source(
        &fixture,
        SESSION_A,
        Some(TASK_A),
        EventActor::User,
        "second",
    );
    let greatest = append_source(&fixture, SESSION_A, None, EventActor::User, "greatest");
    let sources = [
        first.event_id.clone(),
        second.event_id.clone(),
        greatest.event_id.clone(),
    ];
    let connection = Connection::open(fixture.data_dir.join("state.db"))
        .expect("open event interleave trigger connection");
    connection
        .execute_batch(
            "CREATE TRIGGER interleave_before_first_context
             BEFORE INSERT ON events
             WHEN NEW.kind = 'context.node.recorded'
              AND NEW.payload_json LIKE '%causation-permutation-0%'
             BEGIN
               INSERT INTO events (
                 event_id, recorded_at, session_id, task_id, actor, kind,
                 payload_json, causation_id, correlation_id, span_id
               ) VALUES (
                 'fixture-interleaved-before-context', NEW.recorded_at,
                 NEW.session_id, NEW.task_id, 'system', 'fixture.interleave',
                 '{\"interleaved\":true}', NULL, NEW.correlation_id, NULL
               );
             END;",
        )
        .expect("install deterministic event interleave trigger");
    drop(connection);
    let permutations = [
        [0, 1, 2],
        [0, 2, 1],
        [1, 0, 2],
        [1, 2, 0],
        [2, 0, 1],
        [2, 1, 0],
    ];
    let mut receiver = fixture.kernel.subscribe();
    let mut last = None;

    for (index, permutation) in permutations.into_iter().enumerate() {
        let source_event_ids = permutation
            .into_iter()
            .map(|source| sources[source].clone())
            .collect::<Vec<_>>();
        let node_id = format!("causation-permutation-{index}");
        let expected_node = node(
            &node_id,
            ContextScope::Task,
            ContextOrigin::User,
            EpistemicStatus::Asserted,
            source_event_ids,
            Vec::new(),
            "derive causation from the greatest durable source sequence",
        );
        let event = fixture
            .kernel
            .admit_context_node(TrustedContextNodeDraft::task(
                SESSION_A,
                TASK_A,
                expected_node.clone(),
            ))
            .expect("admit causation permutation");

        assert_eq!(event.actor, EventActor::System);
        assert_eq!(event.kind, event_kind::CONTEXT_NODE_RECORDED);
        assert_eq!(event.session_id.as_deref(), Some(SESSION_A));
        assert_eq!(event.task_id.as_deref(), Some(TASK_A));
        assert_eq!(
            event.causation_id.as_deref(),
            Some(greatest.event_id.as_str())
        );
        assert_eq!(event.correlation_id.as_deref(), Some(TASK_A));
        assert_eq!(event.span_id, None);
        let payload: ContextNodeRecordedPayloadV1 =
            serde_json::from_value(event.payload.clone()).expect("decode context payload");
        assert_eq!(payload.event_version, CONTEXT_NODE_EVENT_VERSION);
        assert_eq!(payload.node, expected_node);
        if index == 0 {
            assert_eq!(event.seq, greatest.seq + 2);
            let interleaved = fixture
                .store
                .get_by_seq(event.seq - 1)
                .expect("read interleaved event")
                .expect("interleaved event exists");
            assert_eq!(interleaved.event_id, "fixture-interleaved-before-context");
            assert_eq!(interleaved.kind, "fixture.interleave");
        }

        // Publication is attempted only after this exact durable event is the
        // projection anchor and its node is scope-visible.
        let checkpoint = fixture
            .projection
            .checkpoint()
            .expect("projection checkpoint");
        assert_eq!(checkpoint.through_seq, event.seq);
        assert_eq!(
            checkpoint.through_event_id.as_deref(),
            Some(event.event_id.as_str())
        );
        let snapshot = fixture
            .projection
            .capture_snapshot(SESSION_A, Some(TASK_A))
            .expect("task projection snapshot");
        assert!(
            snapshot
                .candidates()
                .iter()
                .any(|candidate| candidate.id == node_id)
        );
        observe_exactly_once(&mut receiver, &event);
        last = Some(event);
    }

    let connection = Connection::open(fixture.data_dir.join("state.db"))
        .expect("open interleave trigger cleanup connection");
    connection
        .execute_batch("DROP TRIGGER interleave_before_first_context;")
        .expect("drop event interleave trigger");
    drop(connection);

    let last = last.expect("at least one accepted event");
    drop(receiver);
    let (_root, config) = fixture.close();
    let reopened = DittoKernel::open(config.clone()).expect("reopen synchronized kernel");
    let reopened_event = reopened
        .list_events(&EventQuery {
            session_id: Some(SESSION_A.to_owned()),
            ..EventQuery::default()
        })
        .expect("list reopened events")
        .into_iter()
        .find(|event| event.event_id == last.event_id)
        .expect("reopened accepted event");
    assert_same_event(&reopened_event, &last);
    let reopened_projection =
        ContextProjection::open_in(&config.data_dir).expect("open recovered projection");
    assert_eq!(
        reopened_projection
            .checkpoint()
            .expect("reopened checkpoint")
            .through_seq,
        last.seq
    );
}

#[test]
fn cloned_admissions_serialize_session_wide_identity() {
    let fixture = Fixture::new();
    let source = append_source(&fixture, SESSION_A, None, EventActor::User, "race");
    let before = fixture.kernel.event_count().expect("race baseline count");
    let mut receiver = fixture.kernel.subscribe();
    let gate = Arc::new(Barrier::new(3));

    let handles = [TASK_A, TASK_B].map(|task_id| {
        let kernel = fixture.kernel.clone();
        let gate = Arc::clone(&gate);
        let source_event_id = source.event_id.clone();
        thread::spawn(move || {
            gate.wait();
            kernel.admit_context_node(TrustedContextNodeDraft::task(
                SESSION_A,
                task_id,
                task_user_node(
                    "session-wide-race",
                    &source_event_id,
                    format!("candidate from {task_id}"),
                ),
            ))
        })
    });
    gate.wait();
    let [left, right] = handles.map(|handle| handle.join().expect("admission thread"));
    let (accepted, duplicate) = match (left, right) {
        (Ok(event), Err(error)) | (Err(error), Ok(event)) => (event, error),
        other => panic!("expected one acceptance and one duplicate, got {other:?}"),
    };

    assert!(matches!(
        duplicate,
        KernelError::DuplicateContextNodeIdentity {
            ref session_id,
            ref node_id,
            ref event_id,
            event_seq,
        } if session_id == SESSION_A
            && node_id == "session-wide-race"
            && event_id == &accepted.event_id
            && event_seq == accepted.seq
    ));
    assert_eq!(
        fixture.kernel.event_count().expect("race final count"),
        before + 1
    );
    observe_exactly_once(&mut receiver, &accepted);

    let task_a = fixture
        .projection
        .capture_snapshot(SESSION_A, Some(TASK_A))
        .expect("task-a snapshot");
    let task_b = fixture
        .projection
        .capture_snapshot(SESSION_A, Some(TASK_B))
        .expect("task-b snapshot");
    let total = task_a
        .candidates()
        .iter()
        .chain(task_b.candidates())
        .filter(|candidate| candidate.id == "session-wide-race")
        .count();
    assert_eq!(total, 1);
}

#[test]
fn invalid_scope_origin_sources_and_session_wide_duplicates_fail_before_append() {
    let fixture = Fixture::new();
    let user_session = append_source(&fixture, SESSION_A, None, EventActor::User, "user-session");
    let user_task_b = append_source(
        &fixture,
        SESSION_A,
        Some(TASK_B),
        EventActor::User,
        "user-task-b",
    );
    let user_other_session = append_source(
        &fixture,
        SESSION_B,
        None,
        EventActor::User,
        "user-other-session",
    );
    let model_source = append_source(&fixture, SESSION_A, None, EventActor::Model, "model");
    let system_source = append_source(&fixture, SESSION_A, None, EventActor::System, "system");
    let mut receiver = fixture.kernel.subscribe();

    reject_without_append(
        &fixture,
        &mut receiver,
        TrustedContextNodeDraft::session(
            SESSION_A,
            task_user_node(
                "session-envelope-mismatch",
                &user_session.event_id,
                "mismatch",
            ),
        ),
    );
    reject_without_append(
        &fixture,
        &mut receiver,
        TrustedContextNodeDraft::task(
            SESSION_A,
            TASK_A,
            node(
                "task-envelope-mismatch",
                ContextScope::Session,
                ContextOrigin::User,
                EpistemicStatus::Asserted,
                vec![user_session.event_id.clone()],
                Vec::new(),
                "mismatch",
            ),
        ),
    );
    reject_without_append(
        &fixture,
        &mut receiver,
        TrustedContextNodeDraft::session(
            "",
            node(
                "empty-session",
                ContextScope::Session,
                ContextOrigin::User,
                EpistemicStatus::Asserted,
                vec![user_session.event_id.clone()],
                Vec::new(),
                "empty session",
            ),
        ),
    );
    reject_without_append(
        &fixture,
        &mut receiver,
        TrustedContextNodeDraft::task(
            SESSION_A,
            "",
            task_user_node("empty-task", &user_session.event_id, "empty task"),
        ),
    );

    for scope in [
        ContextScope::Turn,
        ContextScope::Project,
        ContextScope::Device,
        ContextScope::Global,
    ] {
        reject_without_append(
            &fixture,
            &mut receiver,
            TrustedContextNodeDraft::session(
                SESSION_A,
                node(
                    format!("unsupported-{scope:?}"),
                    scope,
                    ContextOrigin::User,
                    EpistemicStatus::Asserted,
                    vec![user_session.event_id.clone()],
                    Vec::new(),
                    "unsupported scope",
                ),
            ),
        );
    }

    reject_without_append(
        &fixture,
        &mut receiver,
        TrustedContextNodeDraft::task(
            SESSION_A,
            TASK_A,
            task_user_node(
                "cross-session-source",
                &user_other_session.event_id,
                "cross-session",
            ),
        ),
    );
    reject_without_append(
        &fixture,
        &mut receiver,
        TrustedContextNodeDraft::task(
            SESSION_A,
            TASK_A,
            task_user_node("cross-task-source", &user_task_b.event_id, "cross-task"),
        ),
    );
    reject_without_append(
        &fixture,
        &mut receiver,
        TrustedContextNodeDraft::task(
            SESSION_A,
            TASK_A,
            task_user_node("missing-source", "missing-event", "missing"),
        ),
    );

    for (index, origin) in [
        ContextOrigin::User,
        ContextOrigin::Model,
        ContextOrigin::Capability,
        ContextOrigin::Policy,
        ContextOrigin::System,
    ]
    .into_iter()
    .enumerate()
    {
        let mismatched_source = if origin == ContextOrigin::User {
            &system_source.event_id
        } else {
            &user_session.event_id
        };
        reject_without_append(
            &fixture,
            &mut receiver,
            TrustedContextNodeDraft::task(
                SESSION_A,
                TASK_A,
                node(
                    format!("unattested-origin-{index}"),
                    ContextScope::Task,
                    origin,
                    EpistemicStatus::Inferred,
                    vec![mismatched_source.clone()],
                    Vec::new(),
                    "origin is not attested",
                ),
            ),
        );
    }
    reject_without_append(
        &fixture,
        &mut receiver,
        TrustedContextNodeDraft::task(
            SESSION_A,
            TASK_A,
            node(
                "model-assertion",
                ContextScope::Task,
                ContextOrigin::Model,
                EpistemicStatus::Asserted,
                vec![model_source.event_id.clone()],
                Vec::new(),
                "model cannot assert",
            ),
        ),
    );

    let mut invalid_window = task_user_node(
        "invalid-window",
        &user_session.event_id,
        "invalid time window",
    );
    invalid_window.valid_from = Some(Utc::now() + Duration::minutes(2));
    invalid_window.valid_until = Some(Utc::now() + Duration::minutes(1));
    reject_without_append(
        &fixture,
        &mut receiver,
        TrustedContextNodeDraft::task(SESSION_A, TASK_A, invalid_window),
    );
    let mut invalid_confidence = task_user_node(
        "invalid-confidence",
        &user_session.event_id,
        "invalid confidence",
    );
    invalid_confidence.confidence = f32::NAN;
    reject_without_append(
        &fixture,
        &mut receiver,
        TrustedContextNodeDraft::task(SESSION_A, TASK_A, invalid_confidence),
    );

    let oversized_id = reject_without_append(
        &fixture,
        &mut receiver,
        TrustedContextNodeDraft::task(
            SESSION_A,
            TASK_A,
            task_user_node(
                "i".repeat(MAX_CONTEXT_NODE_ID_BYTES + 1),
                &user_session.event_id,
                "oversized id",
            ),
        ),
    );
    assert_durable_bytes_exceeded(
        oversized_id,
        "context node id",
        MAX_CONTEXT_NODE_ID_BYTES + 1,
        MAX_CONTEXT_NODE_ID_BYTES,
    );
    let oversized_summary = reject_without_append(
        &fixture,
        &mut receiver,
        TrustedContextNodeDraft::task(
            SESSION_A,
            TASK_A,
            task_user_node(
                "oversized-summary",
                &user_session.event_id,
                "s".repeat(MAX_CONTEXT_SUMMARY_BYTES + 1),
            ),
        ),
    );
    assert_durable_bytes_exceeded(
        oversized_summary,
        "context node summary",
        MAX_CONTEXT_SUMMARY_BYTES + 1,
        MAX_CONTEXT_SUMMARY_BYTES,
    );
    let too_many_sources = reject_without_append(
        &fixture,
        &mut receiver,
        TrustedContextNodeDraft::task(
            SESSION_A,
            TASK_A,
            node(
                "too-many-sources",
                ContextScope::Task,
                ContextOrigin::User,
                EpistemicStatus::Asserted,
                vec![user_session.event_id.clone(); MAX_CONTEXT_SOURCE_EVENT_IDS + 1],
                Vec::new(),
                "too many sources",
            ),
        ),
    );
    assert_durable_list_out_of_range(
        too_many_sources,
        "source_event_ids",
        MAX_CONTEXT_SOURCE_EVENT_IDS + 1,
        1,
        MAX_CONTEXT_SOURCE_EVENT_IDS,
    );
    let too_many_supersessions = reject_without_append(
        &fixture,
        &mut receiver,
        TrustedContextNodeDraft::task(
            SESSION_A,
            TASK_A,
            node(
                "too-many-supersessions",
                ContextScope::Task,
                ContextOrigin::User,
                EpistemicStatus::Asserted,
                vec![user_session.event_id.clone()],
                (0..=MAX_CONTEXT_SUPERSEDES)
                    .map(|index| format!("missing-{index}"))
                    .collect(),
                "too many supersessions",
            ),
        ),
    );
    assert_durable_list_out_of_range(
        too_many_supersessions,
        "supersedes",
        MAX_CONTEXT_SUPERSEDES + 1,
        0,
        MAX_CONTEXT_SUPERSEDES,
    );
    let oversized_source_reference = reject_without_append(
        &fixture,
        &mut receiver,
        TrustedContextNodeDraft::task(
            SESSION_A,
            TASK_A,
            task_user_node(
                "oversized-source-reference",
                &"r".repeat(MAX_CONTEXT_REFERENCE_ID_BYTES + 1),
                "oversized source reference",
            ),
        ),
    );
    assert_durable_bytes_exceeded(
        oversized_source_reference,
        "source_event_ids",
        MAX_CONTEXT_REFERENCE_ID_BYTES + 1,
        MAX_CONTEXT_REFERENCE_ID_BYTES,
    );
    let oversized_supersession_reference = reject_without_append(
        &fixture,
        &mut receiver,
        TrustedContextNodeDraft::task(
            SESSION_A,
            TASK_A,
            node(
                "oversized-supersession-reference",
                ContextScope::Task,
                ContextOrigin::User,
                EpistemicStatus::Asserted,
                vec![user_session.event_id.clone()],
                vec!["r".repeat(MAX_CONTEXT_REFERENCE_ID_BYTES + 1)],
                "oversized supersession reference",
            ),
        ),
    );
    assert_durable_bytes_exceeded(
        oversized_supersession_reference,
        "supersedes",
        MAX_CONTEXT_REFERENCE_ID_BYTES + 1,
        MAX_CONTEXT_REFERENCE_ID_BYTES,
    );
    reject_without_append(
        &fixture,
        &mut receiver,
        TrustedContextNodeDraft::task(
            SESSION_A,
            TASK_A,
            node(
                "duplicate-source-reference",
                ContextScope::Task,
                ContextOrigin::User,
                EpistemicStatus::Asserted,
                vec![user_session.event_id.clone(), user_session.event_id.clone()],
                Vec::new(),
                "duplicate source",
            ),
        ),
    );
    reject_without_append(
        &fixture,
        &mut receiver,
        TrustedContextNodeDraft::task(
            SESSION_A,
            TASK_A,
            node(
                "self-supersession",
                ContextScope::Task,
                ContextOrigin::User,
                EpistemicStatus::Asserted,
                vec![user_session.event_id.clone()],
                vec!["self-supersession".to_owned()],
                "self supersession",
            ),
        ),
    );

    let escaped_sources = (0..MAX_CONTEXT_SOURCE_EVENT_IDS)
        .map(|index| format!("{index:03}{}", "\\".repeat(253)))
        .collect::<Vec<_>>();
    let escaped_supersessions = (0..MAX_CONTEXT_SUPERSEDES)
        .map(|index| format!("s{index:03}{}", "\\".repeat(252)))
        .collect::<Vec<_>>();
    let serialized_node = node(
        "serialized-node-over-bound",
        ContextScope::Task,
        ContextOrigin::User,
        EpistemicStatus::Asserted,
        escaped_sources,
        escaped_supersessions,
        "\\".repeat(MAX_CONTEXT_SUMMARY_BYTES),
    );
    let serialized_actual = serde_json::to_vec(&serialized_node)
        .expect("serialize over-bound node fixture")
        .len();
    assert!(serialized_actual > MAX_SERIALIZED_CONTEXT_NODE_BYTES);
    let serialized_error = reject_without_append(
        &fixture,
        &mut receiver,
        TrustedContextNodeDraft::task(SESSION_A, TASK_A, serialized_node),
    );
    assert_durable_bytes_exceeded(
        serialized_error,
        "serialized context node",
        serialized_actual,
        MAX_SERIALIZED_CONTEXT_NODE_BYTES,
    );

    let identity_event = admit_and_observe(
        &fixture,
        &mut receiver,
        TrustedContextNodeDraft::task(
            SESSION_A,
            TASK_A,
            task_user_node("session-wide-identity", &user_session.event_id, "canonical"),
        ),
    );
    for duplicate in [
        TrustedContextNodeDraft::session(
            SESSION_A,
            node(
                "session-wide-identity",
                ContextScope::Session,
                ContextOrigin::User,
                EpistemicStatus::Asserted,
                vec![user_session.event_id.clone()],
                Vec::new(),
                "session collision",
            ),
        ),
        TrustedContextNodeDraft::task(
            SESSION_A,
            TASK_B,
            task_user_node(
                "session-wide-identity",
                &user_session.event_id,
                "sibling collision",
            ),
        ),
    ] {
        let error = reject_without_append(&fixture, &mut receiver, duplicate);
        assert!(matches!(
            error,
            KernelError::DuplicateContextNodeIdentity {
                ref event_id,
                event_seq,
                ..
            } if event_id == &identity_event.event_id && event_seq == identity_event.seq
        ));
    }
    admit_and_observe(
        &fixture,
        &mut receiver,
        TrustedContextNodeDraft::session(
            SESSION_B,
            node(
                "session-wide-identity",
                ContextScope::Session,
                ContextOrigin::User,
                EpistemicStatus::Asserted,
                vec![user_other_session.event_id.clone()],
                Vec::new(),
                "same ID is valid in another session",
            ),
        ),
    );

    let superseded = admit_and_observe(
        &fixture,
        &mut receiver,
        TrustedContextNodeDraft::task(
            SESSION_A,
            TASK_A,
            task_user_node("supersession-base", &user_session.event_id, "base"),
        ),
    );
    reject_without_append(
        &fixture,
        &mut receiver,
        TrustedContextNodeDraft::task(
            SESSION_A,
            TASK_A,
            node(
                "missing-supersession",
                ContextScope::Task,
                ContextOrigin::User,
                EpistemicStatus::Asserted,
                vec![user_session.event_id.clone()],
                vec!["never-recorded".to_owned()],
                "missing target",
            ),
        ),
    );
    for (node_id, draft) in [
        (
            "cross-task-supersession",
            TrustedContextNodeDraft::task(
                SESSION_A,
                TASK_B,
                node(
                    "cross-task-supersession",
                    ContextScope::Task,
                    ContextOrigin::User,
                    EpistemicStatus::Asserted,
                    vec![user_session.event_id.clone()],
                    vec!["supersession-base".to_owned()],
                    "cross-task",
                ),
            ),
        ),
        (
            "cross-scope-supersession",
            TrustedContextNodeDraft::session(
                SESSION_A,
                node(
                    "cross-scope-supersession",
                    ContextScope::Session,
                    ContextOrigin::User,
                    EpistemicStatus::Asserted,
                    vec![user_session.event_id.clone()],
                    vec!["supersession-base".to_owned()],
                    "cross-scope",
                ),
            ),
        ),
    ] {
        let error = reject_without_append(&fixture, &mut receiver, draft);
        assert!(
            matches!(error, KernelError::ContextProjection(_)),
            "{node_id} returned {error:?}"
        );
    }
    let replacement = admit_and_observe(
        &fixture,
        &mut receiver,
        TrustedContextNodeDraft::task(
            SESSION_A,
            TASK_A,
            node(
                "supersession-replacement",
                ContextScope::Task,
                ContextOrigin::User,
                EpistemicStatus::Asserted,
                vec![user_session.event_id],
                vec!["supersession-base".to_owned()],
                "replacement",
            ),
        ),
    );
    assert!(replacement.seq > superseded.seq);
}

#[test]
fn open_eagerly_replays_projection_and_fails_closed_on_a_forged_context_event() {
    let root = tempfile::tempdir().expect("open failure fixture");
    let data_dir = root.path().join("data");
    let store = EventStore::open(data_dir.join("state.db")).expect("open event store");
    let source = store
        .append(NewEvent {
            session_id: Some(SESSION_A.to_owned()),
            task_id: None,
            actor: EventActor::User,
            kind: "fixture.source.open".into(),
            payload: json!({"source": true}),
            causation_id: None,
            correlation_id: Some(SESSION_A.to_owned()),
            span_id: None,
        })
        .expect("append open source");
    store
        .append(NewEvent {
            session_id: Some(SESSION_A.to_owned()),
            task_id: None,
            actor: EventActor::User,
            kind: event_kind::CONTEXT_NODE_RECORDED.to_owned(),
            payload: serde_json::to_value(ContextNodeRecordedPayloadV1::new(node(
                "forged-on-open",
                ContextScope::Session,
                ContextOrigin::User,
                EpistemicStatus::Asserted,
                vec![source.event_id.clone()],
                Vec::new(),
                "forged actor",
            )))
            .expect("serialize forged payload"),
            causation_id: Some(source.event_id),
            correlation_id: Some(SESSION_A.to_owned()),
            span_id: None,
        })
        .expect("append forged context event");
    let before = store.count().expect("event count before failed open");
    drop(store);

    let capabilities = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../capabilities");
    let result = DittoKernel::open(KernelConfig::new(&data_dir, capabilities));
    assert!(matches!(
        result,
        Err(KernelError::ContextProjection(
            ContextProjectionError::InvalidActor { .. }
        ))
    ));
    let reopened_store = EventStore::open(data_dir.join("state.db")).expect("reopen event store");
    assert_eq!(
        reopened_store.count().expect("count after failed open"),
        before
    );
}

#[test]
fn committed_projection_failure_publishes_once_returns_record_and_recovers_without_duplicate() {
    let fixture = Fixture::new();
    let source = append_source(
        &fixture,
        SESSION_A,
        None,
        EventActor::User,
        "committed-failure",
    );
    let before = fixture.kernel.event_count().expect("pre-failure count");
    let mut receiver = fixture.kernel.subscribe();

    let short_raw_path = "/private/ditto/secret/context-projection.db";
    let connection = Connection::open(fixture.data_dir.join("context-projection.db"))
        .expect("open short projection trigger connection");
    connection
        .execute_batch(&format!(
            "CREATE TRIGGER fail_short_projection BEFORE INSERT ON projected_nodes
             WHEN NEW.node_id = 'short-path-redaction'
             BEGIN SELECT RAISE(ABORT, '{short_raw_path}'); END;"
        ))
        .expect("install short projection failure trigger");
    drop(connection);

    let short_error = fixture
        .kernel
        .admit_context_node(TrustedContextNodeDraft::task(
            SESSION_A,
            TASK_A,
            task_user_node(
                "short-path-redaction",
                &source.event_id,
                "short raw path stays private",
            ),
        ))
        .expect_err("short projection trigger must fail after append");
    let short_committed = assert_committed_projection_outcome(
        &short_error,
        "context projection SQLite transaction failed after the durable append",
        short_raw_path,
    );
    assert_eq!(
        fixture
            .kernel
            .event_count()
            .expect("post-short-failure count"),
        before + 1
    );
    observe_exactly_once(&mut receiver, &short_committed);

    let connection = Connection::open(fixture.data_dir.join("context-projection.db"))
        .expect("open short trigger cleanup connection");
    connection
        .execute_batch("DROP TRIGGER fail_short_projection;")
        .expect("remove short projection failure trigger");
    drop(connection);

    let oversized_trigger_detail = format!("{short_raw_path}{}", "é".repeat(2_100));
    assert!(oversized_trigger_detail.len() > 4_096);
    let connection = Connection::open(fixture.data_dir.join("context-projection.db"))
        .expect("open oversized projection trigger connection");
    connection
        .execute_batch(&format!(
            "CREATE TRIGGER fail_oversized_projection BEFORE INSERT ON projected_nodes
             WHEN NEW.node_id = 'commit-then-recover'
             BEGIN SELECT RAISE(ABORT, '{oversized_trigger_detail}'); END;"
        ))
        .expect("install oversized projection failure trigger");
    drop(connection);

    let overflow_error = fixture
        .kernel
        .admit_context_node(TrustedContextNodeDraft::task(
            SESSION_A,
            TASK_A,
            task_user_node(
                "commit-then-recover",
                &source.event_id,
                "canonical committed payload",
            ),
        ))
        .expect_err("oversized projection trigger must fail after append");
    let committed = assert_committed_projection_outcome(
        &overflow_error,
        "projection error detail exceeds 4096 bytes",
        short_raw_path,
    );
    assert_eq!(
        fixture
            .kernel
            .event_count()
            .expect("post-overflow-failure count"),
        before + 2
    );
    observe_exactly_once(&mut receiver, &committed);

    let connection = Connection::open(fixture.data_dir.join("context-projection.db"))
        .expect("open oversized trigger cleanup connection");
    connection
        .execute_batch("DROP TRIGGER fail_oversized_projection;")
        .expect("remove oversized projection failure trigger");
    drop(connection);

    let mut invalid_retry = task_user_node(
        "commit-then-recover",
        "not-the-committed-source",
        "a deliberately different retry payload",
    );
    invalid_retry.summary.clear();
    invalid_retry.confidence = f32::NAN;
    let retry = reject_without_append(
        &fixture,
        &mut receiver,
        TrustedContextNodeDraft::task(SESSION_A, TASK_B, invalid_retry),
    );
    assert!(matches!(
        retry,
        KernelError::DuplicateContextNodeIdentity {
            ref event_id,
            event_seq,
            ..
        } if event_id == &committed.event_id && event_seq == committed.seq
    ));

    let snapshot = fixture
        .projection
        .capture_snapshot(SESSION_A, Some(TASK_A))
        .expect("recovered projection snapshot");
    let recovered = snapshot
        .candidates()
        .iter()
        .find(|candidate| candidate.id == "commit-then-recover")
        .expect("committed node projected during retry catch-up");
    assert_eq!(recovered.summary, "canonical committed payload");
    assert!(snapshot.candidates().iter().any(|candidate| {
        candidate.id == "short-path-redaction"
            && candidate.summary == "short raw path stays private"
    }));
    assert_eq!(
        fixture
            .projection
            .checkpoint()
            .expect("recovered checkpoint")
            .through_seq,
        committed.seq
    );

    drop(receiver);
    let (_root, config) = fixture.close();
    let reopened = DittoKernel::open(config).expect("reopen recovered kernel");
    let persisted = reopened
        .list_events(&EventQuery {
            session_id: Some(SESSION_A.to_owned()),
            ..EventQuery::default()
        })
        .expect("list recovered events")
        .into_iter()
        .filter(|event| {
            event.event_id == committed.event_id || event.event_id == short_committed.event_id
        })
        .collect::<Vec<_>>();
    assert_eq!(persisted.len(), 2);
    let reopened_short = persisted
        .iter()
        .find(|event| event.event_id == short_committed.event_id)
        .expect("reopened short-path event");
    let reopened_overflow = persisted
        .iter()
        .find(|event| event.event_id == committed.event_id)
        .expect("reopened overflow event");
    assert_same_event(reopened_short, &short_committed);
    assert_same_event(reopened_overflow, &committed);
}
