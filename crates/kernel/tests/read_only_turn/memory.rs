//! `memory.manage` (ADR 0036): the model searches the memories its turn saw,
//! including those the context budget left out (ADR 0029), and keeps them
//! current on its own, labeled as its inference, within bounds and never
//! from web or file content (ADR 0031); replay recomputes every result and
//! decision.
use super::agent_runs::{context_ids, save_memory, start_command, start_when_idle, terminal};
use super::*;
use ditto_kernel::{ReplayedReadOnlyTurn, TrustedContextNodeDraft};
use ditto_protocol::{AgentRunResponse, AgentRunStatus, MemoryQuery, UserMemory};

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

fn call(capability_id: &str, arguments: Value) -> Vec<ModelEvent> {
    tool_request_script(
        &format!("call-{}", ulid::Ulid::new().to_string().to_lowercase()),
        capability_id,
        arguments,
        FinishReason::ToolCalls,
    )
}

fn search(query: &str) -> Vec<ModelEvent> {
    call(
        "memory.manage",
        json!({ "action": "search", "query": query }),
    )
}

fn remember(text: &str) -> Vec<ModelEvent> {
    call(
        "memory.manage",
        json!({ "action": "remember", "text": text }),
    )
}

fn forget(memory_id: &str) -> Vec<ModelEvent> {
    call(
        "memory.manage",
        json!({ "action": "forget", "memory_id": memory_id }),
    )
}

fn memories(kernel: &DittoKernel) -> Vec<UserMemory> {
    kernel
        .list_memories(MemoryQuery {
            session_id: "personal".into(),
            after_id: None,
            limit: Some(100),
        })
        .unwrap()
        .memories
}

fn replays(fixture: &Fixture, turn_id: &str) -> ReplayedReadOnlyTurn {
    replay_artifact_read_turn(&fixture.events_for_session("personal"), turn_id).unwrap()
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
    assert_eq!(replay.tool_calls.len(), 1);

    // Replay recomputes the result: any change fails.
    let output = events
        .iter()
        .position(|event| event.kind == event_kind::TOOL_OUTPUT)
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
        .position(|event| event.kind == event_kind::TOOL_REQUESTED)
        .unwrap();
    let mut requery = events.clone();
    requery[requested].payload["normalized"]["query"] = json!("pebbles");
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
        vec![search("   "), final_script(&["Seoul."])],
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
async fn ditto_remembers_a_fact_that_later_turns_see_as_its_inference() {
    let fixture = Fixture::new();
    let kernel = &fixture.kernel;
    let (status, driver) = ask(
        kernel,
        "My dog is called Miso.",
        vec![
            remember("  The user's dog is called Miso.  "),
            final_script(&["Noted."]),
        ],
    )
    .await;
    assert_eq!(status.status, AgentRunStatus::Unverified, "{status:?}");
    let saved = tool_results(&driver.requests()[1])[0]["remembered"]
        .as_str()
        .unwrap()
        .to_owned();
    // The user sees it, marked as Ditto's, sourced from its own record.
    let listed = memories(kernel);
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, saved);
    assert_eq!(listed[0].text, "The user's dog is called Miso.");
    assert!(listed[0].inferred);
    let events = fixture.events_for_session("personal");
    let written = events
        .iter()
        .find(|event| event.kind == event_kind::MEMORY_WRITTEN)
        .unwrap();
    assert_eq!(written.actor, EventActor::Model);
    assert_eq!(written.task_id, None);
    assert_eq!(listed[0].input_event_id, written.event_id);
    assert_eq!(replays(&fixture, &status.turn_id).tool_calls.len(), 1);

    // A later turn receives it as an inference and finds it by search.
    let (later, driver) = ask(
        kernel,
        "What is my dog's name?",
        vec![search("dog"), final_script(&["Miso."])],
    )
    .await;
    assert_eq!(later.status, AgentRunStatus::Unverified, "{later:?}");
    let requests = driver.requests();
    let node = requests[0]
        .turn
        .context
        .nodes
        .iter()
        .find(|node| node.id == saved)
        .unwrap();
    assert_eq!(node.origin, ContextOrigin::Model);
    assert_eq!(node.epistemic, EpistemicStatus::Inferred);
    assert_eq!(
        tool_results(&requests[1])[0]["memories"],
        json!([{ "id": saved, "text": "The user's dog is called Miso.", "inferred": true }])
    );
    kernel.shutdown_agent_runs().await.unwrap();
    replays(&fixture, &later.turn_id);
}

