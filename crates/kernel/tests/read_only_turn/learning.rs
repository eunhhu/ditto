//! Retrieval keys learned from the model's own searches (ADR 0050): two
//! differently worded misses teach a key, the next turn's first request holds
//! the memory without a search, every turn replays, and the user's undo takes
//! the key out of use.
use super::agent_runs::{context_ids, save_memory, start_command, start_when_idle, terminal};
use super::memory::tool_results;
use super::*;
use ditto_protocol::{
    AgentRunResponse, AgentRunStatus, LearnedState, LearningLedger, LearningQuery,
    RetireLearnedCommand,
};

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

fn search(queries: &[&str]) -> Vec<ModelEvent> {
    call(
        "memory.manage",
        json!({ "action": "search", "queries": queries }),
    )
}

/// More memories than the context budget holds, so selection falls back to
/// word overlap and a paraphrase misses.
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

fn ledger(kernel: &DittoKernel) -> LearningLedger {
    kernel
        .learning_ledger(LearningQuery {
            session_id: "personal".into(),
        })
        .unwrap()
}

fn learning_events(fixture: &Fixture) -> Vec<EventRecord> {
    fixture
        .events_for_session("personal")
        .into_iter()
        .filter(|event| event.kind.starts_with("improvement."))
        .collect()
}

/// Two misses, each repaired by a search, worded differently.
async fn teach(fixture: &Fixture) -> String {
    let kernel = &fixture.kernel;
    fill_past_the_budget(kernel);
    let birch = save_memory(
        kernel,
        "personal",
        "Our book club now gathers in Birch Room",
        None,
    );

    let (first, driver) = ask(
        kernel,
        "Where should I go for the reading group get-together?",
        vec![
            search(&["reading group", "book club"]),
            final_script(&["Birch Room."]),
        ],
    )
    .await;
    assert_eq!(first.status, AgentRunStatus::Unverified, "{first:?}");
    assert!(
        !context_ids(&driver.requests()[0]).contains(&birch),
        "the paraphrase misses the memory, so the turn searches"
    );
    let keys = ledger(kernel).keys;
    assert_eq!(keys.len(), 1, "{keys:?}");
    assert_eq!(keys[0].state, LearnedState::Candidate);
    assert_eq!(keys[0].target, birch);

    // The same words again are the same episode: nothing is promoted.
    ask(
        kernel,
        "Where should I go for the reading group get-together?",
        vec![
            search(&["reading group", "book club"]),
            final_script(&["Birch Room."]),
        ],
    )
    .await;
    assert_eq!(ledger(kernel).keys[0].state, LearnedState::Candidate);
    assert_eq!(learning_events(fixture).len(), 1);

    let (second, _) = ask(
        kernel,
        "Remind me where the reading group gets together these days",
        vec![
            search(&["reading group", "book club"]),
            final_script(&["In Birch Room."]),
        ],
    )
    .await;
    assert_eq!(second.status, AgentRunStatus::Unverified, "{second:?}");
    let keys = ledger(kernel).keys;
    assert_eq!(keys[0].state, LearnedState::Active, "{keys:?}");
    assert_eq!(keys[0].evidence.len(), 2);
    assert!(keys[0].words.contains(&"reading".to_owned()), "{keys:?}");
    birch
}

