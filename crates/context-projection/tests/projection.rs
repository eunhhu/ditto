use std::{
    collections::VecDeque,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use chrono::{Duration, TimeZone, Utc};
use ditto_context::{
    ContextCompiler, ContextLens, ContextNode, ContextNodeKind, ContextOrigin, ContextQueryRanking,
    ContextQueryRankingError, ContextScope, EpistemicStatus, TaskQuery, TaskSignatureV2,
};
use ditto_context_projection::{
    CONTEXT_NODE_EVENT_VERSION, CONTEXT_PROJECTION_DATABASE_FILENAME, ContextNodeDraft,
    ContextNodeRecordedPayloadV1, ContextProjection, ContextProjectionError,
    ContextProjectionSnapshot, MAX_CONTEXT_NODE_ID_BYTES, MAX_CONTEXT_SOURCE_EVENT_IDS,
    MAX_CONTEXT_SUMMARY_BYTES, MAX_CONTEXT_SUPERSEDES, MAX_SERIALIZED_CONTEXT_NODE_BYTES,
    MAX_SERIALIZED_CONTEXT_PAYLOAD_BYTES,
};
use ditto_event_store::EventStore;
use ditto_protocol::{EventActor, EventRecord, NewEvent, event_kind};
use ditto_retrieval::{
    CandidateCount, ContextResultLimit, Embedding, EmbeddingProvider, EmbeddingProviderError,
    EmbeddingPurpose, MAX_CANDIDATE_COUNT, MAX_CONTEXT_RESULT_LIMIT, RetrievalError, RetrievalMode,
};
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use tempfile::TempDir;

const SESSION: &str = "session-a";
const TASK_A: &str = "task-a";
const TASK_B: &str = "task-b";

struct Fixture {
    _dir: TempDir,
    store: EventStore,
    projection: ContextProjection,
    projection_path: PathBuf,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().expect("fixture directory");
    let store_path = dir.path().join("state.db");
    let projection_path = dir.path().join("context-projection.db");
    Fixture {
        store: EventStore::open(store_path).expect("event store"),
        projection: ContextProjection::open(&projection_path).expect("projection"),
        projection_path,
        _dir: dir,
    }
}

fn append_source_event(
    store: &EventStore,
    session_id: &str,
    task_id: Option<&str>,
    actor: EventActor,
    kind: &str,
) -> EventRecord {
    store
        .append(NewEvent {
            session_id: Some(session_id.to_owned()),
            task_id: task_id.map(str::to_owned),
            actor,
            kind: kind.to_owned(),
            payload: json!({"fixture_source": kind}),
            causation_id: None,
            correlation_id: Some(task_id.unwrap_or(session_id).to_owned()),
            span_id: None,
        })
        .expect("append source event")
}

fn noise(store: &EventStore, session_id: Option<&str>, task_id: Option<&str>) -> EventRecord {
    store
        .append(NewEvent {
            session_id: session_id.map(str::to_owned),
            task_id: task_id.map(str::to_owned),
            actor: EventActor::System,
            kind: "fixture.noise".into(),
            payload: json!({"ignored": true}),
            causation_id: None,
            correlation_id: session_id.map(|id| task_id.unwrap_or(id).to_owned()),
            span_id: None,
        })
        .expect("append noise event")
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

fn payload(node: &ContextNode) -> Value {
    serde_json::to_value(ContextNodeRecordedPayloadV1::new(node.clone()))
        .expect("serialize context payload")
}

fn greatest_source_id(store: &EventStore, node: &ContextNode) -> Option<String> {
    node.source_event_ids
        .iter()
        .filter_map(|id| store.get_by_event_id(id).expect("source lookup"))
        .max_by_key(|event| event.seq)
        .map(|event| event.event_id)
}

#[allow(clippy::too_many_arguments)]
fn context_event(
    store: &EventStore,
    session_id: Option<&str>,
    task_id: Option<&str>,
    actor: EventActor,
    payload: Value,
    causation_id: Option<String>,
    correlation_id: Option<String>,
    span_id: Option<String>,
) -> EventRecord {
    store
        .append(NewEvent {
            session_id: session_id.map(str::to_owned),
            task_id: task_id.map(str::to_owned),
            actor,
            kind: event_kind::CONTEXT_NODE_RECORDED.to_owned(),
            payload,
            causation_id,
            correlation_id,
            span_id,
        })
        .expect("append context event")
}

fn record_node(
    store: &EventStore,
    session_id: &str,
    task_id: Option<&str>,
    value: ContextNode,
) -> EventRecord {
    let causation_id = greatest_source_id(store, &value);
    context_event(
        store,
        Some(session_id),
        task_id,
        EventActor::System,
        payload(&value),
        causation_id,
        Some(task_id.unwrap_or(session_id).to_owned()),
        None,
    )
}

fn snapshot_ids(snapshot: &ContextProjectionSnapshot) -> Vec<String> {
    snapshot
        .candidates()
        .iter()
        .map(|candidate| candidate.id.clone())
        .collect()
}

fn lookup_committed_identity(
    fixture: &Fixture,
    session_id: &str,
    node_id: &str,
) -> Result<Option<ditto_context_projection::CommittedContextIdentity>, ContextProjectionError> {
    let captured_high_water = fixture
        .store
        .latest_seq()
        .expect("captured identity high water");
    lookup_committed_identity_at(
        &fixture.projection,
        &fixture.store,
        captured_high_water,
        session_id,
        node_id,
    )
}

fn lookup_committed_identity_at(
    projection: &ContextProjection,
    store: &EventStore,
    captured_high_water: i64,
    session_id: &str,
    node_id: &str,
) -> Result<Option<ditto_context_projection::CommittedContextIdentity>, ContextProjectionError> {
    projection.lookup_committed_identity(store, captured_high_water, session_id, node_id)
}

fn validate_draft(
    fixture: &Fixture,
    draft: &ContextNodeDraft,
) -> Result<ditto_context_projection::ValidatedContextNodeDraft, ContextProjectionError> {
    let captured_high_water = fixture
        .store
        .latest_seq()
        .expect("captured draft high water");
    fixture
        .projection
        .validate_draft(&fixture.store, captured_high_water, draft)
}

fn validate_draft_at(
    projection: &ContextProjection,
    store: &EventStore,
    captured_high_water: i64,
    draft: &ContextNodeDraft,
) -> Result<ditto_context_projection::ValidatedContextNodeDraft, ContextProjectionError> {
    projection.validate_draft(store, captured_high_water, draft)
}

fn remove_sqlite_cache(path: &Path) {
    let mut wal = path.as_os_str().to_os_string();
    wal.push("-wal");
    let mut shm = path.as_os_str().to_os_string();
    shm.push("-shm");
    let _ = fs::remove_file(PathBuf::from(wal));
    let _ = fs::remove_file(PathBuf::from(shm));
    fs::remove_file(path).expect("remove projection cache");
}

fn rebuild_result<F>(
    build: F,
) -> Result<ditto_context_projection::ProjectionSync, ContextProjectionError>
where
    F: FnOnce(&EventStore),
{
    let fixture = fixture();
    build(&fixture.store);
    fixture.projection.rebuild(&fixture.store)
}

#[test]
fn projection_owns_a_separate_database_path_and_rejects_the_event_store() {
    let directory = tempfile::tempdir().expect("projection data directory");
    let missing_parent = directory.path().join("missing-parent");
    let case_variant_store_path = missing_parent.join("STATE.DB");
    assert!(matches!(
        ContextProjection::open(&case_variant_store_path),
        Err(ContextProjectionError::SourceDatabaseCollision)
    ));
    assert!(!missing_parent.exists());

    let store_path = directory.path().join("state.db");
    assert!(matches!(
        ContextProjection::open(&store_path),
        Err(ContextProjectionError::SourceDatabaseCollision)
    ));
    assert!(!store_path.exists());

    fs::File::create(&store_path).expect("empty reserved event-store path");
    assert!(matches!(
        ContextProjection::open(&store_path),
        Err(ContextProjectionError::SourceDatabaseCollision)
    ));
    assert_eq!(
        fs::metadata(&store_path)
            .expect("reserved path metadata")
            .len(),
        0
    );
    assert!(!directory.path().join("state.db-wal").exists());
    assert!(!directory.path().join("state.db-shm").exists());

    let store = EventStore::open(&store_path).expect("event store");
    let projection = ContextProjection::open_in(directory.path()).expect("projection cache");
    let expected_path = directory.path().join(CONTEXT_PROJECTION_DATABASE_FILENAME);
    assert_eq!(projection.database_path(), expected_path.as_path());
    assert!(matches!(
        ContextProjection::open(&store_path),
        Err(ContextProjectionError::SourceDatabaseCollision)
    ));
    drop(store);
}

#[test]
fn incremental_reopen_deleted_cache_and_full_rebuild_are_equivalent() {
    let fixture = fixture();
    let source = append_source_event(
        &fixture.store,
        SESSION,
        None,
        EventActor::User,
        "input.received",
    );
    record_node(
        &fixture.store,
        SESSION,
        None,
        node(
            "alpha",
            ContextScope::Session,
            ContextOrigin::User,
            EpistemicStatus::Verified,
            vec![source.event_id.clone()],
            Vec::new(),
            "alpha deploy",
        ),
    );
    noise(&fixture.store, Some(SESSION), None);
    let first_high_water = fixture.store.latest_seq().expect("high water");
    fixture
        .projection
        .synchronize_through(&fixture.store, first_high_water)
        .expect("initial synchronization");

    let beta = record_node(
        &fixture.store,
        SESSION,
        None,
        node(
            "beta",
            ContextScope::Session,
            ContextOrigin::User,
            EpistemicStatus::Verified,
            vec![source.event_id.clone()],
            Vec::new(),
            "beta inspect",
        ),
    );
    let incremental_sync = fixture
        .projection
        .synchronize(&fixture.store)
        .expect("incremental synchronization");
    assert_eq!(incremental_sync.checkpoint.through_seq, beta.seq);

    let incremental = fixture
        .projection
        .capture_snapshot(SESSION, None)
        .expect("incremental snapshot");
    assert_eq!(snapshot_ids(&incremental), vec!["alpha", "beta"]);
    let event_count = fixture.store.count().expect("event count");
    let path = fixture.projection.database_path().to_path_buf();
    drop(fixture.projection);

    let reopened = ContextProjection::open(&path).expect("reopen projection");
    let reopened_result = reopened
        .synchronize_and_snapshot(&fixture.store, SESSION, None)
        .expect("reopened snapshot");
    assert_eq!(
        reopened_result.checkpoint(),
        &reopened.checkpoint().expect("checkpoint")
    );
    assert_eq!(snapshot_ids(&incremental), snapshot_ids(&reopened_result));
    assert_eq!(incremental.scanned_rows(), reopened_result.scanned_rows());
    drop(reopened);

    remove_sqlite_cache(&path);
    let deleted = ContextProjection::open(&path).expect("open deleted cache");
    let deleted_result = deleted
        .synchronize_and_snapshot(&fixture.store, SESSION, None)
        .expect("deleted-cache snapshot");
    assert_eq!(deleted_result.checkpoint().through_seq, beta.seq);
    let full_sync = deleted.rebuild(&fixture.store).expect("full rebuild");
    assert!(full_sync.rebuilt);
    let full_result = deleted
        .capture_snapshot(SESSION, None)
        .expect("full-rebuild snapshot");

    assert_eq!(deleted_result.checkpoint(), full_result.checkpoint());
    assert_eq!(snapshot_ids(&incremental), snapshot_ids(&deleted_result));
    assert_eq!(snapshot_ids(&deleted_result), snapshot_ids(&full_result));
    assert_eq!(incremental.scanned_rows(), full_result.scanned_rows());
    assert_eq!(fixture.store.count().expect("source count"), event_count);
}

#[test]
fn canonical_cache_corruption_is_rebuilt_before_live_draft_admission() {
    {
        let fixture = fixture();
        let source = append_source_event(
            &fixture.store,
            SESSION,
            None,
            EventActor::User,
            "input.received",
        );
        let committed = record_node(
            &fixture.store,
            SESSION,
            None,
            node(
                "deleted-relevant",
                ContextScope::Session,
                ContextOrigin::User,
                EpistemicStatus::Verified,
                vec![source.event_id.clone()],
                Vec::new(),
                "canonical deleted row",
            ),
        );
        fixture
            .projection
            .synchronize(&fixture.store)
            .expect("initial deleted-row sync");
        let before = fixture.projection.checkpoint().expect("checkpoint");
        let source_count = fixture.store.count().expect("source count");
        let connection = Connection::open(&fixture.projection_path).expect("cache connection");
        assert_eq!(
            connection
                .execute(
                    "DELETE FROM projected_nodes WHERE session_id = ?1 AND node_id = ?2",
                    params![SESSION, "deleted-relevant"],
                )
                .expect("delete projected row"),
            1
        );
        drop(connection);

        let draft = node(
            "deleted-relevant",
            ContextScope::Session,
            ContextOrigin::User,
            EpistemicStatus::Verified,
            vec![source.event_id.clone()],
            Vec::new(),
            "retry with a different payload",
        );
        let duplicate =
            validate_draft(&fixture, &ContextNodeDraft::session(SESSION, draft.clone()))
                .expect_err("deleted row must rebuild before duplicate admission");
        assert!(matches!(
            duplicate,
            ContextProjectionError::DuplicateNodeIdentity {
                session_id,
                node_id,
                event_id,
                seq,
            } if session_id == SESSION
                && node_id == "deleted-relevant"
                && event_id.as_str() == committed.event_id.as_str()
                && seq == committed.seq
        ));
        assert_eq!(
            fixture
                .projection
                .checkpoint()
                .expect("repaired checkpoint"),
            before
        );
        let synchronized = fixture
            .projection
            .synchronize(&fixture.store)
            .expect("verify deleted-row repair");
        assert!(!synchronized.rebuilt);
        let restored = fixture
            .projection
            .capture_snapshot(SESSION, None)
            .expect("restored deleted cache row");
        assert_eq!(snapshot_ids(&restored), vec!["deleted-relevant"]);
        let identity = lookup_committed_identity(&fixture, SESSION, "deleted-relevant")
            .expect("restored deleted identity")
            .expect("canonical identity");
        assert_eq!(identity.event_id, committed.event_id);
        assert_eq!(identity.event_seq, committed.seq);
        assert_eq!(
            fixture.store.count().expect("source count unchanged"),
            source_count
        );
        assert!(matches!(
            validate_draft(&fixture, &ContextNodeDraft::session(SESSION, draft)),
            Err(ContextProjectionError::DuplicateNodeIdentity {
                session_id,
                node_id,
                event_id,
                seq,
            }) if session_id == SESSION
                && node_id == "deleted-relevant"
                && event_id.as_str() == committed.event_id.as_str()
                && seq == committed.seq
        ));
    }

    {
        let fixture = fixture();
        let source = append_source_event(
            &fixture.store,
            SESSION,
            None,
            EventActor::User,
            "input.received",
        );
        let canonical = node(
            "altered-relevant",
            ContextScope::Session,
            ContextOrigin::User,
            EpistemicStatus::Verified,
            vec![source.event_id.clone()],
            Vec::new(),
            "canonical summary",
        );
        let committed = record_node(&fixture.store, SESSION, None, canonical.clone());
        fixture
            .projection
            .synchronize(&fixture.store)
            .expect("initial altered-row sync");
        let before = fixture.projection.checkpoint().expect("checkpoint");
        let source_count = fixture.store.count().expect("source count");
        let mut altered = canonical;
        altered.summary = "cache-only altered summary".into();
        let connection = Connection::open(&fixture.projection_path).expect("cache connection");
        assert_eq!(
            connection
                .execute(
                    "UPDATE projected_nodes SET node_json = ?1 WHERE session_id = ?2 AND node_id = ?3",
                    params![serde_json::to_string(&altered).expect("altered row JSON"), SESSION, "altered-relevant"],
                )
                .expect("alter projected row"),
            1
        );
        drop(connection);

        let draft = node(
            "altered-relevant",
            ContextScope::Session,
            ContextOrigin::User,
            EpistemicStatus::Verified,
            vec![source.event_id.clone()],
            Vec::new(),
            "retry with a different payload",
        );
        let duplicate = validate_draft(&fixture, &ContextNodeDraft::session(SESSION, draft))
            .expect_err("altered row must rebuild before duplicate admission");
        assert!(matches!(
            duplicate,
            ContextProjectionError::DuplicateNodeIdentity {
                session_id,
                node_id,
                event_id,
                seq,
            } if session_id == SESSION
                && node_id == "altered-relevant"
                && event_id.as_str() == committed.event_id.as_str()
                && seq == committed.seq
        ));
        assert_eq!(
            fixture
                .projection
                .checkpoint()
                .expect("repaired checkpoint"),
            before
        );
        let synchronized = fixture
            .projection
            .synchronize(&fixture.store)
            .expect("verify altered-row repair");
        assert!(!synchronized.rebuilt);
        let restored = fixture
            .projection
            .capture_snapshot(SESSION, None)
            .expect("snapshot restored canonical row");
        assert_eq!(snapshot_ids(&restored), vec!["altered-relevant"]);
        assert_eq!(restored.candidates()[0].summary, "canonical summary");
        let identity = lookup_committed_identity(&fixture, SESSION, "altered-relevant")
            .expect("restored altered identity")
            .expect("canonical identity");
        assert_eq!(identity.event_id, committed.event_id);
        assert_eq!(identity.event_seq, committed.seq);
        assert_eq!(
            fixture.store.count().expect("source count unchanged"),
            source_count
        );
    }

    {
        let fixture = fixture();
        let source = append_source_event(
            &fixture.store,
            SESSION,
            None,
            EventActor::User,
            "input.received",
        );
        let existing = node(
            "existing-target",
            ContextScope::Session,
            ContextOrigin::User,
            EpistemicStatus::Verified,
            vec![source.event_id.clone()],
            Vec::new(),
            "existing target",
        );
        let committed = record_node(&fixture.store, SESSION, None, existing);
        fixture
            .projection
            .synchronize(&fixture.store)
            .expect("initial cache-only-target sync");
        let before = fixture.projection.checkpoint().expect("checkpoint");
        let source_count = fixture.store.count().expect("source count");
        let fake_target = node(
            "cache-only-target",
            ContextScope::Session,
            ContextOrigin::User,
            EpistemicStatus::Verified,
            vec![source.event_id.clone()],
            Vec::new(),
            "cache-only target",
        );
        let connection = Connection::open(&fixture.projection_path).expect("cache connection");
        assert_eq!(
            connection
                .execute(
                    "INSERT INTO projected_nodes (session_id, task_id, node_id, event_seq, event_id, node_json) VALUES (?1, NULL, ?2, ?3, ?4, ?5)",
                    params![
                        SESSION,
                        fake_target.id,
                        committed.seq + 10_000,
                        "cache-only-event",
                        serde_json::to_string(&fake_target).expect("fake target JSON"),
                    ],
                )
                .expect("insert cache-only target"),
            1
        );
        drop(connection);

        let draft = node(
            "new-superseding-cache-only",
            ContextScope::Session,
            ContextOrigin::User,
            EpistemicStatus::Verified,
            vec![source.event_id.clone()],
            vec!["cache-only-target".into()],
            "new superseding draft",
        );
        let error = validate_draft(&fixture, &ContextNodeDraft::session(SESSION, draft.clone()))
            .expect_err("cache-only target must rebuild before supersession admission");
        assert!(matches!(
            error,
            ContextProjectionError::MissingSupersededNode {
                node_id,
                superseded_id,
            } if node_id == "new-superseding-cache-only" && superseded_id == "cache-only-target"
        ));
        assert_eq!(
            fixture
                .projection
                .checkpoint()
                .expect("repaired checkpoint"),
            before
        );
        let synchronized = fixture
            .projection
            .synchronize(&fixture.store)
            .expect("verify cache-only target repair");
        assert!(!synchronized.rebuilt);
        let repaired = fixture
            .projection
            .capture_snapshot(SESSION, None)
            .expect("snapshot without cache-only target");
        assert_eq!(snapshot_ids(&repaired), vec!["existing-target"]);
        assert!(
            lookup_committed_identity(&fixture, SESSION, "cache-only-target")
                .expect("cache-only identity lookup")
                .is_none()
        );
        assert_eq!(
            fixture.store.count().expect("source count unchanged"),
            source_count
        );
    }

    {
        let fixture = fixture();
        let source = append_source_event(
            &fixture.store,
            SESSION,
            None,
            EventActor::User,
            "input.received",
        );
        let target = record_node(
            &fixture.store,
            SESSION,
            None,
            node(
                "edge-target",
                ContextScope::Session,
                ContextOrigin::User,
                EpistemicStatus::Verified,
                vec![source.event_id.clone()],
                Vec::new(),
                "canonical edge target",
            ),
        );
        let replacement = record_node(
            &fixture.store,
            SESSION,
            None,
            node(
                "edge-replacement",
                ContextScope::Session,
                ContextOrigin::User,
                EpistemicStatus::Verified,
                vec![source.event_id.clone()],
                vec!["edge-target".into()],
                "canonical edge replacement",
            ),
        );
        fixture
            .projection
            .synchronize(&fixture.store)
            .expect("initial edge sync");
        let before = fixture.projection.checkpoint().expect("edge checkpoint");
        let source_count = fixture.store.count().expect("edge source count");

        let connection = Connection::open(&fixture.projection_path).expect("edge cache connection");
        assert_eq!(
            connection
                .execute(
                    "INSERT INTO supersession_edges (session_id, task_key, superseding_node_id, superseded_node_id, event_seq) VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![
                        SESSION,
                        "",
                        "ghost-superseder",
                        "edge-target",
                        replacement.seq,
                    ],
                )
                .expect("inject cache-only edge"),
            1
        );
        drop(connection);

        let duplicate = validate_draft(
            &fixture,
            &ContextNodeDraft::session(
                SESSION,
                node(
                    "edge-target",
                    ContextScope::Session,
                    ContextOrigin::User,
                    EpistemicStatus::Verified,
                    vec![source.event_id],
                    Vec::new(),
                    "retry edge target",
                ),
            ),
        )
        .expect_err("cache-only incoming edge must rebuild before admission");
        assert!(matches!(
            duplicate,
            ContextProjectionError::DuplicateNodeIdentity {
                session_id,
                node_id,
                event_id,
                seq,
            } if session_id == SESSION
                && node_id == "edge-target"
                && event_id.as_str() == target.event_id.as_str()
                && seq == target.seq
        ));
        assert_eq!(
            fixture
                .projection
                .checkpoint()
                .expect("repaired edge checkpoint"),
            before
        );
        let repaired = fixture
            .projection
            .capture_snapshot(SESSION, None)
            .expect("snapshot after edge repair");
        assert_eq!(snapshot_ids(&repaired), vec!["edge-replacement"]);
        let connection = Connection::open(&fixture.projection_path).expect("read repaired edges");
        let ghost_edges: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM supersession_edges WHERE session_id = ?1 AND superseding_node_id = ?2",
                params![SESSION, "ghost-superseder"],
                |row| row.get(0),
            )
            .expect("count repaired ghost edges");
        assert_eq!(ghost_edges, 0);
        let real_edges: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM supersession_edges WHERE session_id = ?1 AND superseding_node_id = ?2 AND superseded_node_id = ?3",
                params![SESSION, "edge-replacement", "edge-target"],
                |row| row.get(0),
            )
            .expect("count retained real edge");
        assert_eq!(real_edges, 1);
        drop(connection);
        let synchronized = fixture
            .projection
            .synchronize(&fixture.store)
            .expect("verify edge repair");
        assert!(!synchronized.rebuilt);
        assert_eq!(
            fixture.store.count().expect("edge source count unchanged"),
            source_count
        );
    }

    {
        let fixture = fixture();
        let source = append_source_event(
            &fixture.store,
            SESSION,
            None,
            EventActor::User,
            "input.received",
        );
        let canonical = record_node(
            &fixture.store,
            SESSION,
            None,
            node(
                "outgoing-source",
                ContextScope::Session,
                ContextOrigin::User,
                EpistemicStatus::Verified,
                vec![source.event_id.clone()],
                Vec::new(),
                "canonical outgoing source",
            ),
        );
        fixture
            .projection
            .synchronize(&fixture.store)
            .expect("initial outgoing-edge sync");
        let before = fixture
            .projection
            .checkpoint()
            .expect("outgoing-edge checkpoint");
        let source_count = fixture.store.count().expect("outgoing source count");

        let connection =
            Connection::open(&fixture.projection_path).expect("outgoing edge cache connection");
        assert_eq!(
            connection
                .execute(
                    "INSERT INTO supersession_edges (session_id, task_key, superseding_node_id, superseded_node_id, event_seq) VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![
                        SESSION,
                        "",
                        "outgoing-source",
                        "ghost-target",
                        canonical.seq,
                    ],
                )
                .expect("inject cache-only outgoing edge"),
            1
        );
        drop(connection);

        let duplicate = validate_draft(
            &fixture,
            &ContextNodeDraft::session(
                SESSION,
                node(
                    "outgoing-source",
                    ContextScope::Session,
                    ContextOrigin::User,
                    EpistemicStatus::Verified,
                    vec![source.event_id],
                    Vec::new(),
                    "retry outgoing source",
                ),
            ),
        )
        .expect_err("cache-only outgoing edge must rebuild before admission");
        assert!(matches!(
            duplicate,
            ContextProjectionError::DuplicateNodeIdentity {
                session_id,
                node_id,
                event_id,
                seq,
            } if session_id == SESSION
                && node_id == "outgoing-source"
                && event_id.as_str() == canonical.event_id.as_str()
                && seq == canonical.seq
        ));
        assert_eq!(
            fixture
                .projection
                .checkpoint()
                .expect("repaired outgoing-edge checkpoint"),
            before
        );
        let repaired = fixture
            .projection
            .capture_snapshot(SESSION, None)
            .expect("snapshot after outgoing-edge repair");
        assert_eq!(snapshot_ids(&repaired), vec!["outgoing-source"]);
        let connection =
            Connection::open(&fixture.projection_path).expect("read repaired outgoing edges");
        let ghost_edges: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM supersession_edges WHERE session_id = ?1 AND superseding_node_id = ?2",
                params![SESSION, "outgoing-source"],
                |row| row.get(0),
            )
            .expect("count repaired outgoing edges");
        assert_eq!(ghost_edges, 0);
        drop(connection);
        let synchronized = fixture
            .projection
            .synchronize(&fixture.store)
            .expect("verify outgoing-edge repair");
        assert!(!synchronized.rebuilt);
        assert_eq!(
            fixture
                .store
                .count()
                .expect("outgoing source count unchanged"),
            source_count
        );
    }
}

