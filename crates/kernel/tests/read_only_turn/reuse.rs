//! State kept between turns (ADR 0028 Phase B): reused context and kept
//! threads must equal what a fresh read of the journal gives, whoever wrote it.
use super::agent_runs::{ask, context_ids, message_texts, save_memory, start_command, terminal};
use super::*;
use ditto_context_projection::ContextNodeRecordedPayloadV1;
use ditto_kernel::TrustedContextNodeDraft;
use ditto_protocol::AgentRunStatus;

fn node(id: &str, source: &EventRecord, scope: ContextScope) -> ContextNode {
    ContextNode {
        id: id.into(),
        kind: ContextNodeKind::Claim,
        summary: format!("{id} summary"),
        origin: ContextOrigin::User,
        epistemic: EpistemicStatus::Asserted,
        scope,
        lens: ContextLens::Personal,
        confidence: 1.0,
        source_event_ids: vec![source.event_id.clone()],
        supersedes: Vec::new(),
        valid_from: None,
        valid_until: None,
    }
}

fn user_input(kernel: &DittoKernel, text: &str) -> EventRecord {
    kernel
        .record_user_input(SubmitInputCommand {
            text: text.into(),
            session_id: Some("personal".into()),
            task_id: None,
        })
        .unwrap()
}

/// A second writer on the same journal, outside this kernel.
fn other_writer(fixture: &Fixture) -> EventStore {
    EventStore::open(fixture.config.data_dir.join("state.db")).unwrap()
}

async fn ask_ids(kernel: &DittoKernel, question: &str) -> Vec<String> {
    let (status, driver) = ask(kernel, "personal", question, final_script(&["Ok."])).await;
    assert_eq!(status.status, AgentRunStatus::Unverified, "{question}");
    context_ids(&driver.requests()[0])
}

#[tokio::test]
async fn reused_context_follows_every_committed_context_node() {
    let fixture = Fixture::new();
    let kernel = &fixture.kernel;
    let seoul = save_memory(kernel, "personal", "I live in Seoul", None);
    assert_eq!(
        ask_ids(kernel, "Where do I live?").await,
        vec![seoul.clone()]
    );
    assert_eq!(ask_ids(kernel, "Again?").await, vec![seoul.clone()]);

    // A new memory and a correction, each committed after the context was
    // taken, reach the next turn.
    let dog = save_memory(kernel, "personal", "My dog is Miso", None);
    assert_eq!(
        ask_ids(kernel, "My dog?").await,
        vec![seoul.clone(), dog.clone()]
    );
    let busan = save_memory(kernel, "personal", "I live in Busan", Some(seoul.clone()));
    assert_eq!(
        ask_ids(kernel, "Where now?").await,
        vec![dog.clone(), busan.clone()]
    );

    // So does a node another writer appended straight to the journal.
    let journal = other_writer(&fixture);
    let source = journal
        .append(NewEvent::user_input(
            "personal",
            None::<String>,
            "a fact from elsewhere",
        ))
        .unwrap();
    journal
        .append(NewEvent {
            session_id: Some("personal".into()),
            task_id: None,
            actor: EventActor::System,
            kind: event_kind::CONTEXT_NODE_RECORDED.into(),
            payload: serde_json::to_value(ContextNodeRecordedPayloadV1::new(node(
                "external-fact",
                &source,
                ContextScope::Session,
            )))
            .unwrap(),
            causation_id: Some(source.event_id.clone()),
            correlation_id: Some("personal".into()),
            span_id: None,
        })
        .unwrap();
    assert_eq!(
        ask_ids(kernel, "Anything new?").await,
        vec!["external-fact".to_owned(), dog, busan]
    );
    kernel.shutdown_agent_runs().await.unwrap();
}

#[tokio::test]
async fn task_scoped_context_never_reaches_another_run() {
    let fixture = Fixture::new();
    let kernel = &fixture.kernel;
    let tea = save_memory(kernel, "personal", "I like tea", None);
    // Taken while the session holds only session-wide nodes.
    assert_eq!(ask_ids(kernel, "Drinks?").await, vec![tea.clone()]);

    let pinned = user_input(kernel, "pinned for one run");
    let command = start_command("What do I like?");
    let task = format!("run_{}", command.request_id);
    kernel
        .admit_context_node(TrustedContextNodeDraft::task(
            "personal",
            task,
            node("run-pinned", &pinned, ContextScope::Task),
        ))
        .unwrap();
    let driver = ScriptedDriver::new(vec![final_script(&["Tea."])]);
    kernel
        .start_agent_run(command.clone(), Arc::new(driver.clone()))
        .unwrap();
    assert_eq!(
        terminal(kernel, &command).await.status,
        AgentRunStatus::Unverified
    );
    assert_eq!(
        context_ids(&driver.requests()[0]),
        vec![tea.clone(), "run-pinned".to_owned()]
    );

    // Another run's context never carries the pinned node.
    assert_eq!(ask_ids(kernel, "And now?").await, vec![tea.clone()]);
    assert_eq!(ask_ids(kernel, "Once more?").await, vec![tea]);
    kernel.shutdown_agent_runs().await.unwrap();
}

