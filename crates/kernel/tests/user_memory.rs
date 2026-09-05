use std::{
    sync::{Arc, Barrier},
    thread,
};

use ditto_event_store::EventStore;
use ditto_kernel::{DittoKernel, KernelConfig, KernelError};
use ditto_protocol::{
    EventActor, EventQuery, MemoryQuery, MemoryWriteOutcome, NewEvent, RememberInputCommand,
    SubmitInputCommand, event_kind,
};
use serde_json::json;
use tempfile::TempDir;

struct Fixture {
    _root: TempDir,
    config: KernelConfig,
    kernel: DittoKernel,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let config = KernelConfig::new(root.path().join("data"), root.path().join("capabilities"));
        let kernel = DittoKernel::open(config.clone()).unwrap();
        Self {
            _root: root,
            config,
            kernel,
        }
    }
    fn input(&self, session: &str, text: &str) -> RememberInputCommand {
        let input = self
            .kernel
            .record_user_input(SubmitInputCommand {
                session_id: Some(session.into()),
                task_id: None,
                text: text.into(),
            })
            .unwrap();
        RememberInputCommand {
            session_id: session.into(),
            input_event_id: input.event_id,
            replaces: None,
        }
    }
}
fn query(session: &str) -> MemoryQuery {
    MemoryQuery {
        session_id: session.into(),
        after_id: None,
        limit: Some(100),
    }
}

#[test]
fn save_correct_retry_and_rebuild_preserve_exact_text_and_one_active_memory() {
    let fixture = Fixture::new();
    let first = fixture.input("personal", "오전 8시에 업무를 시작한다");
    let saved = fixture.kernel.remember_input(first.clone()).unwrap();
    assert_eq!(saved.outcome, MemoryWriteOutcome::Recorded);
    let before = fixture.kernel.event_count().unwrap();
    let mut receiver = fixture.kernel.subscribe();
    let retry = fixture.kernel.remember_input(first.clone()).unwrap();
    assert_eq!(retry.outcome, MemoryWriteOutcome::AlreadyRecorded);
    assert_eq!(retry.event_id, saved.event_id);
    assert_eq!(fixture.kernel.event_count().unwrap(), before);
    assert!(receiver.try_recv().is_err());
    let page = fixture.kernel.list_memories(query("personal")).unwrap();
    assert_eq!(page.memories[0].text, "오전 8시에 업무를 시작한다");
    assert!(
        fixture
            .kernel
            .list_memories(query("other"))
            .unwrap()
            .memories
            .is_empty()
    );

    let mut second = fixture.input("personal", "오전 9시에 업무를 시작한다");
    second.replaces = Some(saved.memory_id.clone());
    let replacement = fixture.kernel.remember_input(second.clone()).unwrap();
    let retry = fixture.kernel.remember_input(second.clone()).unwrap();
    assert_eq!(retry.event_id, replacement.event_id);
    second.replaces = None;
    assert!(matches!(
        fixture.kernel.remember_input(second),
        Err(KernelError::MemoryConflict(_))
    ));
    let expected = fixture
        .kernel
        .list_memories(query("personal"))
        .unwrap()
        .memories;
    assert_eq!(expected.len(), 1);
    assert_eq!(expected[0].id, replacement.memory_id);
    assert_eq!(expected[0].text, "오전 9시에 업무를 시작한다");

    // Reopen with no live handles to the same store, then delete only its derived cache.
    let Fixture {
        _root,
        config,
        kernel,
    } = fixture;
    drop(kernel);
    for suffix in ["", "-wal", "-shm"] {
        let path = config
            .data_dir
            .join(format!("context-projection.db{suffix}"));
        if path.exists() {
            std::fs::remove_file(path).unwrap();
        }
    }
    let kernel = DittoKernel::open(config).unwrap();
    assert_eq!(
        kernel.list_memories(query("personal")).unwrap().memories,
        expected
    );
    let original_retry = kernel.remember_input(first).unwrap();
    assert_eq!(original_retry.event_id, saved.event_id);
    assert_eq!(
        kernel.list_memories(query("personal")).unwrap().memories,
        expected
    );
    let events = kernel.list_events(&EventQuery::default()).unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|e| e.kind == event_kind::CONTEXT_NODE_RECORDED)
            .count(),
        2
    );
    assert!(events.iter().all(
        |e| e.kind == event_kind::INPUT_RECEIVED || e.kind == event_kind::CONTEXT_NODE_RECORDED
    ));
}