#[test]
fn node_id_is_session_wide_while_supersession_remains_exact_scope() {
    let first = fixture();
    let source_a = append_source_event(
        &first.store,
        SESSION,
        None,
        EventActor::User,
        "input.received",
    );
    let session_node = record_node(
        &first.store,
        SESSION,
        None,
        node(
            "shared",
            ContextScope::Session,
            ContextOrigin::User,
            EpistemicStatus::Verified,
            vec![source_a.event_id.clone()],
            Vec::new(),
            "session value",
        ),
    );
    first
        .projection
        .synchronize_through(&first.store, session_node.seq)
        .expect("session node");
    let task_source = append_source_event(
        &first.store,
        SESSION,
        Some(TASK_A),
        EventActor::User,
        "input.received",
    );
    let duplicate_task = record_node(
        &first.store,
        SESSION,
        Some(TASK_A),
        node(
            "shared",
            ContextScope::Task,
            ContextOrigin::User,
            EpistemicStatus::Verified,
            vec![task_source.event_id.clone()],
            Vec::new(),
            "task duplicate",
        ),
    );
    let count_before = first.store.count().expect("count before duplicate sync");
    assert!(matches!(
        first.projection.synchronize(&first.store),
        Err(ContextProjectionError::DuplicateNodeIdentity {
            session_id,
            node_id,
            ..
        }) if session_id == SESSION && node_id == "shared"
    ));
    assert_eq!(
        first.store.count().expect("count after duplicate"),
        count_before
    );
    assert_eq!(
        first
            .projection
            .checkpoint()
            .expect("checkpoint after duplicate")
            .through_seq,
        session_node.seq
    );
    assert!(
        lookup_committed_identity_at(
            &first.projection,
            &first.store,
            session_node.seq,
            SESSION,
            "shared",
        )
        .expect("identity through valid prefix")
        .is_some()
    );
    assert_eq!(
        duplicate_task.seq,
        first.store.latest_seq().expect("latest")
    );

    {
        let fixture = fixture();
        let source = append_source_event(
            &fixture.store,
            SESSION,
            None,
            EventActor::User,
            "input.received",
        );
        let first_task = record_node(
            &fixture.store,
            SESSION,
            Some(TASK_A),
            node(
                "task-shared",
                ContextScope::Task,
                ContextOrigin::User,
                EpistemicStatus::Verified,
                vec![source.event_id.clone()],
                Vec::new(),
                "first task",
            ),
        );
        fixture
            .projection
            .synchronize_through(&fixture.store, first_task.seq)
            .expect("first task");
        let second_source = append_source_event(
            &fixture.store,
            SESSION,
            Some(TASK_B),
            EventActor::User,
            "input.received",
        );
        record_node(
            &fixture.store,
            SESSION,
            Some(TASK_B),
            node(
                "task-shared",
                ContextScope::Task,
                ContextOrigin::User,
                EpistemicStatus::Verified,
                vec![second_source.event_id],
                Vec::new(),
                "second task duplicate",
            ),
        );
        assert!(matches!(
            fixture.projection.synchronize(&fixture.store),
            Err(ContextProjectionError::DuplicateNodeIdentity { .. })
        ));
    }

    {
        let fixture = fixture();
        let source_one = append_source_event(
            &fixture.store,
            "other-session",
            None,
            EventActor::User,
            "input.received",
        );
        record_node(
            &fixture.store,
            "other-session",
            None,
            node(
                "shared",
                ContextScope::Session,
                ContextOrigin::User,
                EpistemicStatus::Verified,
                vec![source_one.event_id],
                Vec::new(),
                "other session",
            ),
        );
        fixture
            .projection
            .synchronize(&fixture.store)
            .expect("same ID in another session");
        assert!(
            lookup_committed_identity(&fixture, "other-session", "shared")
                .expect("other identity")
                .is_some()
        );
    }

    {
        let fixture = fixture();
        let source = append_source_event(
            &fixture.store,
            SESSION,
            Some(TASK_A),
            EventActor::User,
            "input.received",
        );
        let old = record_node(
            &fixture.store,
            SESSION,
            Some(TASK_A),
            node(
                "old",
                ContextScope::Task,
                ContextOrigin::User,
                EpistemicStatus::Verified,
                vec![source.event_id.clone()],
                Vec::new(),
                "old needle",
            ),
        );
        fixture
            .projection
            .synchronize_through(&fixture.store, old.seq)
            .expect("old task node");
        let replacement = record_node(
            &fixture.store,
            SESSION,
            Some(TASK_A),
            node(
                "replacement",
                ContextScope::Task,
                ContextOrigin::User,
                EpistemicStatus::Verified,
                vec![source.event_id.clone()],
                vec!["old".into()],
                "replacement needle",
            ),
        );
        fixture
            .projection
            .synchronize(&fixture.store)
            .expect("same-scope supersession");
        assert!(
            lookup_committed_identity(&fixture, SESSION, "old")
                .expect("old identity")
                .is_some()
        );
        let replacement_snapshot = fixture
            .projection
            .capture_snapshot(SESSION, Some(TASK_A))
            .expect("replacement snapshot");
        assert_eq!(snapshot_ids(&replacement_snapshot), vec!["replacement"]);
        assert_eq!(
            fixture
                .projection
                .checkpoint()
                .expect("checkpoint")
                .through_seq,
            replacement.seq
        );
    }

    let fixture = fixture();
    let source = append_source_event(
        &fixture.store,
        SESSION,
        Some(TASK_A),
        EventActor::User,
        "input.received",
    );
    let old = record_node(
        &fixture.store,
        SESSION,
        Some(TASK_A),
        node(
            "cross-scope-old",
            ContextScope::Task,
            ContextOrigin::User,
            EpistemicStatus::Verified,
            vec![source.event_id.clone()],
            Vec::new(),
            "old",
        ),
    );
    fixture
        .projection
        .synchronize_through(&fixture.store, old.seq)
        .expect("old exact-scope target");
    let cross_scope_source = append_source_event(
        &fixture.store,
        SESSION,
        None,
        EventActor::User,
        "input.cross-scope",
    );
    record_node(
        &fixture.store,
        SESSION,
        Some(TASK_B),
        node(
            "cross-scope-new",
            ContextScope::Task,
            ContextOrigin::User,
            EpistemicStatus::Verified,
            vec![cross_scope_source.event_id],
            vec!["cross-scope-old".into()],
            "new",
        ),
    );
    assert!(matches!(
        fixture.projection.synchronize(&fixture.store),
        Err(ContextProjectionError::SupersessionScopeMismatch { .. })
    ));
    assert_eq!(
        fixture
            .projection
            .checkpoint()
            .expect("checkpoint")
            .through_seq,
        old.seq
    );
}

