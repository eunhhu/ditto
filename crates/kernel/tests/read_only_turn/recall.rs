//! `memory.search` (ADR 0029): the model searches the memories its turn saw,
//! including those the context budget left out, and replay recomputes every
//! result.
use super::agent_runs::{context_ids, save_memory, start_command, start_when_idle, terminal};
use super::*;
use ditto_kernel::TrustedContextNodeDraft;
use ditto_protocol::{AgentRunResponse, AgentRunStatus};

fn tool_results(request: &ModelRequest) -> Vec<Value> {
    request
        .turn
        .conversation
        .iter()
        .filter_map(|item| match item {
            ConversationItem::ToolResult { content, .. } => {
                content.iter().find_map(|part| match part {
                    ContentPart::Structured { value } => Some(value.clone()),
                    _ => None,
                })
            }
            _ => None,
        })
        .collect()
}

/// One agent turn in session `personal` whose requests follow `scripts`.
async fn ask(
    kernel: &DittoKernel,
    question: &str,
    scripts: Vec<Vec<ModelEvent>>,
) -> (AgentRunResponse, ScriptedDriver) {
    let driver = ScriptedDriver::new(scripts);
    let command = start_command(question);
    start_when_idle(kernel, &command, Arc::new(driver.clone())).await;
    (terminal(kernel, &command).await, driver)
}

fn search(query: &str) -> Vec<ModelEvent> {
    tool_request_script(
        "recall-1",
        "memory.search",
        json!({ "query": query }),
        FinishReason::ToolCalls,
    )
}

/// Enough unrelated memories that the complete set no longer fits the context
/// budget, so selection falls back to lexical overlap with the question.
fn fill_past_the_budget(kernel: &DittoKernel) {
    for index in 0..40 {
        save_memory(
            kernel,
            "personal",
            &format!("synthetic filler fact number {index:02} about pebbles and moss"),
            None,
        );
    }
}