#[test]
fn concurrent_corrections_and_retries_append_exactly_once() {
    let fixture = Fixture::new();
    let initial = fixture.input("personal", "initial preference");
    let saved = fixture.kernel.remember_input(initial).unwrap();
    let mut commands = [
        fixture.input("personal", "first correction"),
        fixture.input("personal", "second correction"),
    ];
    for command in &mut commands {
        command.replaces = Some(saved.memory_id.clone());
    }
    let barrier = Arc::new(Barrier::new(2));
    let handles: Vec<_> = commands
        .into_iter()
        .map(|command| {
            let kernel = fixture.kernel.clone();
            let barrier = barrier.clone();
            thread::spawn(move || {
                barrier.wait();
                kernel.remember_input(command)
            })
        })
        .collect();
    let outcomes: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    assert_eq!(outcomes.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        outcomes
            .iter()
            .filter(|result| matches!(result, Err(KernelError::MemoryConflict(_))))
            .count(),
        1
    );
    assert_eq!(
        fixture
            .kernel
            .list_memories(query("personal"))
            .unwrap()
            .memories
            .len(),
        1
    );

    let command = fixture.input("personal", "one new memory");
    let before = fixture.kernel.event_count().unwrap();
    let barrier = Arc::new(Barrier::new(4));
    let handles: Vec<_> = (0..4)
        .map(|_| {
            let kernel = fixture.kernel.clone();
            let barrier = barrier.clone();
            let command = command.clone();
            thread::spawn(move || {
                barrier.wait();
                kernel.remember_input(command).unwrap()
            })
        })
        .collect();
    let outcomes: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    assert_eq!(
        outcomes
            .iter()
            .filter(|r| r.outcome == MemoryWriteOutcome::Recorded)
            .count(),
        1
    );
    assert!(outcomes.iter().all(|r| r.event_id == outcomes[0].event_id));
    assert_eq!(fixture.kernel.event_count().unwrap(), before + 1);
}

#[test]
fn wrong_sources_scope_and_client_authority_never_create_memory() {
    let fixture = Fixture::new();
    let store = EventStore::open(fixture.config.data_dir.join("state.db")).unwrap();
    let mut invalid_commands = Vec::new();
    for (actor, kind, task) in [
        (EventActor::Model, event_kind::INPUT_RECEIVED, None),
        (EventActor::User, "fixture.other", None),
        (
            EventActor::User,
            event_kind::INPUT_RECEIVED,
            Some("task".into()),
        ),
    ] {
        let event = store
            .append(NewEvent {
                actor,
                kind: kind.into(),
                session_id: Some("personal".into()),
                task_id: task,
                payload: json!({"text":"never a personal assertion"}),
                causation_id: None,
                correlation_id: None,
                span_id: None,
            })
            .unwrap();
        invalid_commands.push(RememberInputCommand {
            session_id: "personal".into(),
            input_event_id: event.event_id,
            replaces: None,
        });
    }
    let mut other = fixture.input("other", "isolated");
    other.session_id = "personal".into();
    invalid_commands.push(other);
    let mut malformed = fixture.input("personal", "valid source");
    malformed.session_id = " personal".into();
    invalid_commands.push(malformed);
    invalid_commands.push(RememberInputCommand {
        session_id: "personal".into(),
        input_event_id: "not-an-event".into(),
        replaces: None,
    });
    let before = fixture.kernel.event_count().unwrap();
    let mut receiver = fixture.kernel.subscribe();
    for command in invalid_commands {
        assert!(fixture.kernel.remember_input(command).is_err());
    }
    assert_eq!(fixture.kernel.event_count().unwrap(), before);
    assert!(receiver.try_recv().is_err());
    for field in [
        "actor",
        "kind",
        "node",
        "origin",
        "epistemic",
        "source_event_ids",
        "task_id",
    ] {
        let mut raw =
            json!({"session_id":"personal", "input_event_id":"01J00000000000000000000000"});
        raw[field] = json!("forged");
        assert!(serde_json::from_value::<RememberInputCommand>(raw).is_err());
    }
}