#[test]
fn context_document_and_prefilter_v2_limits_are_exact_and_never_clamped() {
    let document_node = node(
        "id=raw",
        ContextScope::Session,
        ContextOrigin::User,
        EpistemicStatus::Verified,
        vec!["source".into()],
        Vec::new(),
        "summary\nwith=equals",
    );
    let document = document_node
        .retrieval_document()
        .expect("context retrieval document");
    assert_eq!(
        document.as_str(),
        "id=id=raw\nkind=claim\nsummary=summary\nwith=equals"
    );
    assert_eq!(
        document.len(),
        18 + document_node.id.len()
            + document_node.kind.as_str().len()
            + document_node.summary.len()
    );

    let mut maximum_node = node(
        "i".repeat(MAX_CONTEXT_NODE_ID_BYTES),
        ContextScope::Session,
        ContextOrigin::User,
        EpistemicStatus::Verified,
        vec!["source".into()],
        Vec::new(),
        "s".repeat(MAX_CONTEXT_SUMMARY_BYTES),
    );
    maximum_node.kind = ContextNodeKind::OpenQuestion;
    let maximum_document = maximum_node
        .retrieval_document()
        .expect("maximum context document");
    assert_eq!(maximum_document.len(), 18 + 256 + 13 + 65_000);
    assert!(maximum_document.len() <= 65_536);

    assert!(ContextResultLimit::new(1).is_ok());
    assert_eq!(
        ContextResultLimit::new(MAX_CONTEXT_RESULT_LIMIT)
            .expect("maximum result limit")
            .get(),
        MAX_CONTEXT_RESULT_LIMIT
    );
    assert!(matches!(
        ContextResultLimit::new(0),
        Err(RetrievalError::ResultLimitOutOfRange { requested: 0, .. })
    ));
    assert!(matches!(
        ContextResultLimit::new(MAX_CONTEXT_RESULT_LIMIT + 1),
        Err(RetrievalError::ResultLimitOutOfRange { requested: 257, .. })
    ));

    let fixture = fixture();
    let source = append_source_event(
        &fixture.store,
        SESSION,
        None,
        EventActor::User,
        "input.received",
    );
    for index in 0..MAX_CANDIDATE_COUNT {
        record_node(
            &fixture.store,
            SESSION,
            None,
            node(
                format!("candidate-{index:05}"),
                ContextScope::Session,
                ContextOrigin::User,
                EpistemicStatus::Verified,
                vec![source.event_id.clone()],
                Vec::new(),
                format!("candidate {index}"),
            ),
        );
    }
    fixture
        .projection
        .synchronize(&fixture.store)
        .expect("exactly ten thousand candidates");
    let snapshot = fixture
        .projection
        .capture_snapshot(SESSION, None)
        .expect("ten-thousand snapshot");
    assert_eq!(snapshot.scanned_rows(), MAX_CANDIDATE_COUNT);
    assert_eq!(snapshot.candidates().len(), MAX_CANDIDATE_COUNT);
    assert_eq!(snapshot.candidates()[0].id, "candidate-00000");
    assert_eq!(
        snapshot.candidates()[MAX_CANDIDATE_COUNT - 1].id,
        "candidate-09999"
    );

    let prefilter_query =
        TaskQuery::new(TaskSignatureV2::new("not-present")).expect("prefilter query");
    let mut all_denied = snapshot.candidates().to_vec();
    for candidate in &mut all_denied {
        candidate.epistemic = EpistemicStatus::Disputed;
    }
    let denied_ranking = ContextQueryRanking::new(
        &prefilter_query,
        all_denied.clone(),
        Utc::now(),
        ContextResultLimit::new(1).expect("prefilter result limit"),
        None,
    )
    .expect("all denied candidates are scanned before filtering");
    assert!(denied_ranking.is_empty());
    all_denied.push(node(
        "candidate-10000",
        ContextScope::Session,
        ContextOrigin::User,
        EpistemicStatus::Disputed,
        vec!["source".into()],
        Vec::new(),
        "not present",
    ));
    assert!(matches!(
        ContextQueryRanking::new(
            &prefilter_query,
            all_denied,
            Utc::now(),
            ContextResultLimit::new(1).expect("overflow result limit"),
            None,
        ),
        Err(ContextQueryRankingError::Retrieval(
            RetrievalError::CandidateCountExceeded {
                actual: 10_001,
                maximum: 10_000,
            }
        ))
    ));

    let overflow = record_node(
        &fixture.store,
        SESSION,
        None,
        node(
            "candidate-10000",
            ContextScope::Session,
            ContextOrigin::User,
            EpistemicStatus::Verified,
            vec![source.event_id],
            Vec::new(),
            "overflow",
        ),
    );
    let count_before = fixture.store.count().expect("count before overflow scan");
    assert!(matches!(
        fixture.projection.synchronize(&fixture.store),
        Ok(sync) if sync.checkpoint.through_seq == overflow.seq
    ));
    let error = fixture
        .projection
        .capture_snapshot(SESSION, None)
        .expect_err("10,001st selected row must fail");
    assert!(matches!(
        error,
        ContextProjectionError::Retrieval(RetrievalError::CandidateCountExceeded {
            actual: 10_001,
            maximum: 10_000
        })
    ));
    assert_eq!(
        fixture.store.count().expect("count after overflow scan"),
        count_before
    );
    assert_eq!(
        CandidateCount::new(MAX_CANDIDATE_COUNT).expect("N").get(),
        MAX_CANDIDATE_COUNT
    );
    assert!(matches!(
        CandidateCount::new(MAX_CANDIDATE_COUNT + 1),
        Err(RetrievalError::CandidateCountExceeded { actual: 10_001, .. })
    ));
}