#[tokio::test]
async fn a_search_finds_a_memory_the_context_budget_left_out() {
    let fixture = Fixture::new();
    let kernel = &fixture.kernel;
    let miso = save_memory(kernel, "personal", "My dog is called Miso", None);
    let old = save_memory(kernel, "personal", "My dog is called Rex", None);
    let corrected = save_memory(
        kernel,
        "personal",
        "My dog is called Mochi",
        Some(old.clone()),
    );
    save_memory(kernel, "elsewhere", "My dog is called Private", None);
    fill_past_the_budget(kernel);

    // "What is my pet's name?" shares no word with the dog memories.
    let (status, driver) = ask(
        kernel,
        "What is my pet's name?",
        vec![search("dog called"), final_script(&["Miso and Mochi."])],
    )
    .await;
    assert_eq!(status.status, AgentRunStatus::Unverified, "{status:?}");
    let requests = driver.requests();
    assert!(!context_ids(&requests[0]).contains(&miso));
    let found = &tool_results(&requests[1])[0];
    let ids = found["memories"]
        .as_array()
        .unwrap()
        .iter()
        .map(|memory| memory["id"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    // Equal scores rank by ID; superseded and other-session memories are
    // never searched.
    assert_eq!(ids, [miso.clone(), corrected.clone()]);
    assert_eq!(found["searched"], json!(42));
    assert_eq!(
        found["content_origin"],
        json!(
            "memories Ditto keeps for the user: what they asked Ditto to remember, and what Ditto inferred (marked inferred)"
        )
    );
    kernel.shutdown_agent_runs().await.unwrap();

    let events = fixture.events_for_session("personal");
    let replay = replay_artifact_read_turn(&events, &status.turn_id).unwrap();
    assert_eq!(replay.recall_calls.len(), 1);

    // Replay recomputes the result: any change fails.
    let output = events
        .iter()
        .position(|event| event.kind == event_kind::AGENT_MEMORY_OUTPUT)
        .unwrap();
    let mut dropped = events.clone();
    dropped[output].payload["result"]["memories"]
        .as_array_mut()
        .unwrap()
        .pop();
    assert!(replay_artifact_read_turn(&dropped, &status.turn_id).is_err());
    let mut reordered = events.clone();
    reordered[output].payload["result"]["memories"]
        .as_array_mut()
        .unwrap()
        .reverse();
    assert!(replay_artifact_read_turn(&reordered, &status.turn_id).is_err());
    let mut invented = events.clone();
    invented[output].payload["result"]["memories"][0]["text"] = json!("My dog is called Rex");
    assert!(replay_artifact_read_turn(&invented, &status.turn_id).is_err());
    let mut widened = events.clone();
    widened[output].payload["result"]["searched"] = json!(43);
    assert!(replay_artifact_read_turn(&widened, &status.turn_id).is_err());
    // A changed result that the next request's digest also carries: only
    // recomputing the search catches it.
    let mut consistent = events.clone();
    consistent[output].payload["result"]["memories"]
        .as_array_mut()
        .unwrap()
        .pop();
    let mut sent = requests[1].clone();
    for item in &mut sent.turn.conversation {
        if let ConversationItem::ToolResult { content, .. } = item
            && let [ContentPart::Structured { value }] = content.as_mut_slice()
        {
            value["memories"].as_array_mut().unwrap().pop();
        }
    }
    let second = events
        .iter()
        .rposition(|event| event.kind == event_kind::MODEL_REQUESTED)
        .unwrap();
    super::reseal_request(&mut consistent[second], &sent);
    assert!(replay_artifact_read_turn(&consistent, &status.turn_id).is_err());
    let requested = events
        .iter()
        .position(|event| event.kind == event_kind::AGENT_MEMORY_REQUESTED)
        .unwrap();
    let mut requery = events.clone();
    requery[requested].payload["query"] = json!("pebbles");
    assert!(replay_artifact_read_turn(&requery, &status.turn_id).is_err());
}

#[tokio::test]
async fn a_search_returns_only_what_the_user_asserted() {
    let fixture = Fixture::new();
    let kernel = &fixture.kernel;
    let miso = save_memory(kernel, "personal", "My dog is called Miso", None);
    let source = kernel
        .record_user_input(SubmitInputCommand {
            text: "the dog".into(),
            session_id: Some("personal".into()),
            task_id: None,
        })
        .unwrap();
    kernel
        .admit_context_node(TrustedContextNodeDraft::session(
            "personal",
            ContextNode {
                id: "inferred".into(),
                kind: ContextNodeKind::Claim,
                summary: "My dog is called Rex".into(),
                origin: ContextOrigin::User,
                epistemic: EpistemicStatus::Inferred,
                scope: ContextScope::Session,
                lens: ContextLens::Personal,
                confidence: 0.5,
                source_event_ids: vec![source.event_id.clone()],
                supersedes: Vec::new(),
                valid_from: None,
                valid_until: None,
            },
        ))
        .unwrap();
    let (status, driver) = ask(
        kernel,
        "What is my dog called?",
        vec![search("dog called"), final_script(&["Miso."])],
    )
    .await;
    assert_eq!(status.status, AgentRunStatus::Unverified, "{status:?}");
    let requests = driver.requests();
    // The capsule carries both nodes with their epistemic status, but the
    // search result, read as the user's own words, holds only what they said.
    assert_eq!(context_ids(&requests[0]).len(), 2);
    assert_eq!(
        tool_results(&requests[1])[0]["memories"],
        json!([{ "id": miso, "text": "My dog is called Miso" }])
    );
    assert_eq!(tool_results(&requests[1])[0]["searched"], json!(1));
    kernel.shutdown_agent_runs().await.unwrap();
    replay_artifact_read_turn(&fixture.events_for_session("personal"), &status.turn_id).unwrap();
}

#[tokio::test]
async fn invalid_arguments_return_an_error_the_model_reads() {
    let fixture = Fixture::new();
    let kernel = &fixture.kernel;
    save_memory(kernel, "personal", "I live in Seoul", None);
    let (status, driver) = ask(
        kernel,
        "Where do I live?",
        vec![
            tool_request_script(
                "recall-1",
                "memory.search",
                json!({ "query": "   " }),
                FinishReason::ToolCalls,
            ),
            final_script(&["Seoul."]),
        ],
    )
    .await;
    assert_eq!(status.status, AgentRunStatus::Unverified, "{status:?}");
    assert_eq!(
        tool_results(&driver.requests()[1])[0],
        json!({"error": "invalid_arguments"})
    );
    kernel.shutdown_agent_runs().await.unwrap();
    replay_artifact_read_turn(&fixture.events_for_session("personal"), &status.turn_id).unwrap();
}

#[tokio::test]
async fn only_agent_runs_with_the_installed_package_are_offered_the_search() {
    // Without the package the tool is withdrawn, and a call to it fails.
    let fixture = Fixture::artifact_read_only();
    let driver = ScriptedDriver::new(vec![search("anything")]);
    let command = start_command("Remember anything?");
    fixture
        .kernel
        .start_agent_run(command.clone(), Arc::new(driver.clone()))
        .unwrap();
    let status = terminal(&fixture.kernel, &command).await;
    assert_eq!(status.status, AgentRunStatus::Failed);
    assert_eq!(status.failure_code.as_deref(), Some("protocol"));
    assert_eq!(
        driver.requests()[0]
            .tools
            .iter()
            .map(|tool| tool.id.as_str())
            .collect::<Vec<_>>(),
        ["artifact.read"]
    );
    fixture.kernel.shutdown_agent_runs().await.unwrap();
    replay_artifact_read_turn(&fixture.events_for_session("personal"), &status.turn_id).unwrap();
}