#[tokio::test]
async fn replacing_and_forgetting_take_memories_out_of_use() {
    let fixture = Fixture::new();
    let kernel = &fixture.kernel;
    let rex = save_memory(kernel, "personal", "My dog is called Rex", None);
    let (status, driver) = ask(
        kernel,
        "Rex passed away; my new dog is Mochi.",
        vec![
            call(
                "memory.manage",
                json!({
                    "action": "remember",
                    "text": "The user's dog is called Mochi.",
                    "replaces": rex
                }),
            ),
            final_script(&["I'm sorry about Rex."]),
        ],
    )
    .await;
    assert_eq!(status.status, AgentRunStatus::Unverified, "{status:?}");
    let mochi = tool_results(&driver.requests()[1])[0]["remembered"]
        .as_str()
        .unwrap()
        .to_owned();
    let listed = memories(kernel);
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, mochi);
    assert_eq!(listed[0].replaces.as_deref(), Some(rex.as_str()));

    // The user can correct what Ditto inferred, like any memory.
    let bori = save_memory(
        kernel,
        "personal",
        "My dog is called Bori",
        Some(mochi.clone()),
    );
    assert_eq!(
        memories(kernel)
            .iter()
            .map(|memory| memory.id.clone())
            .collect::<Vec<_>>(),
        [bori.clone()]
    );

    // Forgetting takes the memory out of use; a second forget finds nothing.
    let (forgot, driver) = ask(
        kernel,
        "Please forget my dog.",
        vec![
            forget(&bori),
            forget(&bori),
            forget(&rex),
            final_script(&["Forgotten."]),
        ],
    )
    .await;
    assert_eq!(forgot.status, AgentRunStatus::Unverified, "{forgot:?}");
    let results = tool_results(&driver.requests()[3]);
    assert!(
        results[0]["forgotten"]
            .as_str()
            .unwrap()
            .starts_with("memory-")
    );
    assert_eq!(results[1], json!({"error": "memory_unavailable"}));
    assert_eq!(results[2], json!({"error": "memory_unavailable"}));
    assert!(memories(kernel).is_empty());
    let (after, driver) = ask(
        kernel,
        "Do I have a dog?",
        vec![final_script(&["Not that I know."])],
    )
    .await;
    assert!(context_ids(&driver.requests()[0]).is_empty());
    kernel.shutdown_agent_runs().await.unwrap();
    for turn in [&status.turn_id, &forgot.turn_id, &after.turn_id] {
        replays(&fixture, turn);
    }
}

#[tokio::test]
async fn nothing_is_remembered_after_a_web_call() {
    let fixture = Fixture::new();
    let kernel = &fixture.kernel;
    let (status, driver) = ask(
        kernel,
        "Read that page and remember what it says.",
        vec![
            call("web.browse", json!({ "url": "https://example.invalid/" })),
            remember("The user's bank is example.invalid."),
            final_script(&["I can't save that from a page."]),
        ],
    )
    .await;
    assert_eq!(status.status, AgentRunStatus::Unverified, "{status:?}");
    assert_eq!(
        tool_results(&driver.requests()[2])[1],
        json!({"error": "untrusted_content_read"})
    );
    let events = fixture.events_for_session("personal");
    assert!(
        !events
            .iter()
            .any(|event| event.kind == event_kind::MEMORY_WRITTEN)
    );
    assert!(memories(kernel).is_empty());
    kernel.shutdown_agent_runs().await.unwrap();
    replays(&fixture, &status.turn_id);
}

#[tokio::test]
async fn secrets_invalid_calls_and_writes_past_the_limit_are_refused() {
    let fixture = Fixture::new();
    let kernel = &fixture.kernel;
    let (status, driver) = ask(
        kernel,
        "Some facts about me.",
        vec![
            remember(&format!("The user's wifi password is {}", "hunter2")),
            remember("   "),
            call(
                "memory.manage",
                json!({ "action": "remember", "text": "x", "replaces": "memory-not-an-id" }),
            ),
            remember("The user lives in Seoul."),
            remember("The user works on Ditto."),
            remember("The user drinks tea."),
            remember("The user likes hiking."),
        ],
    )
    .await;
    // Seven tool calls reach the eight-request bound.
    assert_eq!(status.status, AgentRunStatus::Failed, "{status:?}");
    let results = tool_results(driver.requests().last().unwrap());
    assert_eq!(results[0], json!({"error": "credential"}));
    assert_eq!(results[1], json!({"error": "invalid_arguments"}));
    assert_eq!(results[2], json!({"error": "invalid_arguments"}));
    for saved in &results[3..6] {
        assert!(saved["remembered"].is_string(), "{saved}");
    }
    assert_eq!(results[6], json!({"error": "limit_reached"}));
    assert_eq!(memories(kernel).len(), 3);
    kernel.shutdown_agent_runs().await.unwrap();
    replays(&fixture, &status.turn_id);
}