#[test]
fn multi_page_high_water_unknown_kind_and_source_count_are_isolated() {
    let fixture = fixture();
    let source = append_source_event(
        &fixture.store,
        SESSION,
        None,
        EventActor::User,
        "input.received",
    );
    let source_count_before_gap = fixture.store.count().expect("count before legal gap");
    let source_path = fixture._dir.path().join("state.db");
    let connection = Connection::open(&source_path).expect("open source sequence metadata");
    assert_eq!(
        connection
            .execute(
                "UPDATE sqlite_sequence SET seq = seq + 7 WHERE name = 'events'",
                [],
            )
            .expect("advance legal AUTOINCREMENT high water gap"),
        1
    );
    drop(connection);
    let first_node = record_node(
        &fixture.store,
        SESSION,
        None,
        node(
            "first",
            ContextScope::Session,
            ContextOrigin::User,
            EpistemicStatus::Verified,
            vec![source.event_id],
            Vec::new(),
            "first page needle",
        ),
    );
    assert_eq!(first_node.seq, source.seq + 8);
    assert_eq!(
        fixture.store.count().expect("count after legal gap"),
        source_count_before_gap + 1
    );
    for index in 0..2_003 {
        noise(&fixture.store, Some(SESSION), None);
        if index == 1_000 {
            assert!(
                fixture
                    .store
                    .get_by_seq(first_node.seq)
                    .expect("event lookup")
                    .is_some()
            );
        }
    }
    let captured_high_water = fixture.store.latest_seq().expect("captured high water");
    assert_eq!(captured_high_water, first_node.seq + 2_003);
    let source_count = fixture.store.count().expect("source count before sync");

    let late_event = noise(&fixture.store, Some(SESSION), None);
    let bounded = fixture
        .projection
        .synchronize_through(&fixture.store, captured_high_water)
        .expect("bounded multi-page synchronization");
    assert_eq!(bounded.captured_high_water, captured_high_water);
    assert_eq!(bounded.checkpoint.through_seq, captured_high_water);
    assert_eq!(
        bounded.checkpoint.through_event_id.as_deref(),
        fixture
            .store
            .get_by_seq(captured_high_water)
            .expect("anchor lookup")
            .as_ref()
            .map(|event| event.event_id.as_str())
    );
    let bounded_snapshot = fixture
        .projection
        .capture_snapshot(SESSION, None)
        .expect("snapshot after legal sequence gap");
    assert_eq!(snapshot_ids(&bounded_snapshot), vec!["first"]);
    assert_eq!(bounded_snapshot.scanned_rows(), 1);
    assert!(
        lookup_committed_identity_at(
            &fixture.projection,
            &fixture.store,
            captured_high_water,
            SESSION,
            "first",
        )
        .expect("first identity")
        .is_some()
    );
    assert!(
        lookup_committed_identity_at(
            &fixture.projection,
            &fixture.store,
            captured_high_water,
            SESSION,
            "late",
        )
        .expect("late identity")
        .is_none()
    );
    assert_eq!(
        fixture.store.count().expect("source count after sync"),
        source_count + 1
    );

    let caught_up = fixture
        .projection
        .synchronize(&fixture.store)
        .expect("catch up to late event");
    assert_eq!(caught_up.checkpoint.through_seq, late_event.seq);
    assert_eq!(
        caught_up.checkpoint.through_event_id.as_deref(),
        Some(late_event.event_id.as_str())
    );
    assert_eq!(
        fixture.store.count().expect("source count after catch-up"),
        source_count + 1
    );
}

#[test]
fn malformed_context_event_stops_before_checkpoint_and_foreign_anchor_rebuilds() {
    {
        let fixture = fixture();
        let source = append_source_event(
            &fixture.store,
            SESSION,
            None,
            EventActor::User,
            "input.received",
        );
        let valid = record_node(
            &fixture.store,
            SESSION,
            None,
            node(
                "valid",
                ContextScope::Session,
                ContextOrigin::User,
                EpistemicStatus::Verified,
                vec![source.event_id.clone()],
                Vec::new(),
                "valid",
            ),
        );
        fixture
            .projection
            .synchronize_through(&fixture.store, valid.seq)
            .expect("valid prefix");
        let before = fixture.projection.checkpoint().expect("prefix checkpoint");
        let malformed = context_event(
            &fixture.store,
            Some(SESSION),
            None,
            EventActor::System,
            json!({"event_version": CONTEXT_NODE_EVENT_VERSION}),
            None,
            Some(SESSION.into()),
            None,
        );
        let event_count = fixture.store.count().expect("count before malformed sync");
        assert!(matches!(
            fixture.projection.synchronize(&fixture.store),
            Err(ContextProjectionError::MalformedPayload { seq, .. }) if seq == malformed.seq
        ));
        assert_eq!(
            fixture
                .projection
                .checkpoint()
                .expect("checkpoint after malformed"),
            before
        );
        assert_eq!(
            fixture.store.count().expect("count after malformed"),
            event_count
        );
        assert!(
            lookup_committed_identity_at(
                &fixture.projection,
                &fixture.store,
                before.through_seq,
                SESSION,
                "valid",
            )
            .expect("valid identity after rollback")
            .is_some()
        );
    }

    let anchor_fixture = fixture();
    let anchor_source = append_source_event(
        &anchor_fixture.store,
        SESSION,
        None,
        EventActor::User,
        "input.received",
    );
    let anchor_node = record_node(
        &anchor_fixture.store,
        SESSION,
        None,
        node(
            "anchor-node",
            ContextScope::Session,
            ContextOrigin::User,
            EpistemicStatus::Verified,
            vec![anchor_source.event_id.clone()],
            Vec::new(),
            "anchor",
        ),
    );
    let foreign_event = noise(&anchor_fixture.store, Some(SESSION), None);
    anchor_fixture
        .projection
        .synchronize(&anchor_fixture.store)
        .expect("anchor synchronization");
    let path = anchor_fixture.projection_path.clone();
    drop(anchor_fixture.projection);
    let connection = Connection::open(&path).expect("open projection for anchor corruption");
    connection
        .execute(
            "UPDATE projection_checkpoint SET through_event_id = ?1 WHERE singleton = 1",
            [&anchor_source.event_id],
        )
        .expect("corrupt anchor");
    drop(connection);
    let reopened = ContextProjection::open(&path).expect("reopen foreign anchor");
    let rebuilt = reopened
        .synchronize(&anchor_fixture.store)
        .expect("foreign anchor rebuild");
    assert!(rebuilt.rebuilt);
    assert_eq!(rebuilt.checkpoint.through_seq, foreign_event.seq);
    assert_eq!(
        rebuilt.checkpoint.through_event_id.as_deref(),
        Some(foreign_event.event_id.as_str())
    );
    assert!(
        lookup_committed_identity_at(
            &reopened,
            &anchor_fixture.store,
            foreign_event.seq,
            SESSION,
            "anchor-node",
        )
        .expect("rebuilt identity")
        .is_some()
    );
    assert_eq!(anchor_node.seq + 1, foreign_event.seq);
}

#[test]
fn schema_reset_discards_only_derived_rows_and_replays_the_event_spine() {
    let fixture = fixture();
    let source = append_source_event(
        &fixture.store,
        SESSION,
        None,
        EventActor::User,
        "input.received",
    );
    record_node(
        &fixture.store,
        SESSION,
        None,
        node(
            "schema-node",
            ContextScope::Session,
            ContextOrigin::User,
            EpistemicStatus::Verified,
            vec![source.event_id],
            Vec::new(),
            "schema",
        ),
    );
    fixture
        .projection
        .synchronize(&fixture.store)
        .expect("initial schema sync");
    let event_count = fixture.store.count().expect("source count");
    let path = fixture.projection_path.clone();
    drop(fixture.projection);

    let connection = Connection::open(&path).expect("open schema cache");
    connection
        .execute_batch("PRAGMA user_version = 999;")
        .expect("mark unsupported projection schema");
    drop(connection);
    let reopened = ContextProjection::open(&path).expect("schema reset on reopen");
    assert_eq!(
        reopened.checkpoint().expect("zero checkpoint").through_seq,
        0
    );
    assert!(
        lookup_committed_identity_at(&reopened, &fixture.store, 0, SESSION, "schema-node",)
            .expect("empty derived cache")
            .is_none()
    );
    reopened
        .synchronize(&fixture.store)
        .expect("replay after schema reset");
    let replayed_snapshot = reopened
        .capture_snapshot(SESSION, None)
        .expect("replayed snapshot after schema reset");
    assert_eq!(snapshot_ids(&replayed_snapshot), vec!["schema-node"]);
    assert!(
        lookup_committed_identity_at(
            &reopened,
            &fixture.store,
            fixture.store.latest_seq().expect("schema high water"),
            SESSION,
            "schema-node",
        )
        .expect("replayed identity")
        .is_some()
    );
    assert_eq!(
        fixture.store.count().expect("source count unchanged"),
        event_count
    );
}

#[test]
fn snapshot_scope_excludes_only_superseded_rows_and_preserves_durable_order() {
    let fixture = fixture();
    let source = append_source_event(
        &fixture.store,
        SESSION,
        None,
        EventActor::User,
        "input.received",
    );
    let now = Utc
        .with_ymd_and_hms(2026, 8, 31, 0, 0, 0)
        .single()
        .expect("fixed test instant");

    record_node(
        &fixture.store,
        SESSION,
        None,
        node(
            "active",
            ContextScope::Session,
            ContextOrigin::User,
            EpistemicStatus::Verified,
            vec![source.event_id.clone()],
            Vec::new(),
            "needle active",
        ),
    );
    let mut future = node(
        "future",
        ContextScope::Session,
        ContextOrigin::User,
        EpistemicStatus::Verified,
        vec![source.event_id.clone()],
        Vec::new(),
        "needle future",
    );
    future.valid_from = Some(now + Duration::hours(1));
    record_node(&fixture.store, SESSION, None, future);

    let mut expired = node(
        "expired",
        ContextScope::Session,
        ContextOrigin::User,
        EpistemicStatus::Verified,
        vec![source.event_id.clone()],
        Vec::new(),
        "needle expired",
    );
    expired.valid_until = Some(now - Duration::hours(1));
    record_node(&fixture.store, SESSION, None, expired);

    record_node(
        &fixture.store,
        SESSION,
        None,
        node(
            "disputed",
            ContextScope::Session,
            ContextOrigin::User,
            EpistemicStatus::Disputed,
            vec![source.event_id.clone()],
            Vec::new(),
            "needle disputed",
        ),
    );
    let superseded = record_node(
        &fixture.store,
        SESSION,
        None,
        node(
            "superseded",
            ContextScope::Session,
            ContextOrigin::User,
            EpistemicStatus::Verified,
            vec![source.event_id.clone()],
            Vec::new(),
            "needle superseded",
        ),
    );
    let replacement = record_node(
        &fixture.store,
        SESSION,
        None,
        node(
            "active-replacement",
            ContextScope::Session,
            ContextOrigin::User,
            EpistemicStatus::Verified,
            vec![source.event_id.clone()],
            vec!["superseded".into()],
            "needle replacement",
        ),
    );
    record_node(
        &fixture.store,
        SESSION,
        None,
        node(
            "lexical-negative",
            ContextScope::Session,
            ContextOrigin::User,
            EpistemicStatus::Verified,
            vec![source.event_id],
            Vec::new(),
            "unrelated",
        ),
    );
    fixture
        .projection
        .synchronize(&fixture.store)
        .expect("filter fixture sync");
    let snapshot = fixture
        .projection
        .capture_snapshot(SESSION, None)
        .expect("filter snapshot");
    assert_eq!(snapshot.scanned_rows(), 7);
    assert_eq!(
        snapshot_ids(&snapshot),
        vec![
            "active",
            "future",
            "expired",
            "disputed",
            "active-replacement",
            "lexical-negative",
        ]
    );
    assert!(
        lookup_committed_identity(&fixture, SESSION, "superseded")
            .expect("superseded identity")
            .is_some()
    );
    assert!(replacement.seq < fixture.store.latest_seq().expect("latest"));
    assert!(superseded.seq < replacement.seq);
}

#[test]
fn exact_context_identity_precedes_lexical_ranking_and_never_uses_kind_or_summary() {
    let fixture = fixture();
    let source = append_source_event(
        &fixture.store,
        SESSION,
        None,
        EventActor::User,
        "input.received",
    );
    let exact = node(
        "node-42",
        ContextScope::Session,
        ContextOrigin::User,
        EpistemicStatus::Verified,
        vec![source.event_id.clone()],
        Vec::new(),
        "unrelated summary",
    );
    let lexical = node(
        "other",
        ContextScope::Session,
        ContextOrigin::User,
        EpistemicStatus::Verified,
        vec![source.event_id.clone()],
        Vec::new(),
        "node-42 lexical summary",
    );
    record_node(&fixture.store, SESSION, None, exact);
    record_node(&fixture.store, SESSION, None, lexical);
    fixture
        .projection
        .synchronize(&fixture.store)
        .expect("exact-match fixture sync");

    let snapshot = fixture
        .projection
        .capture_snapshot(SESSION, None)
        .expect("exact-match snapshot");
    assert_eq!(snapshot_ids(&snapshot), vec!["node-42", "other"]);

    let signature = TaskSignatureV2 {
        request: "node-42".into(),
        entities: vec![" NODE-42 ".into()],
        resources: vec!["NODE-42".into()],
        ..TaskSignatureV2::default()
    };
    let task_query = TaskQuery::new(signature).expect("exact query");
    let normalized_identity_query = TaskQuery::new(TaskSignatureV2 {
        request: "request without an identity".into(),
        entities: vec![" NODE-42 ".into()],
        resources: vec!["node-42".into()],
        ..TaskSignatureV2::default()
    })
    .expect("normalized identity query");
    assert_eq!(
        normalized_identity_query.exact_terms(),
        &["node-42".to_owned()]
    );
    assert!(
        normalized_identity_query
            .matches_exact_term(" node-42 ")
            .expect("exact match")
    );
    assert!(
        normalized_identity_query
            .matches_exact_term(" NODE-42 ")
            .expect("normalized entity/resource exact match")
    );
    assert!(
        !normalized_identity_query
            .matches_exact_term("node-420")
            .expect("near miss")
    );
    assert!(
        !normalized_identity_query
            .matches_exact_term("claim")
            .expect("kind is not an exact term")
    );
    assert!(
        !normalized_identity_query
            .matches_exact_term("unrelated summary")
            .expect("summary is not an exact term")
    );

    let ranking = ContextQueryRanking::new(
        &task_query,
        snapshot.candidates().iter().cloned(),
        Utc::now(),
        ContextResultLimit::new(10).expect("limit"),
        None,
    )
    .expect("context-owned exact and lexical ranking");
    let compiled = ContextCompiler::default()
        .compile_ranked_query(&ranking, None)
        .expect("compile context-owned ranking");
    assert_eq!(
        compiled
            .nodes
            .iter()
            .map(|candidate| candidate.id.as_str())
            .collect::<Vec<_>>(),
        vec!["node-42", "other"]
    );

    let kind_and_summary_query = TaskQuery::new(TaskSignatureV2 {
        request: "unrelated".into(),
        ..TaskSignatureV2::default()
    })
    .expect("kind-summary query");
    assert!(
        !kind_and_summary_query
            .matches_exact_term("claim")
            .expect("kind exact check")
    );
    assert!(
        !kind_and_summary_query
            .matches_exact_term("unrelated")
            .expect("summary exact check")
    );
}