#[test]
fn memory_and_page_bounds_are_exact_and_paging_does_not_append_or_full_replay() {
    let fixture = Fixture::new();
    let exact = fixture.input("personal", &"é".repeat(2048));
    fixture.kernel.remember_input(exact).unwrap();
    let over = fixture.input("personal", &("é".repeat(2048) + "a"));
    let before = fixture.kernel.event_count().unwrap();
    assert!(fixture.kernel.remember_input(over).is_err());
    assert_eq!(fixture.kernel.event_count().unwrap(), before);
    for i in 0..100 {
        fixture
            .kernel
            .remember_input(fixture.input("personal", &format!("memory {i}")))
            .unwrap();
    }
    for limit in [0, 101] {
        let mut request = query("personal");
        request.limit = Some(limit);
        assert!(fixture.kernel.list_memories(request).is_err());
    }
    let before = fixture.kernel.event_count().unwrap();
    let metrics = fixture.kernel.retrieval_verification_metrics().unwrap();
    let first = fixture.kernel.list_memories(query("personal")).unwrap();
    assert_eq!(first.memories.len(), 100);
    assert!(
        first
            .memories
            .windows(2)
            .all(|pair| pair[0].id < pair[1].id)
    );
    let second = fixture
        .kernel
        .list_memories(MemoryQuery {
            after_id: first.next_after_id.clone(),
            ..query("personal")
        })
        .unwrap();
    assert_eq!(second.memories.len(), 1);
    assert!(second.next_after_id.is_none());
    assert!(second.memories[0].id > first.next_after_id.unwrap());
    assert_eq!(fixture.kernel.event_count().unwrap(), before);
    assert_eq!(
        fixture
            .kernel
            .retrieval_verification_metrics()
            .unwrap()
            .full_replays,
        metrics.full_replays
    );
}

#[test]
fn accepted_projection_failure_is_explicit_and_retry_recovers_without_duplicate() {
    let fixture = Fixture::new();
    let command = fixture.input("personal", "recoverable memory");
    let connection =
        rusqlite::Connection::open(fixture.config.data_dir.join("context-projection.db")).unwrap();
    connection.execute_batch("CREATE TRIGGER fail_memory_projection BEFORE INSERT ON projected_nodes BEGIN SELECT RAISE(ABORT, 'private diagnostic'); END;").unwrap();
    let before = fixture.kernel.event_count().unwrap();
    let mut receiver = fixture.kernel.subscribe();
    let result = fixture.kernel.remember_input(command.clone()).unwrap();
    assert_eq!(
        result.outcome,
        MemoryWriteOutcome::CommittedButProjectionUnavailable
    );
    assert_eq!(fixture.kernel.event_count().unwrap(), before + 1);
    assert_eq!(receiver.try_recv().unwrap().event_id, result.event_id);
    assert!(receiver.try_recv().is_err());
    connection
        .execute_batch("DROP TRIGGER fail_memory_projection;")
        .unwrap();
    drop(connection);
    let retry = fixture.kernel.remember_input(command).unwrap();
    assert_eq!(retry.event_id, result.event_id);
    assert_eq!(retry.outcome, MemoryWriteOutcome::AlreadyRecorded);
    assert_eq!(fixture.kernel.event_count().unwrap(), before + 1);
    assert!(receiver.try_recv().is_err());
    assert_eq!(
        fixture
            .kernel
            .list_memories(query("personal"))
            .unwrap()
            .memories[0]
            .text,
        "recoverable memory"
    );
}