#[tokio::test]
async fn two_repaired_misses_teach_a_key_the_next_turn_uses_without_a_search() {
    let fixture = Fixture::new();
    let kernel = &fixture.kernel;
    let birch = teach(&fixture).await;

    // One request, no search: the memory is already in the capsule.
    let (third, driver) = ask(
        kernel,
        "Is the reading group still meeting tonight?",
        vec![final_script(&["Yes, in Birch Room."])],
    )
    .await;
    assert_eq!(third.status, AgentRunStatus::Unverified, "{third:?}");
    assert_eq!(driver.requests().len(), 1);
    assert!(context_ids(&driver.requests()[0]).contains(&birch));
    let key = ledger(kernel).keys.remove(0);
    assert_eq!(key.uses, 1, "{key:?}");

    // Every turn replays, the one that used the key included.
    fixture.kernel.shutdown_agent_runs().await.unwrap();
    let events = fixture.events_for_session("personal");
    let turns = events
        .iter()
        .filter(|event| event.kind == event_kind::TURN_FINISHED)
        .map(|event| event.correlation_id.clone().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(turns.len(), 4);
    for turn in &turns {
        replay_artifact_read_turn(&events, turn).unwrap();
    }
    let compiled = events
        .iter()
        .rev()
        .find(|event| event.kind == event_kind::CONTEXT_COMPILED)
        .unwrap();
    let reason = compiled.payload["compiled"]["receipt"]["included"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["node_id"] == birch)
        .unwrap()["reason"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        reason,
        format!("policy-required: learned key {}", key.event_id)
    );
}

#[tokio::test]
async fn an_undone_key_stops_bringing_its_memory_in() {
    let fixture = Fixture::new();
    let kernel = &fixture.kernel;
    let birch = teach(&fixture).await;
    let key = ledger(kernel).keys.remove(0);
    let retired = kernel
        .retire_learned_key(RetireLearnedCommand {
            session_id: "personal".into(),
            key_id: key.key_id.clone(),
        })
        .unwrap();
    assert_eq!(retired.state, LearnedState::Retired);
    let retirement = learning_events(&fixture).pop().unwrap();
    assert_eq!(retirement.kind, event_kind::IMPROVEMENT_RETIRED);
    assert_eq!(retirement.actor, EventActor::User);

    let (_, driver) = ask(
        kernel,
        "Is the reading group still meeting tonight?",
        vec![
            search(&["reading group", "book club"]),
            final_script(&["Yes, in Birch Room."]),
        ],
    )
    .await;
    assert!(!context_ids(&driver.requests()[0]).contains(&birch));
    // The repaired miss does not teach the undone key back.
    assert_eq!(ledger(kernel).keys[0].state, LearnedState::Retired);
    assert!(
        kernel
            .retire_learned_key(RetireLearnedCommand {
                session_id: "personal".into(),
                key_id: key.key_id,
            })
            .is_err(),
        "a retired key cannot be retired twice"
    );
}

#[tokio::test]
async fn a_turn_that_read_the_web_teaches_nothing() {
    let fixture = Fixture::new();
    let kernel = &fixture.kernel;
    fill_past_the_budget(kernel);
    save_memory(
        kernel,
        "personal",
        "Our book club now gathers in Birch Room",
        None,
    );
    let (status, driver) = ask(
        kernel,
        "Where should I go for the reading group get-together?",
        vec![
            call("web.browse", json!({ "url": "https://example.com/venues" })),
            search(&["reading group", "book club"]),
            final_script(&["Birch Room."]),
        ],
    )
    .await;
    assert_eq!(status.status, AgentRunStatus::Unverified, "{status:?}");
    let results = tool_results(&driver.requests()[2]);
    let found = results
        .iter()
        .find(|result| result.get("memories").is_some())
        .expect("the memory search ran after the web call");
    assert_eq!(found["memories"].as_array().unwrap().len(), 1);
    assert!(learning_events(&fixture).is_empty());
}

#[tokio::test]
async fn a_receipt_citing_a_key_the_journal_does_not_back_does_not_replay() {
    let fixture = Fixture::new();
    let kernel = &fixture.kernel;
    teach(&fixture).await;
    let (_, _) = ask(
        kernel,
        "Is the reading group still meeting tonight?",
        vec![final_script(&["Yes, in Birch Room."])],
    )
    .await;
    fixture.kernel.shutdown_agent_runs().await.unwrap();
    let events = fixture.events_for_session("personal");
    let turn = events
        .iter()
        .rev()
        .find(|event| event.kind == event_kind::TURN_FINISHED)
        .unwrap()
        .correlation_id
        .clone()
        .unwrap();
    replay_artifact_read_turn(&events, &turn).unwrap();

    let promotion = |event: &EventRecord| event.kind == event_kind::IMPROVEMENT_PROMOTED;
    // The promotion is missing.
    let missing = events
        .iter()
        .filter(|event| !promotion(event))
        .cloned()
        .collect::<Vec<_>>();
    assert!(replay_artifact_read_turn(&missing, &turn).is_err());
    // The promotion belongs to another session.
    let mut moved = events.clone();
    for event in moved.iter_mut().filter(|event| promotion(event)) {
        event.session_id = Some("someone-else".into());
    }
    assert!(replay_artifact_read_turn(&moved, &turn).is_err());
    // The promotion was retired before the turn.
    let mut retired = events.clone();
    let cited = retired
        .iter()
        .find(|event| promotion(event))
        .unwrap()
        .clone();
    let mut retirement = cited.clone();
    retirement.event_id = ulid::Ulid::new().to_string();
    retirement.kind = event_kind::IMPROVEMENT_RETIRED.into();
    retirement.payload["supersedes"] = json!(cited.event_id);
    retirement
        .payload
        .as_object_mut()
        .unwrap()
        .remove("expires_at");
    let position = retired
        .iter()
        .position(|event| event.event_id == cited.event_id)
        .unwrap();
    retired.insert(position + 1, retirement);
    assert!(replay_artifact_read_turn(&retired, &turn).is_err());
}