#[test]
fn durable_node_and_list_bounds_accept_n_and_reject_n_plus_one() {
    let id_at_limit = "i".repeat(MAX_CONTEXT_NODE_ID_BYTES);
    assert!(
        rebuild_result(|store| {
            let evidence =
                append_source_event(store, SESSION, None, EventActor::User, "input.received");
            record_node(
                store,
                SESSION,
                None,
                node(
                    id_at_limit,
                    ContextScope::Session,
                    ContextOrigin::User,
                    EpistemicStatus::Verified,
                    vec![evidence.event_id],
                    Vec::new(),
                    "id bound",
                ),
            );
        })
        .is_ok()
    );

    let id_over_limit = "i".repeat(MAX_CONTEXT_NODE_ID_BYTES + 1);
    assert!(matches!(
        rebuild_result(|store| {
            let evidence =
                append_source_event(store, SESSION, None, EventActor::User, "input.received");
            record_node(
                store,
                SESSION,
                None,
                node(
                    id_over_limit,
                    ContextScope::Session,
                    ContextOrigin::User,
                    EpistemicStatus::Verified,
                    vec![evidence.event_id],
                    Vec::new(),
                    "id bound",
                ),
            );
        }),
        Err(ContextProjectionError::DurableBytesExceeded {
            field: "context node id",
            actual: 257,
            maximum: 256,
        })
    ));

    let oversized_error = rebuild_result(|store| {
        let evidence =
            append_source_event(store, SESSION, None, EventActor::User, "input.received");
        let mut oversized = node(
            "i".repeat(1024 * 1024),
            ContextScope::Session,
            ContextOrigin::User,
            EpistemicStatus::Verified,
            vec![evidence.event_id],
            Vec::new(),
            "",
        );
        oversized.confidence = f32::NAN;
        record_node(store, SESSION, None, oversized);
    })
    .expect_err("one-mebibyte node ID");
    assert!(matches!(
        &oversized_error,
        ContextProjectionError::DurableBytesExceeded {
            field: "serialized context payload",
            actual,
            maximum: MAX_SERIALIZED_CONTEXT_PAYLOAD_BYTES,
        } if *actual > 1_048_576
    ));
    let rendered_oversized_error = oversized_error.to_string();
    assert!(rendered_oversized_error.len() < 256);
    assert!(!rendered_oversized_error.contains(&"i".repeat(MAX_CONTEXT_NODE_ID_BYTES + 1)));

    let clone_safe_error = rebuild_result(|store| {
        let evidence =
            append_source_event(store, SESSION, None, EventActor::User, "input.received");
        record_node(
            store,
            SESSION,
            None,
            node(
                "i".repeat(64 * 1024),
                ContextScope::Session,
                ContextOrigin::User,
                EpistemicStatus::Verified,
                vec![evidence.event_id],
                Vec::new(),
                "",
            ),
        );
    })
    .expect_err("node ID bound precedes clone-producing node validation");
    assert!(matches!(
        &clone_safe_error,
        ContextProjectionError::DurableBytesExceeded {
            field: "context node id",
            actual: 65_536,
            maximum: 256,
        }
    ));
    let rendered_clone_safe_error = clone_safe_error.to_string();
    assert!(rendered_clone_safe_error.len() < 256);
    assert!(!rendered_clone_safe_error.contains(&"i".repeat(MAX_CONTEXT_NODE_ID_BYTES + 1)));

    assert!(
        rebuild_result(|store| {
            let evidence =
                append_source_event(store, SESSION, None, EventActor::User, "input.received");
            record_node(
                store,
                SESSION,
                None,
                node(
                    "summary-bound",
                    ContextScope::Session,
                    ContextOrigin::User,
                    EpistemicStatus::Verified,
                    vec![evidence.event_id],
                    Vec::new(),
                    "s".repeat(MAX_CONTEXT_SUMMARY_BYTES),
                ),
            );
        })
        .is_ok()
    );
    assert!(matches!(
        rebuild_result(|store| {
            let evidence =
                append_source_event(store, SESSION, None, EventActor::User, "input.received");
            record_node(
                store,
                SESSION,
                None,
                node(
                    "summary-over",
                    ContextScope::Session,
                    ContextOrigin::User,
                    EpistemicStatus::Verified,
                    vec![evidence.event_id],
                    Vec::new(),
                    "s".repeat(MAX_CONTEXT_SUMMARY_BYTES + 1),
                ),
            );
        }),
        Err(ContextProjectionError::DurableBytesExceeded {
            field: "context node summary",
            actual: 65_001,
            maximum: 65_000,
        })
    ));

    assert!(
        rebuild_result(|store| {
            let sources = (0..MAX_CONTEXT_SOURCE_EVENT_IDS)
                .map(|index| {
                    append_source_event(
                        store,
                        SESSION,
                        None,
                        EventActor::User,
                        &format!("input.source.{index}"),
                    )
                    .event_id
                })
                .collect();
            record_node(
                store,
                SESSION,
                None,
                node(
                    "sources-bound",
                    ContextScope::Session,
                    ContextOrigin::User,
                    EpistemicStatus::Verified,
                    sources,
                    Vec::new(),
                    "source list bound",
                ),
            );
        })
        .is_ok()
    );
    assert!(matches!(
        rebuild_result(|store| {
            let sources = (0..=MAX_CONTEXT_SOURCE_EVENT_IDS)
                .map(|index| {
                    append_source_event(
                        store,
                        SESSION,
                        None,
                        EventActor::User,
                        &format!("input.source.{index}"),
                    )
                    .event_id
                })
                .collect();
            record_node(
                store,
                SESSION,
                None,
                node(
                    "sources-over",
                    ContextScope::Session,
                    ContextOrigin::User,
                    EpistemicStatus::Verified,
                    sources,
                    Vec::new(),
                    "source list over",
                ),
            );
        }),
        Err(ContextProjectionError::DurableListOutOfRange {
            field: "source_event_ids",
            actual: 65,
            minimum: 1,
            maximum: 64,
        })
    ));

    assert!(
        rebuild_result(|store| {
            let evidence =
                append_source_event(store, SESSION, None, EventActor::User, "input.received");
            let targets = (0..MAX_CONTEXT_SUPERSEDES)
                .map(|index| {
                    let target = format!("target-{index}");
                    record_node(
                        store,
                        SESSION,
                        None,
                        node(
                            target.clone(),
                            ContextScope::Session,
                            ContextOrigin::User,
                            EpistemicStatus::Verified,
                            vec![evidence.event_id.clone()],
                            Vec::new(),
                            "supersession target",
                        ),
                    );
                    target
                })
                .collect();
            record_node(
                store,
                SESSION,
                None,
                node(
                    "replacement-at-bound",
                    ContextScope::Session,
                    ContextOrigin::User,
                    EpistemicStatus::Verified,
                    vec![evidence.event_id],
                    targets,
                    "replacement",
                ),
            );
        })
        .is_ok()
    );
    assert!(matches!(
        rebuild_result(|store| {
            let evidence =
                append_source_event(store, SESSION, None, EventActor::User, "input.received");
            let targets = (0..=MAX_CONTEXT_SUPERSEDES)
                .map(|index| {
                    let target = format!("target-{index}");
                    record_node(
                        store,
                        SESSION,
                        None,
                        node(
                            target.clone(),
                            ContextScope::Session,
                            ContextOrigin::User,
                            EpistemicStatus::Verified,
                            vec![evidence.event_id.clone()],
                            Vec::new(),
                            "supersession target",
                        ),
                    );
                    target
                })
                .collect();
            record_node(
                store,
                SESSION,
                None,
                node(
                    "replacement-over-bound",
                    ContextScope::Session,
                    ContextOrigin::User,
                    EpistemicStatus::Verified,
                    vec![evidence.event_id],
                    targets,
                    "replacement",
                ),
            );
        }),
        Err(ContextProjectionError::DurableListOutOfRange {
            field: "supersedes",
            actual: 65,
            minimum: 0,
            maximum: 64,
        })
    ));

    let maximum_shape = node(
        "i".repeat(MAX_CONTEXT_NODE_ID_BYTES),
        ContextScope::Session,
        ContextOrigin::User,
        EpistemicStatus::Verified,
        vec!["source".into(); MAX_CONTEXT_SOURCE_EVENT_IDS],
        vec!["target".into(); MAX_CONTEXT_SUPERSEDES],
        "s".repeat(MAX_CONTEXT_SUMMARY_BYTES),
    );
    let node_bytes = serde_json::to_vec(&maximum_shape).expect("serialize maximum node");
    let payload_bytes = serde_json::to_vec(&ContextNodeRecordedPayloadV1::new(maximum_shape))
        .expect("serialize maximum payload");
    assert!(node_bytes.len() <= MAX_SERIALIZED_CONTEXT_NODE_BYTES);
    assert!(payload_bytes.len() <= MAX_SERIALIZED_CONTEXT_PAYLOAD_BYTES);
}

