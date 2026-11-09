//! The profile's standing instructions (ADR 0045): journaled, pinned before the
//! first request, and replayed against the text that was in force when the turn
//! ran rather than whatever the profile says now.
use super::agent_runs::{start_command, start_when_idle, terminal};
use super::*;

const KOREAN: &str = "Answer in Korean. Never book anything without asking first.";

/// One read-only turn that answers without calling anything.
async fn answer(
    fixture: &Fixture,
    question: &str,
) -> (ditto_protocol::AgentRunResponse, ScriptedDriver) {
    let driver = ScriptedDriver::new(vec![final_script(&["done"])]);
    let command = start_command(question);
    start_when_idle(&fixture.kernel, &command, Arc::new(driver.clone())).await;
    (terminal(&fixture.kernel, &command).await, driver)
}

fn segments(request: &ModelRequest) -> Vec<String> {
    request.stable_system_prefix.segments.clone()
}

#[test]
fn the_standing_instructions_lead_the_request_and_replay_keeps_the_old_text() {
    let fixture = Fixture::new();
    let document = fixture
        .kernel
        .set_instructions(KOREAN)
        .expect("record instructions");
    assert_eq!(document.text, KOREAN);

    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let (response, driver) = runtime.block_on(async { answer(&fixture, "hello").await });
    assert_eq!(response.status, ditto_protocol::AgentRunStatus::Unverified);

    let requests = driver.requests.lock().expect("requests").clone();
    let last = segments(&requests[0]).pop().expect("a profile segment");
    assert!(
        last.contains(KOREAN),
        "the pinned segment carries the profile's words: {last}"
    );
    assert!(
        last.contains("Standing instructions"),
        "and says whose they are"
    );

    // A later edit must not rewrite what the finished turn was told.
    fixture
        .kernel
        .set_instructions("Be brief.")
        .expect("replace instructions");
    let turn_id = response.turn_id.clone();
    let events = fixture.events_for_session("personal");
    let replay = replay_artifact_read_turn(&events, &turn_id).expect("replay");
    let replayed = replay.requests.first().expect("a replayed request");
    let mut replayed_segments = replayed.request.stable_system_prefix.segments.clone();
    let last = replayed_segments.pop().expect("a profile segment");
    assert!(
        last.contains(KOREAN),
        "replay uses the journaled text: {last}"
    );
    assert!(!last.contains("Be brief."), "never the current file's text");
}

#[test]
fn blank_instructions_pin_nothing_and_an_overlong_one_is_refused() {
    let fixture = Fixture::new();
    fixture
        .kernel
        .set_instructions("   ")
        .expect("blank is allowed");
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let (_, driver) = runtime.block_on(async { answer(&fixture, "hello").await });
    let requests = driver.requests.lock().expect("requests").clone();
    assert!(
        !segments(&requests[0])
            .iter()
            .any(|segment| segment.contains("Standing instructions")),
        "an empty profile adds no segment and costs no prefix space"
    );

    let too_long = "x".repeat(ditto_kernel::instructions::MAX_INSTRUCTIONS_CHARS + 1);
    let error = fixture
        .kernel
        .set_instructions(&too_long)
        .expect_err("overlong instructions are refused, not truncated");
    assert!(
        matches!(error, KernelError::InvalidCommand(ref message) if message.contains("2000")),
        "the refusal names the limit: {error}"
    );
}