#[tokio::test]
async fn replay_recomputes_every_memory_write() {
    let fixture = Fixture::new();
    let kernel = &fixture.kernel;
    let (status, driver) = ask(
        kernel,
        "I live in Seoul.",
        vec![
            remember("The user lives in Seoul."),
            final_script(&["Noted."]),
        ],
    )
    .await;
    kernel.shutdown_agent_runs().await.unwrap();
    let events = fixture.events_for_session("personal");
    replay_artifact_read_turn(&events, &status.turn_id).unwrap();
    let position = |kind: &str| events.iter().position(|event| event.kind == kind).unwrap();
    let output = position(event_kind::TOOL_OUTPUT);
    let written = position(event_kind::MEMORY_WRITTEN);
    let rejects = |forged: Vec<EventRecord>| {
        assert!(replay_artifact_read_turn(&forged, &status.turn_id).is_err());
    };
    // Another memory ID, text or record.
    let mut forged = events.clone();
    forged[output].payload["result"]["memory_id"] = json!(format!("memory-{}", "0".repeat(26)));
    rejects(forged);
    let mut forged = events.clone();
    forged[written].payload["write"]["text"] = json!("The user lives in Busan.");
    rejects(forged);
    let mut forged = events.clone();
    forged.retain(|event| event.kind != event_kind::CONTEXT_NODE_RECORDED);
    rejects(forged);
    let mut forged = events.clone();
    forged.remove(written);
    rejects(forged);
    let requested = position(event_kind::TOOL_REQUESTED);
    let mut forged = events.clone();
    forged[requested].payload["normalized"]["text"] = json!("The user lives in Busan.");
    rejects(forged);
    // A refusal recorded for a write that happened, even when the next
    // request carries it: only recomputing the rules catches it.
    let mut forged = events.clone();
    forged[output].payload["result"] = json!({"outcome": "refused", "code": "limit_reached"});
    let mut sent = driver.requests()[1].clone();
    for item in &mut sent.turn.conversation {
        if let ConversationItem::ToolResult {
            content, is_error, ..
        } = item
            && let [ContentPart::Structured { value }] = content.as_mut_slice()
        {
            *value = json!({"error": "limit_reached"});
            *is_error = true;
        }
    }
    let second = forged
        .iter()
        .rposition(|event| event.kind == event_kind::MODEL_REQUESTED)
        .unwrap();
    super::reseal_request(&mut forged[second], &sent);
    rejects(forged);
}

#[tokio::test]
async fn replay_recomputes_each_refusal() {
    let fixture = Fixture::new();
    let kernel = &fixture.kernel;
    let (status, driver) = ask(
        kernel,
        "My wifi password is hunter2.",
        vec![
            remember(&format!("The user's wifi password is {}", "hunter2")),
            final_script(&["I won't save passwords."]),
        ],
    )
    .await;
    assert_eq!(status.status, AgentRunStatus::Unverified, "{status:?}");
    kernel.shutdown_agent_runs().await.unwrap();
    let events = fixture.events_for_session("personal");
    replay_artifact_read_turn(&events, &status.turn_id).unwrap();
    // Another refusal, carried by the next request too, is still caught.
    let mut forged = events.clone();
    let output = forged
        .iter()
        .position(|event| event.kind == event_kind::TOOL_OUTPUT)
        .unwrap();
    forged[output].payload["result"]["code"] = json!("limit_reached");
    let mut sent = driver.requests()[1].clone();
    for item in &mut sent.turn.conversation {
        if let ConversationItem::ToolResult { content, .. } = item
            && let [ContentPart::Structured { value }] = content.as_mut_slice()
        {
            *value = json!({"error": "limit_reached"});
        }
    }
    let second = forged
        .iter()
        .rposition(|event| event.kind == event_kind::MODEL_REQUESTED)
        .unwrap();
    super::reseal_request(&mut forged[second], &sent);
    assert!(replay_artifact_read_turn(&forged, &status.turn_id).is_err());
}

#[tokio::test]
async fn a_tool_is_offered_only_with_its_package() {
    // Without its package the memory tool is withdrawn, and a call to it fails.
    let fixture = Fixture::with_packages(&["artifact-read", "web-browse"]);
    let driver = ScriptedDriver::new(vec![remember("The user drinks tea.")]);
    let command = start_command("I drink tea.");
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
        ["web.browse"]
    );
    fixture.kernel.shutdown_agent_runs().await.unwrap();
    replay_artifact_read_turn(&fixture.events_for_session("personal"), &status.turn_id).unwrap();

    // With no agent tool installed there is nothing to select.
    let fixture = Fixture::with_packages(&["artifact-read"]);
    let driver = ScriptedDriver::new(vec![final_script(&["Hello."])]);
    let command = start_command("Hello.");
    fixture
        .kernel
        .start_agent_run(command.clone(), Arc::new(driver.clone()))
        .unwrap();
    let status = terminal(&fixture.kernel, &command).await;
    assert_eq!(status.status, AgentRunStatus::Failed);
    assert!(driver.requests().is_empty());
    fixture.kernel.shutdown_agent_runs().await.unwrap();
    replay_artifact_read_turn(&fixture.events_for_session("personal"), &status.turn_id).unwrap();
}