#[test]
fn event_actor_version_envelope_span_scope_and_additive_payload_fields_are_checked() {
    let fixture = fixture();
    let evidence = append_source_event(
        &fixture.store,
        SESSION,
        None,
        EventActor::User,
        "input.received",
    );
    let valid_node = node(
        "additive",
        ContextScope::Session,
        ContextOrigin::User,
        EpistemicStatus::Verified,
        vec![evidence.event_id.clone()],
        Vec::new(),
        "additive payload",
    );
    let mut additive_payload = payload(&valid_node);
    additive_payload
        .as_object_mut()
        .expect("payload object")
        .insert("future_field".into(), json!({"ignored": true}));
    let event = context_event(
        &fixture.store,
        Some(SESSION),
        None,
        EventActor::System,
        additive_payload,
        Some(evidence.event_id.clone()),
        Some(SESSION.into()),
        None,
    );
    fixture
        .projection
        .synchronize(&fixture.store)
        .expect("additive payload is forward compatible");
    assert_eq!(
        fixture
            .projection
            .checkpoint()
            .expect("checkpoint")
            .through_event_id
            .as_deref(),
        Some(event.event_id.as_str())
    );

    let error = rebuild_result(|store| {
        let evidence =
            append_source_event(store, SESSION, None, EventActor::User, "input.received");
        context_event(
            store,
            Some(SESSION),
            None,
            EventActor::User,
            payload(&node(
                "wrong-actor",
                ContextScope::Session,
                ContextOrigin::User,
                EpistemicStatus::Verified,
                vec![evidence.event_id.clone()],
                Vec::new(),
                "wrong actor",
            )),
            Some(evidence.event_id),
            Some(SESSION.into()),
            None,
        );
    })
    .expect_err("non-system context event");
    assert!(matches!(error, ContextProjectionError::InvalidActor { .. }));

    let error = rebuild_result(|store| {
        let evidence =
            append_source_event(store, SESSION, None, EventActor::User, "input.received");
        let mut invalid_payload = payload(&node(
            "wrong-version",
            ContextScope::Session,
            ContextOrigin::User,
            EpistemicStatus::Verified,
            vec![evidence.event_id.clone()],
            Vec::new(),
            "wrong version",
        ));
        invalid_payload["event_version"] = json!(u64::from(CONTEXT_NODE_EVENT_VERSION) + 1);
        context_event(
            store,
            Some(SESSION),
            None,
            EventActor::System,
            invalid_payload,
            Some(evidence.event_id),
            Some(SESSION.into()),
            None,
        );
    })
    .expect_err("unsupported payload version");
    assert!(matches!(
        error,
        ContextProjectionError::UnsupportedEventVersion { found: 2, .. }
    ));

    let error = rebuild_result(|store| {
        let evidence =
            append_source_event(store, SESSION, None, EventActor::User, "input.received");
        context_event(
            store,
            None,
            None,
            EventActor::System,
            payload(&node(
                "missing-session",
                ContextScope::Session,
                ContextOrigin::User,
                EpistemicStatus::Verified,
                vec![evidence.event_id.clone()],
                Vec::new(),
                "missing session",
            )),
            Some(evidence.event_id),
            None,
            None,
        );
    })
    .expect_err("missing session envelope");
    assert!(matches!(error, ContextProjectionError::InvalidScope { .. }));

    let error = rebuild_result(|store| {
        let evidence =
            append_source_event(store, SESSION, None, EventActor::User, "input.received");
        context_event(
            store,
            Some(SESSION),
            None,
            EventActor::System,
            payload(&node(
                "task-without-task-envelope",
                ContextScope::Task,
                ContextOrigin::User,
                EpistemicStatus::Verified,
                vec![evidence.event_id.clone()],
                Vec::new(),
                "scope mismatch",
            )),
            Some(evidence.event_id),
            Some(SESSION.into()),
            None,
        );
    })
    .expect_err("task scope without task envelope");
    assert!(matches!(error, ContextProjectionError::InvalidScope { .. }));

    let error = rebuild_result(|store| {
        let evidence =
            append_source_event(store, SESSION, None, EventActor::User, "input.received");
        context_event(
            store,
            Some(SESSION),
            None,
            EventActor::System,
            payload(&node(
                "wrong-correlation",
                ContextScope::Session,
                ContextOrigin::User,
                EpistemicStatus::Verified,
                vec![evidence.event_id.clone()],
                Vec::new(),
                "wrong correlation",
            )),
            Some(evidence.event_id),
            Some("not-session".into()),
            None,
        );
    })
    .expect_err("wrong correlation");
    assert!(matches!(
        error,
        ContextProjectionError::CorrelationMismatch { .. }
    ));

    let error = rebuild_result(|store| {
        let evidence =
            append_source_event(store, SESSION, None, EventActor::User, "input.received");
        context_event(
            store,
            Some(SESSION),
            None,
            EventActor::System,
            payload(&node(
                "wrong-causation",
                ContextScope::Session,
                ContextOrigin::User,
                EpistemicStatus::Verified,
                vec![evidence.event_id.clone()],
                Vec::new(),
                "wrong causation",
            )),
            Some("not-the-source".into()),
            Some(SESSION.into()),
            None,
        );
    })
    .expect_err("wrong causation");
    assert!(matches!(
        error,
        ContextProjectionError::CausationMismatch { .. }
    ));

    let error = rebuild_result(|store| {
        let evidence =
            append_source_event(store, SESSION, None, EventActor::User, "input.received");
        context_event(
            store,
            Some(SESSION),
            None,
            EventActor::System,
            payload(&node(
                "unexpected-span",
                ContextScope::Session,
                ContextOrigin::User,
                EpistemicStatus::Verified,
                vec![evidence.event_id.clone()],
                Vec::new(),
                "unexpected span",
            )),
            Some(evidence.event_id),
            Some(SESSION.into()),
            Some("span".into()),
        );
    })
    .expect_err("version one context span");
    assert!(matches!(
        error,
        ContextProjectionError::UnexpectedSpan { .. }
    ));
}

#[test]
fn all_origin_attestations_allow_mixed_sources_and_ban_model_assertions() {
    let cases = [
        (
            ContextOrigin::User,
            EventActor::User,
            EpistemicStatus::Asserted,
        ),
        (
            ContextOrigin::Model,
            EventActor::Model,
            EpistemicStatus::Inferred,
        ),
        (
            ContextOrigin::Capability,
            EventActor::Capability,
            EpistemicStatus::Verified,
        ),
        (
            ContextOrigin::Policy,
            EventActor::Policy,
            EpistemicStatus::Verified,
        ),
        (
            ContextOrigin::System,
            EventActor::System,
            EpistemicStatus::Verified,
        ),
    ];
    for (index, (origin, actor, epistemic)) in cases.into_iter().enumerate() {
        let result = rebuild_result(|store| {
            let evidence = append_source_event(
                store,
                SESSION,
                None,
                actor,
                &format!("evidence.origin.{index}"),
            );
            record_node(
                store,
                SESSION,
                None,
                node(
                    format!("origin-{index}"),
                    ContextScope::Session,
                    origin,
                    epistemic,
                    vec![evidence.event_id],
                    Vec::new(),
                    "origin evidence",
                ),
            );
        });
        assert!(
            result.is_ok(),
            "matching origin actor must attest: {origin:?}"
        );
    }

    for (index, (origin, actor, epistemic)) in cases.into_iter().enumerate() {
        let wrong_actor = match actor {
            EventActor::User => EventActor::Model,
            EventActor::Model => EventActor::User,
            EventActor::Capability => EventActor::Policy,
            EventActor::Policy => EventActor::Capability,
            EventActor::System => EventActor::User,
            EventActor::Scheduler => EventActor::User,
        };
        let error = rebuild_result(|store| {
            let evidence = append_source_event(
                store,
                SESSION,
                None,
                wrong_actor,
                &format!("evidence.wrong-origin.{index}"),
            );
            record_node(
                store,
                SESSION,
                None,
                node(
                    format!("wrong-origin-{index}"),
                    ContextScope::Session,
                    origin,
                    epistemic,
                    vec![evidence.event_id],
                    Vec::new(),
                    "wrong origin evidence",
                ),
            );
        })
        .expect_err("unattested origin");
        assert!(matches!(
            error,
            ContextProjectionError::OriginNotAttested { .. }
        ));
    }

    let mixed = rebuild_result(|store| {
        let user_source =
            append_source_event(store, SESSION, None, EventActor::User, "user.evidence");
        let capability_source = append_source_event(
            store,
            SESSION,
            None,
            EventActor::Capability,
            "capability.evidence",
        );
        record_node(
            store,
            SESSION,
            None,
            node(
                "mixed-origin",
                ContextScope::Session,
                ContextOrigin::User,
                EpistemicStatus::Asserted,
                vec![capability_source.event_id, user_source.event_id],
                Vec::new(),
                "mixed actor evidence",
            ),
        );
    });
    assert!(mixed.is_ok(), "additional non-matching evidence is allowed");

    let user_without_user_source = rebuild_result(|store| {
        let evidence = append_source_event(
            store,
            SESSION,
            None,
            EventActor::Capability,
            "capability.evidence",
        );
        record_node(
            store,
            SESSION,
            None,
            node(
                "asserted-without-user",
                ContextScope::Session,
                ContextOrigin::User,
                EpistemicStatus::Asserted,
                vec![evidence.event_id],
                Vec::new(),
                "user assertion",
            ),
        );
    })
    .expect_err("asserted user claim needs user evidence");
    assert!(matches!(
        user_without_user_source,
        ContextProjectionError::OriginNotAttested {
            origin: ContextOrigin::User,
            ..
        }
    ));

    let model_assertion = rebuild_result(|store| {
        let evidence =
            append_source_event(store, SESSION, None, EventActor::Model, "model.evidence");
        record_node(
            store,
            SESSION,
            None,
            node(
                "model-assertion",
                ContextScope::Session,
                ContextOrigin::Model,
                EpistemicStatus::Asserted,
                vec![evidence.event_id],
                Vec::new(),
                "model assertion",
            ),
        );
    })
    .expect_err("model-origin assertion");
    assert!(matches!(
        model_assertion,
        ContextProjectionError::InvalidNode { .. }
    ));
}

#[test]
fn source_session_task_prior_and_greatest_sequence_permutations_are_rejected_or_allowed() {
    let future_source_fixture = fixture();
    let captured_high_water = future_source_fixture
        .store
        .latest_seq()
        .expect("empty source high water");
    let future_source = append_source_event(
        &future_source_fixture.store,
        SESSION,
        None,
        EventActor::User,
        "input.future-source",
    );
    let source_count = future_source_fixture
        .store
        .count()
        .expect("future source count");
    let prior_error = validate_draft_at(
        &future_source_fixture.projection,
        &future_source_fixture.store,
        captured_high_water,
        &ContextNodeDraft::session(
            SESSION,
            node(
                "future-source",
                ContextScope::Session,
                ContextOrigin::User,
                EpistemicStatus::Verified,
                vec![future_source.event_id.clone()],
                Vec::new(),
                "source is not prior",
            ),
        ),
    )
    .expect_err("a source at the captured cutoff is not prior");
    assert!(matches!(
        prior_error,
        ContextProjectionError::SourceNotPrior {
            node_id,
            source_event_id,
            source_seq,
        } if node_id == "future-source"
            && source_event_id == future_source.event_id
            && source_seq == future_source.seq
    ));
    assert_eq!(
        future_source_fixture
            .store
            .count()
            .expect("future source count unchanged"),
        source_count
    );

    let missing = rebuild_result(|store| {
        let value = node(
            "missing-source",
            ContextScope::Session,
            ContextOrigin::User,
            EpistemicStatus::Verified,
            vec!["does-not-exist".into()],
            Vec::new(),
            "missing source",
        );
        record_node(store, SESSION, None, value);
    })
    .expect_err("missing source");
    assert!(matches!(
        missing,
        ContextProjectionError::MissingSource { .. }
    ));

    let cross_session = rebuild_result(|store| {
        let evidence = append_source_event(
            store,
            "other-session",
            None,
            EventActor::User,
            "input.other-session",
        );
        record_node(
            store,
            SESSION,
            None,
            node(
                "cross-session-source",
                ContextScope::Session,
                ContextOrigin::User,
                EpistemicStatus::Verified,
                vec![evidence.event_id],
                Vec::new(),
                "cross session source",
            ),
        );
    })
    .expect_err("cross-session source");
    assert!(matches!(
        cross_session,
        ContextProjectionError::SourceSessionMismatch { .. }
    ));

    let cross_task = rebuild_result(|store| {
        let evidence = append_source_event(
            store,
            SESSION,
            Some(TASK_B),
            EventActor::User,
            "input.other-task",
        );
        record_node(
            store,
            SESSION,
            Some(TASK_A),
            node(
                "cross-task-source",
                ContextScope::Task,
                ContextOrigin::User,
                EpistemicStatus::Verified,
                vec![evidence.event_id],
                Vec::new(),
                "cross task source",
            ),
        );
    })
    .expect_err("cross-task source");
    assert!(matches!(
        cross_task,
        ContextProjectionError::SourceTaskMismatch { .. }
    ));

    let task_can_cite_session_source = rebuild_result(|store| {
        let evidence = append_source_event(
            store,
            SESSION,
            None,
            EventActor::User,
            "input.session-source",
        );
        record_node(
            store,
            SESSION,
            Some(TASK_A),
            node(
                "task-uses-session-source",
                ContextScope::Task,
                ContextOrigin::User,
                EpistemicStatus::Verified,
                vec![evidence.event_id],
                Vec::new(),
                "task session source",
            ),
        );
    });
    assert!(task_can_cite_session_source.is_ok());

    let session_can_cite_task_source = rebuild_result(|store| {
        let evidence = append_source_event(
            store,
            SESSION,
            Some(TASK_A),
            EventActor::User,
            "input.task-source",
        );
        record_node(
            store,
            SESSION,
            None,
            node(
                "session-uses-task-source",
                ContextScope::Session,
                ContextOrigin::User,
                EpistemicStatus::Verified,
                vec![evidence.event_id],
                Vec::new(),
                "session task source",
            ),
        );
    });
    assert!(session_can_cite_task_source.is_ok());

    let nonmaximum = rebuild_result(|store| {
        let first = append_source_event(store, SESSION, None, EventActor::User, "input.first");
        let second = append_source_event(store, SESSION, None, EventActor::User, "input.second");
        let value = node(
            "nonmaximum-causation",
            ContextScope::Session,
            ContextOrigin::User,
            EpistemicStatus::Verified,
            vec![second.event_id.clone(), first.event_id.clone()],
            Vec::new(),
            "greatest source wins",
        );
        context_event(
            store,
            Some(SESSION),
            None,
            EventActor::System,
            payload(&value),
            Some(first.event_id),
            Some(SESSION.into()),
            None,
        );
    })
    .expect_err("causation must be greatest source sequence");
    assert!(matches!(
        nonmaximum,
        ContextProjectionError::CausationMismatch { .. }
    ));
}