#[test]
fn corrected_memory_enters_existing_working_set_without_resurrecting_old_text() {
    let fixture = Fixture::new();
    let old = fixture
        .kernel
        .remember_input(fixture.input("personal", "I prefer morning meetings"))
        .unwrap();
    let mut correction = fixture.input("personal", "I prefer afternoon meetings");
    correction.replaces = Some(old.memory_id.clone());
    let current = fixture.kernel.remember_input(correction).unwrap();
    let before = fixture.kernel.event_count().unwrap();
    let working_set = fixture
        .kernel
        .retrieve_working_set(ditto_kernel::WorkingSetRequest {
            scope: ditto_retrieval::RetrievalScope::session(
                ditto_retrieval::SessionId::new("personal").unwrap(),
            ),
            signature: ditto_retrieval::TaskSignatureV2::new("meetings"),
            context_token_budget: None,
            context_result_limit: 20,
            capability_root_limit: 20,
            execution_epoch_limit: 20,
            capability_search: ditto_kernel::SearchContext::catalogue(),
        })
        .unwrap();
    assert_eq!(working_set.compiled_context().nodes.len(), 1);
    assert_eq!(
        working_set.compiled_context().nodes[0].id,
        current.memory_id
    );
    assert_eq!(
        working_set.compiled_context().nodes[0].summary,
        "I prefer afternoon meetings"
    );
    assert_eq!(fixture.kernel.event_count().unwrap(), before);
}

#[test]
fn memory_listing_rejects_text_that_differs_from_user_evidence_and_repairs_cache_drift() {
    use ditto_context::{
        ContextLens, ContextNode, ContextNodeKind, ContextOrigin, ContextScope, EpistemicStatus,
    };
    use ditto_kernel::TrustedContextNodeDraft;
    let fixture = Fixture::new();
    let source = fixture.input("personal", "explicit user text");
    let id = format!("memory-{}", source.input_event_id.to_ascii_lowercase());
    fixture
        .kernel
        .admit_context_node(TrustedContextNodeDraft::session(
            "personal",
            ContextNode {
                id: "memory-topic".into(),
                kind: ContextNodeKind::Claim,
                summary: "explicit user text".into(),
                origin: ContextOrigin::User,
                epistemic: EpistemicStatus::Asserted,
                scope: ContextScope::Session,
                lens: ContextLens::Task,
                confidence: 1.0,
                source_event_ids: vec![source.input_event_id.clone()],
                supersedes: vec![],
                valid_from: None,
                valid_until: None,
            },
        ))
        .unwrap();
    assert!(
        fixture
            .kernel
            .list_memories(query("personal"))
            .unwrap()
            .memories
            .is_empty()
    );
    fixture
        .kernel
        .admit_context_node(TrustedContextNodeDraft::session(
            "personal",
            ContextNode {
                id,
                kind: ContextNodeKind::Claim,
                summary: "different interpretation".into(),
                origin: ContextOrigin::User,
                epistemic: EpistemicStatus::Asserted,
                scope: ContextScope::Session,
                lens: ContextLens::Personal,
                confidence: 1.0,
                source_event_ids: vec![source.input_event_id],
                supersedes: vec![],
                valid_from: None,
                valid_until: None,
            },
        ))
        .unwrap();
    assert!(fixture.kernel.list_memories(query("personal")).is_err());

    let command = fixture.input("separate", "canonical text");
    let saved = fixture.kernel.remember_input(command).unwrap();
    let connection =
        rusqlite::Connection::open(fixture.config.data_dir.join("context-projection.db")).unwrap();
    connection
        .execute(
            "DELETE FROM projected_nodes WHERE node_id = ?1",
            [&saved.memory_id],
        )
        .unwrap();
    drop(connection);
    let before = fixture.kernel.event_count().unwrap();
    assert_eq!(
        fixture
            .kernel
            .list_memories(query("separate"))
            .unwrap()
            .memories[0]
            .text,
        "canonical text"
    );
    assert_eq!(fixture.kernel.event_count().unwrap(), before);
}