#[tokio::test]
async fn windowed_context_is_evaluated_at_each_turn() {
    let fixture = Fixture::new();
    let kernel = &fixture.kernel;
    let tea = save_memory(kernel, "personal", "I like tea", None);
    let source = user_input(kernel, "a changing week");
    let boundary = chrono::DateTime::from_timestamp_millis(
        (Utc::now() + ChronoDuration::milliseconds(1_000)).timestamp_millis(),
    )
    .unwrap();
    for (id, from, until) in [
        ("ends-soon", None, Some(boundary)),
        ("starts-soon", Some(boundary), None),
    ] {
        kernel
            .admit_context_node(TrustedContextNodeDraft::session(
                "personal",
                ContextNode {
                    valid_from: from,
                    valid_until: until,
                    ..node(id, &source, ContextScope::Session)
                },
            ))
            .unwrap();
    }
    assert_eq!(
        ask_ids(kernel, "What now?").await,
        vec!["ends-soon".to_owned(), tea.clone()]
    );
    let wait = (boundary - Utc::now()).to_std().unwrap_or_default();
    tokio::time::sleep(wait + std::time::Duration::from_millis(50)).await;
    // No context node was committed in between, yet the active set changed.
    assert_eq!(
        ask_ids(kernel, "And later?").await,
        vec![tea, "starts-soon".to_owned()]
    );
    kernel.shutdown_agent_runs().await.unwrap();
}

#[tokio::test]
async fn kept_thread_follows_a_reset_written_by_another_writer() {
    let fixture = Fixture::new();
    let kernel = &fixture.kernel;
    ask(
        kernel,
        "personal",
        "My dog is Miso.",
        final_script(&["Noted."]),
    )
    .await;
    let (_, driver) = ask(
        kernel,
        "personal",
        "And my cat?",
        final_script(&["Unknown."]),
    )
    .await;
    assert_eq!(message_texts(&driver.requests()[0]).len(), 3);

    other_writer(&fixture)
        .append(NewEvent {
            session_id: Some("personal".into()),
            task_id: None,
            actor: EventActor::User,
            kind: event_kind::CONVERSATION_RESET.into(),
            payload: json!({"version": 1}),
            causation_id: None,
            correlation_id: None,
            span_id: None,
        })
        .unwrap();
    let (status, driver) = ask(kernel, "personal", "Fresh start?", final_script(&["Yes."])).await;
    assert_eq!(message_texts(&driver.requests()[0]).len(), 1);
    let (_, driver) = ask(kernel, "personal", "Still fresh?", final_script(&["Yes."])).await;
    assert_eq!(message_texts(&driver.requests()[0]).len(), 3);
    kernel.shutdown_agent_runs().await.unwrap();
    // Replay recomputes the same window from the journal alone.
    replay_artifact_read_turn(&fixture.events_for_session("personal"), &status.turn_id).unwrap();
}

#[tokio::test]
async fn tool_contracts_are_paged_once_per_process() {
    let fixture = Fixture::new();
    for question in ["one", "two", "three"] {
        ask_ids(&fixture.kernel, question).await;
    }
    // web.browse and memory.manage, each paged by the first turn only.
    assert_eq!(fixture.kernel.capability_load_metrics().manifests_paged, 2);
    fixture.kernel.shutdown_agent_runs().await.unwrap();
}

/// On a multi-thread runtime a run journals only from the blocking pool:
/// every worker is marked, so a journal access on one fails the run.
#[test]
#[cfg(debug_assertions)]
fn multi_thread_runs_never_touch_the_journal_on_async_workers() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .on_thread_park(ditto_kernel::mark_async_runtime_thread)
        .build()
        .unwrap();
    runtime.block_on(async {
        let fixture = Fixture::new();
        let kernel = &fixture.kernel;
        let tea = save_memory(kernel, "personal", "I like tea", None);
        for question in ["Drinks?", "Again?"] {
            assert_eq!(ask_ids(kernel, question).await, vec![tea.clone()]);
        }
        kernel.shutdown_agent_runs().await.unwrap();
    });
}