#[test]
fn rebuild_rejects_nonmaximum_causation_unattested_origin_and_summary_65001() {
    let nonmaximum = rebuild_result(|store| {
        let first = append_source_event(store, SESSION, None, EventActor::User, "input.first");
        let second = append_source_event(store, SESSION, None, EventActor::User, "input.second");
        let value = node(
            "forged-causation",
            ContextScope::Session,
            ContextOrigin::User,
            EpistemicStatus::Verified,
            vec![first.event_id.clone(), second.event_id.clone()],
            Vec::new(),
            "forged causation",
        );
        context_event(
            store,
            Some(SESSION),
            None,
            EventActor::System,
            payload(&value),
            Some(first.event_id),
            Some(SESSION.into()),
            None,
        );
    })
    .expect_err("forged nonmaximum causation");
    assert!(matches!(
        nonmaximum,
        ContextProjectionError::CausationMismatch { .. }
    ));

    let unattested = rebuild_result(|store| {
        let evidence = append_source_event(store, SESSION, None, EventActor::User, "input.user");
        let value = node(
            "unattested-capability",
            ContextScope::Session,
            ContextOrigin::Capability,
            EpistemicStatus::Verified,
            vec![evidence.event_id],
            Vec::new(),
            "unattested origin",
        );
        record_node(store, SESSION, None, value);
    })
    .expect_err("unattested origin");
    assert!(matches!(
        unattested,
        ContextProjectionError::OriginNotAttested {
            origin: ContextOrigin::Capability,
            ..
        }
    ));

    let summary_over = rebuild_result(|store| {
        let evidence = append_source_event(store, SESSION, None, EventActor::User, "input.user");
        record_node(
            store,
            SESSION,
            None,
            node(
                "summary-65001",
                ContextScope::Session,
                ContextOrigin::User,
                EpistemicStatus::Verified,
                vec![evidence.event_id],
                Vec::new(),
                "s".repeat(65_001),
            ),
        );
    })
    .expect_err("summary 65,001");
    assert!(matches!(
        summary_over,
        ContextProjectionError::DurableBytesExceeded {
            field: "context node summary",
            actual: 65_001,
            maximum: 65_000,
        }
    ));
}

#[test]
fn atomic_page_rollback_uses_a_real_sqlite_trigger_and_preserves_checkpoint() {
    let fixture = fixture();
    let evidence = append_source_event(
        &fixture.store,
        SESSION,
        None,
        EventActor::User,
        "input.received",
    );
    let prefix = record_node(
        &fixture.store,
        SESSION,
        None,
        node(
            "prefix",
            ContextScope::Session,
            ContextOrigin::User,
            EpistemicStatus::Verified,
            vec![evidence.event_id.clone()],
            Vec::new(),
            "prefix",
        ),
    );
    fixture
        .projection
        .synchronize_through(&fixture.store, prefix.seq)
        .expect("prefix sync");
    let before = fixture.projection.checkpoint().expect("prefix checkpoint");
    let first = record_node(
        &fixture.store,
        SESSION,
        None,
        node(
            "page-first",
            ContextScope::Session,
            ContextOrigin::User,
            EpistemicStatus::Verified,
            vec![evidence.event_id.clone()],
            Vec::new(),
            "page first",
        ),
    );
    let second = record_node(
        &fixture.store,
        SESSION,
        None,
        node(
            "page-second",
            ContextScope::Session,
            ContextOrigin::User,
            EpistemicStatus::Verified,
            vec![evidence.event_id],
            Vec::new(),
            "page second",
        ),
    );
    let event_count = fixture.store.count().expect("source count before trigger");
    let connection =
        Connection::open(&fixture.projection_path).expect("open cache trigger connection");
    connection
        .execute_batch(
            "CREATE TRIGGER fail_page_second BEFORE INSERT ON projected_nodes
             WHEN NEW.node_id = 'page-second'
             BEGIN SELECT RAISE(ABORT, 'test page failure'); END;",
        )
        .expect("install SQLite trigger");
    drop(connection);

    let error = fixture
        .projection
        .synchronize(&fixture.store)
        .expect_err("trigger must abort page transaction");
    assert!(matches!(error, ContextProjectionError::Sqlite(_)));
    assert_eq!(
        fixture
            .projection
            .checkpoint()
            .expect("rollback checkpoint"),
        before
    );
    let rolled_back = fixture
        .projection
        .capture_snapshot(SESSION, None)
        .expect("snapshot after page rollback");
    assert_eq!(snapshot_ids(&rolled_back), vec!["prefix"]);
    assert_eq!(rolled_back.scanned_rows(), 1);
    assert_eq!(
        fixture.store.count().expect("source count after trigger"),
        event_count
    );

    let connection = Connection::open(&fixture.projection_path).expect("remove SQLite trigger");
    connection
        .execute_batch("DROP TRIGGER fail_page_second;")
        .expect("drop SQLite trigger");
    drop(connection);
    let recovered = fixture
        .projection
        .synchronize(&fixture.store)
        .expect("retry rolled-back page");
    assert_eq!(recovered.checkpoint.through_seq, second.seq);
    let recovered_snapshot = fixture
        .projection
        .capture_snapshot(SESSION, None)
        .expect("snapshot after retry");
    assert_eq!(
        snapshot_ids(&recovered_snapshot),
        vec!["prefix", "page-first", "page-second"]
    );
    assert!(
        lookup_committed_identity(&fixture, SESSION, "page-first")
            .expect("first row after retry")
            .is_some()
    );
    assert!(
        lookup_committed_identity(&fixture, SESSION, "page-second")
            .expect("second row after retry")
            .is_some()
    );
    assert_eq!(first.seq + 1, second.seq);
}

#[test]
fn exact_event_sync_and_session_identity_lookup_are_anchor_bound() {
    let fixture = fixture();
    let evidence = append_source_event(
        &fixture.store,
        SESSION,
        None,
        EventActor::User,
        "input.received",
    );
    let node_event = record_node(
        &fixture.store,
        SESSION,
        None,
        node(
            "lookup-node",
            ContextScope::Session,
            ContextOrigin::User,
            EpistemicStatus::Verified,
            vec![evidence.event_id],
            Vec::new(),
            "lookup",
        ),
    );
    let exact = fixture
        .projection
        .synchronize_through_event(&fixture.store, &node_event)
        .expect("exact event synchronization");
    assert_eq!(exact.checkpoint.through_seq, node_event.seq);
    assert_eq!(
        exact.checkpoint.through_event_id.as_deref(),
        Some(node_event.event_id.as_str())
    );
    let identity = lookup_committed_identity_at(
        &fixture.projection,
        &fixture.store,
        node_event.seq,
        SESSION,
        "lookup-node",
    )
    .expect("identity lookup")
    .expect("committed identity");
    assert_eq!(identity.session_id, SESSION);
    assert_eq!(identity.task_id, None);
    assert_eq!(identity.node_id, "lookup-node");
    assert_eq!(identity.event_id, node_event.event_id);
    assert_eq!(identity.event_seq, node_event.seq);
    assert!(
        lookup_committed_identity_at(
            &fixture.projection,
            &fixture.store,
            node_event.seq,
            SESSION,
            "missing",
        )
        .expect("missing identity")
        .is_none()
    );
    assert!(matches!(
        lookup_committed_identity_at(
            &fixture.projection,
            &fixture.store,
            node_event.seq,
            "",
            "lookup-node",
        ),
        Err(ContextProjectionError::InvalidScope { .. })
    ));

    let mut wrong_identity = node_event.clone();
    wrong_identity.event_id = "not-the-canonical-event".into();
    assert!(matches!(
        fixture
            .projection
            .synchronize_through_event(&fixture.store, &wrong_identity),
        Err(ContextProjectionError::TargetEventMismatch { .. })
    ));
    assert_eq!(
        fixture
            .projection
            .checkpoint()
            .expect("checkpoint unchanged")
            .through_seq,
        node_event.seq
    );

    let mut wrong_sequence = node_event.clone();
    wrong_sequence.seq = node_event.seq + 1;
    assert!(matches!(
        fixture
            .projection
            .synchronize_through_event(&fixture.store, &wrong_sequence),
        Err(ContextProjectionError::TargetEventMismatch { .. })
    ));
}

#[derive(Clone)]
struct FixtureProvider {
    calls: Arc<Mutex<Vec<(EmbeddingPurpose, String)>>>,
    outputs: Arc<Mutex<VecDeque<Result<Embedding, EmbeddingProviderError>>>>,
}

impl FixtureProvider {
    fn new(outputs: Vec<Result<Embedding, EmbeddingProviderError>>) -> Self {
        Self {
            calls: Arc::new(Mutex::new(Vec::new())),
            outputs: Arc::new(Mutex::new(outputs.into_iter().collect())),
        }
    }

    fn success(count: usize) -> Self {
        Self::new(
            (0..count)
                .map(|_| Ok(Embedding::new("fixture-v1", vec![1.0, 0.0])))
                .collect(),
        )
    }

    fn calls(&self) -> Vec<(EmbeddingPurpose, String)> {
        self.calls.lock().expect("provider calls mutex").clone()
    }
}

impl EmbeddingProvider for FixtureProvider {
    fn embed(
        &self,
        purpose: EmbeddingPurpose,
        text: &str,
    ) -> Result<Embedding, EmbeddingProviderError> {
        self.calls
            .lock()
            .expect("provider calls mutex")
            .push((purpose, text.to_owned()));
        self.outputs
            .lock()
            .expect("provider outputs mutex")
            .pop_front()
            .unwrap_or_else(|| Err(EmbeddingProviderError::failure("fixture output exhausted")))
    }
}

fn embedded_query(provider: &FixtureProvider) -> TaskQuery {
    TaskQuery::with_provider(TaskSignatureV2::new("needle"), Some(provider))
        .expect("embedded query")
}

fn embedding_fixture() -> Fixture {
    let fixture = fixture();
    let evidence = append_source_event(
        &fixture.store,
        SESSION,
        None,
        EventActor::User,
        "input.received",
    );
    record_node(
        &fixture.store,
        SESSION,
        None,
        node(
            "b",
            ContextScope::Session,
            ContextOrigin::User,
            EpistemicStatus::Verified,
            vec![evidence.event_id.clone()],
            Vec::new(),
            "needle b",
        ),
    );
    record_node(
        &fixture.store,
        SESSION,
        None,
        node(
            "a",
            ContextScope::Session,
            ContextOrigin::User,
            EpistemicStatus::Verified,
            vec![evidence.event_id],
            Vec::new(),
            "needle a",
        ),
    );
    fixture
        .projection
        .synchronize(&fixture.store)
        .expect("embedding fixture sync");
    fixture
}

#[test]
fn provider_pairing_call_order_mismatch_nonfinite_and_failure_never_return_partial_context() {
    let fixture = embedding_fixture();
    let snapshot = fixture
        .projection
        .capture_snapshot(SESSION, None)
        .expect("embedding snapshot");
    let limit = ContextResultLimit::new(10).expect("limit");
    let provider = FixtureProvider::success(3);
    let task_query = embedded_query(&provider);
    let ranking = ContextQueryRanking::new(
        &task_query,
        snapshot.candidates().iter().cloned(),
        Utc::now(),
        limit,
        Some(&provider),
    )
    .expect("embedded context ranking");
    let compiled = ContextCompiler::default()
        .compile_ranked_query(&ranking, None)
        .expect("compile embedded context ranking");
    assert_eq!(
        compiled
            .nodes
            .iter()
            .map(|candidate| candidate.id.as_str())
            .collect::<Vec<_>>(),
        vec!["a", "b"]
    );
    let calls = provider.calls();
    assert_eq!(calls.len(), 3);
    assert_eq!(calls[0].0, EmbeddingPurpose::Query);
    assert_eq!(calls[0].1, "needle");
    assert_eq!(calls[1].0, EmbeddingPurpose::Document);
    assert!(calls[1].1.starts_with("id=a\n"));
    assert_eq!(calls[2].0, EmbeddingPurpose::Document);
    assert!(calls[2].1.starts_with("id=b\n"));

    let lexical = TaskQuery::new(TaskSignatureV2::new("needle")).expect("lexical query");
    let mismatch_provider = FixtureProvider::success(1);
    assert!(matches!(
        ContextQueryRanking::new(
            &lexical,
            snapshot.candidates().iter().cloned(),
            Utc::now(),
            limit,
            Some(&mismatch_provider),
        ),
        Err(ContextQueryRankingError::ProviderModeMismatch {
            mode: RetrievalMode::LexicalOnly,
            provider_present: true,
        })
    ));
    assert!(mismatch_provider.calls().is_empty());

    let provider_for_query = FixtureProvider::success(1);
    let embedded = embedded_query(&provider_for_query);
    assert!(matches!(
        ContextQueryRanking::new(
            &embedded,
            snapshot.candidates().iter().cloned(),
            Utc::now(),
            limit,
            None,
        ),
        Err(ContextQueryRankingError::ProviderModeMismatch {
            mode: RetrievalMode::Embedded,
            provider_present: false,
        })
    ));
    assert_eq!(provider_for_query.calls().len(), 1);

    let failure_provider = FixtureProvider::new(vec![
        Ok(Embedding::new("fixture-v1", vec![1.0, 0.0])),
        Err(EmbeddingProviderError::failure("document failed")),
    ]);
    let failure_query = embedded_query(&failure_provider);
    let error = ContextQueryRanking::new(
        &failure_query,
        snapshot.candidates().iter().cloned(),
        Utc::now(),
        limit,
        Some(&failure_provider),
    )
    .expect_err("configured provider failure");
    assert!(matches!(
        error,
        ContextQueryRankingError::Retrieval(RetrievalError::ProviderFailure { detail })
            if detail == "document failed"
    ));
    assert_eq!(failure_provider.calls().len(), 2);

    let nonfinite_provider = FixtureProvider::new(vec![
        Ok(Embedding::new("fixture-v1", vec![1.0, 0.0])),
        Ok(Embedding::new("fixture-v1", vec![f32::NAN, 0.0])),
    ]);
    let nonfinite_query = embedded_query(&nonfinite_provider);
    let error = ContextQueryRanking::new(
        &nonfinite_query,
        snapshot.candidates().iter().cloned(),
        Utc::now(),
        limit,
        Some(&nonfinite_provider),
    )
    .expect_err("non-finite document vector");
    assert!(matches!(
        error,
        ContextQueryRankingError::Retrieval(RetrievalError::NonFiniteEmbeddingValue { index: 0 })
    ));
    assert_eq!(nonfinite_provider.calls().len(), 2);

    let descriptor_provider = FixtureProvider::new(vec![
        Ok(Embedding::new("fixture-v1", vec![1.0, 0.0])),
        Ok(Embedding::new("different", vec![1.0, 0.0])),
    ]);
    let descriptor_query = embedded_query(&descriptor_provider);
    let error = ContextQueryRanking::new(
        &descriptor_query,
        snapshot.candidates().iter().cloned(),
        Utc::now(),
        limit,
        Some(&descriptor_provider),
    )
    .expect_err("descriptor mismatch");
    assert!(matches!(
        error,
        ContextQueryRankingError::Retrieval(RetrievalError::EmbeddingDescriptorMismatch { .. })
    ));
    assert_eq!(descriptor_provider.calls().len(), 2);

    let dimension_provider = FixtureProvider::new(vec![
        Ok(Embedding::new("fixture-v1", vec![1.0, 0.0])),
        Ok(Embedding::new("fixture-v1", vec![1.0])),
    ]);
    let dimension_query = embedded_query(&dimension_provider);
    let error = ContextQueryRanking::new(
        &dimension_query,
        snapshot.candidates().iter().cloned(),
        Utc::now(),
        limit,
        Some(&dimension_provider),
    )
    .expect_err("embedding dimension mismatch");
    assert!(matches!(
        error,
        ContextQueryRankingError::Retrieval(RetrievalError::EmbeddingDimensionMismatch { .. })
    ));
    assert_eq!(dimension_provider.calls().len(), 2);

    let invalid_query_provider =
        FixtureProvider::new(vec![Ok(Embedding::new("fixture-v1", vec![f32::INFINITY]))]);
    assert!(matches!(
        TaskQuery::with_provider(
            TaskSignatureV2::new("needle"),
            Some(&invalid_query_provider)
        ),
        Err(RetrievalError::NonFiniteEmbeddingValue { index: 0 })
    ));
    assert_eq!(invalid_query_provider.calls().len(), 1);
}

fn payload_with_padding(value: &ContextNode, padding_len: usize) -> Value {
    let mut value = payload(value);
    value
        .as_object_mut()
        .expect("payload object")
        .insert("padding".into(), Value::String("x".repeat(padding_len)));
    value
}

fn largest_payload_padding(value: &ContextNode) -> usize {
    let mut lower = 0usize;
    let mut upper = MAX_SERIALIZED_CONTEXT_PAYLOAD_BYTES;
    while lower < upper {
        let candidate = lower + (upper - lower).div_ceil(2);
        let serialized_len = serde_json::to_vec(&payload_with_padding(value, candidate))
            .expect("serialize padded payload")
            .len();
        if serialized_len <= MAX_SERIALIZED_CONTEXT_PAYLOAD_BYTES {
            lower = candidate;
        } else {
            upper = candidate - 1;
        }
    }
    lower
}

#[test]
fn serialized_context_payload_accepts_exact_bound_and_rejects_one_more_without_partial_rows() {
    {
        let fixture = fixture();
        let evidence = append_source_event(
            &fixture.store,
            SESSION,
            None,
            EventActor::User,
            "input.received",
        );
        let value = node(
            "payload-at-bound",
            ContextScope::Session,
            ContextOrigin::User,
            EpistemicStatus::Verified,
            vec![evidence.event_id.clone()],
            Vec::new(),
            "payload bound",
        );
        let padding_len = largest_payload_padding(&value);
        let exact_payload = payload_with_padding(&value, padding_len);
        assert_eq!(
            serde_json::to_vec(&exact_payload)
                .expect("serialize exact payload")
                .len(),
            MAX_SERIALIZED_CONTEXT_PAYLOAD_BYTES
        );
        let exact_event = context_event(
            &fixture.store,
            Some(SESSION),
            None,
            EventActor::System,
            exact_payload,
            Some(evidence.event_id.clone()),
            Some(SESSION.into()),
            None,
        );
        fixture
            .projection
            .synchronize(&fixture.store)
            .expect("payload at exact bound");
        assert_eq!(
            fixture
                .projection
                .checkpoint()
                .expect("exact payload checkpoint")
                .through_seq,
            exact_event.seq
        );
    }

    let fixture = fixture();
    let evidence = append_source_event(
        &fixture.store,
        SESSION,
        None,
        EventActor::User,
        "input.received",
    );
    let value = node(
        "payload-over-bound",
        ContextScope::Session,
        ContextOrigin::User,
        EpistemicStatus::Verified,
        vec![evidence.event_id.clone()],
        Vec::new(),
        "payload bound",
    );
    let padding_len = largest_payload_padding(&value);
    let over_payload = payload_with_padding(&value, padding_len + 1);
    let serialized_len = serde_json::to_vec(&over_payload)
        .expect("serialize over payload")
        .len();
    assert_eq!(serialized_len, MAX_SERIALIZED_CONTEXT_PAYLOAD_BYTES + 1);
    context_event(
        &fixture.store,
        Some(SESSION),
        None,
        EventActor::System,
        over_payload,
        Some(evidence.event_id),
        Some(SESSION.into()),
        None,
    );
    let error = fixture
        .projection
        .rebuild(&fixture.store)
        .expect_err("payload over exact bound");
    assert!(matches!(
        error,
        ContextProjectionError::DurableBytesExceeded {
            field: "serialized context payload",
            actual: 131_073,
            maximum: 131_072,
        }
    ));
    assert_eq!(
        fixture
            .projection
            .checkpoint()
            .expect("over-bound checkpoint")
            .through_seq,
        0
    );
}

#[test]
fn trusted_draft_validation_is_projection_anchored_and_derives_causation() {
    let fixture = fixture();
    let evidence = append_source_event(
        &fixture.store,
        SESSION,
        None,
        EventActor::User,
        "input.received",
    );
    fixture
        .projection
        .synchronize(&fixture.store)
        .expect("source checkpoint");
    let value = node(
        "draft-node",
        ContextScope::Session,
        ContextOrigin::User,
        EpistemicStatus::Asserted,
        vec![evidence.event_id.clone()],
        Vec::new(),
        "draft",
    );
    let validated = validate_draft(&fixture, &ContextNodeDraft::session(SESSION, value.clone()))
        .expect("valid trusted draft");
    assert_eq!(validated.session_id(), SESSION);
    assert_eq!(validated.task_id(), None);
    assert_eq!(validated.node(), &value);
    assert_eq!(validated.causation_id(), evidence.event_id);
    assert_eq!(validated.correlation_id(), SESSION);
    assert_eq!(
        validated.payload().event_version,
        CONTEXT_NODE_EVENT_VERSION
    );
    assert_eq!(validated.payload().node, value);

    let committed = record_node(
        &fixture.store,
        SESSION,
        None,
        node(
            "already-recorded",
            ContextScope::Session,
            ContextOrigin::User,
            EpistemicStatus::Verified,
            vec![evidence.event_id.clone()],
            Vec::new(),
            "existing",
        ),
    );
    fixture
        .projection
        .synchronize(&fixture.store)
        .expect("existing node");
    let mut invalid_duplicate = node(
        "already-recorded",
        ContextScope::Session,
        ContextOrigin::User,
        EpistemicStatus::Verified,
        Vec::new(),
        Vec::new(),
        "",
    );
    invalid_duplicate.confidence = f32::NAN;
    assert!(invalid_duplicate.validate().is_err());
    let duplicate = validate_draft(
        &fixture,
        &ContextNodeDraft::session(SESSION, invalid_duplicate),
    )
    .expect_err("duplicate draft identity");
    assert!(matches!(
        duplicate,
        ContextProjectionError::DuplicateNodeIdentity {
            session_id,
            node_id,
            event_id,
            seq,
        } if session_id == SESSION
            && node_id == "already-recorded"
            && event_id.as_str() == committed.event_id.as_str()
            && seq == committed.seq
    ));
    assert_eq!(
        committed.seq,
        fixture
            .projection
            .checkpoint()
            .expect("checkpoint")
            .through_seq
    );

    let source_count = fixture
        .store
        .count()
        .expect("source count before oversized draft");
    let checkpoint_before_oversized = fixture
        .projection
        .checkpoint()
        .expect("checkpoint before oversized draft");
    let oversized_draft = ContextNodeDraft::session(
        SESSION,
        node(
            "x".repeat(1024 * 1024),
            ContextScope::Session,
            ContextOrigin::User,
            EpistemicStatus::Verified,
            vec![evidence.event_id.clone()],
            Vec::new(),
            "oversized identity",
        ),
    );
    let oversized_error = validate_draft_at(
        &fixture.projection,
        &fixture.store,
        i64::MAX,
        &oversized_draft,
    )
    .expect_err("oversized identity is rejected before event-store access");
    assert!(matches!(
        &oversized_error,
        ContextProjectionError::DurableBytesExceeded {
            field: "context node id",
            actual: 1_048_576,
            maximum: 256,
        }
    ));
    let rendered_oversized_error = oversized_error.to_string();
    assert!(rendered_oversized_error.len() < 256);
    assert!(!rendered_oversized_error.contains(&"x".repeat(MAX_CONTEXT_NODE_ID_BYTES + 1)));
    assert_eq!(
        fixture
            .store
            .count()
            .expect("source count after oversized draft"),
        source_count
    );
    assert_eq!(
        fixture
            .projection
            .checkpoint()
            .expect("checkpoint after oversized draft"),
        checkpoint_before_oversized
    );

    noise(&fixture.store, Some(SESSION), None);
    let stale = validate_draft(
        &fixture,
        &ContextNodeDraft::session(
            SESSION,
            node(
                "stale",
                ContextScope::Session,
                ContextOrigin::User,
                EpistemicStatus::Verified,
                vec![evidence.event_id],
                Vec::new(),
                "stale projection",
            ),
        ),
    )
    .expect_err("stale projection");
    assert!(matches!(
        stale,
        ContextProjectionError::ProjectionNotSynchronized { .. }
    ));
}

#[test]
fn unsupported_scopes_and_reference_shape_errors_fail_closed_before_checkpoint_advance() {
    for unsupported_scope in [
        ContextScope::Turn,
        ContextScope::Project,
        ContextScope::Device,
        ContextScope::Global,
    ] {
        let error = rebuild_result(|store| {
            let evidence =
                append_source_event(store, SESSION, None, EventActor::User, "input.scope");
            record_node(
                store,
                SESSION,
                None,
                node(
                    format!("unsupported-{unsupported_scope:?}"),
                    unsupported_scope,
                    ContextOrigin::User,
                    EpistemicStatus::Verified,
                    vec![evidence.event_id],
                    Vec::new(),
                    "unsupported scope",
                ),
            );
        })
        .expect_err("unsupported scope");
        assert!(matches!(error, ContextProjectionError::InvalidScope { .. }));
    }

    let duplicate_source = rebuild_result(|store| {
        let evidence = append_source_event(store, SESSION, None, EventActor::User, "input.source");
        record_node(
            store,
            SESSION,
            None,
            node(
                "duplicate-source",
                ContextScope::Session,
                ContextOrigin::User,
                EpistemicStatus::Verified,
                vec![evidence.event_id.clone(), evidence.event_id],
                Vec::new(),
                "duplicate source",
            ),
        );
    })
    .expect_err("duplicate source IDs");
    assert!(matches!(
        duplicate_source,
        ContextProjectionError::DuplicateReference {
            field: "source_event_ids",
            ..
        }
    ));

    let empty_source = rebuild_result(|store| {
        let value = node(
            "empty-source",
            ContextScope::Session,
            ContextOrigin::User,
            EpistemicStatus::Verified,
            vec![String::new()],
            Vec::new(),
            "empty source",
        );
        record_node(store, SESSION, None, value);
    })
    .expect_err("empty source ID");
    assert!(matches!(
        empty_source,
        ContextProjectionError::EmptyReference {
            field: "source_event_ids",
            index: 0,
        }
    ));

    let duplicate_supersedes = rebuild_result(|store| {
        let evidence = append_source_event(store, SESSION, None, EventActor::User, "input.source");
        let value = node(
            "duplicate-supersedes",
            ContextScope::Session,
            ContextOrigin::User,
            EpistemicStatus::Verified,
            vec![evidence.event_id],
            vec!["target".into(), "target".into()],
            "duplicate supersession",
        );
        record_node(store, SESSION, None, value);
    })
    .expect_err("duplicate supersession IDs");
    assert!(matches!(
        duplicate_supersedes,
        ContextProjectionError::DuplicateReference {
            field: "supersedes",
            ..
        }
    ));

    let self_supersedes = rebuild_result(|store| {
        let evidence = append_source_event(store, SESSION, None, EventActor::User, "input.source");
        let value = node(
            "self-supersedes",
            ContextScope::Session,
            ContextOrigin::User,
            EpistemicStatus::Verified,
            vec![evidence.event_id],
            vec!["self-supersedes".into()],
            "self supersession",
        );
        record_node(store, SESSION, None, value);
    })
    .expect_err("self supersession");
    assert!(matches!(
        self_supersedes,
        ContextProjectionError::SelfSupersession { .. }
    ));
}
