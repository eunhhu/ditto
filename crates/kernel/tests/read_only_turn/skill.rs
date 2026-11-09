//! `skill.read` (ADR 0046): the catalogue names a skill, the model asks for one
//! body, and replay agrees with what the journal recorded.
use super::agent_runs::{start_command, start_when_idle, terminal};
use super::*;

fn read(name: &str) -> Vec<ModelEvent> {
    tool_request_script(
        &format!("call-{}", ulid::Ulid::new().to_string().to_lowercase()),
        "skill.read",
        json!({ "name": name }),
        FinishReason::ToolCalls,
    )
}

fn install(fixture: &Fixture, name: &str, description: &str, body: &str) {
    let directory = fixture.config.data_dir.join("skills").join(name);
    std::fs::create_dir_all(&directory).expect("skill dir");
    std::fs::write(
        directory.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: {description}\n---\n{body}"),
    )
    .expect("skill file");
}

async fn ask(
    fixture: &Fixture,
    question: &str,
    scripts: Vec<Vec<ModelEvent>>,
) -> (ditto_protocol::AgentRunResponse, ScriptedDriver) {
    let driver = ScriptedDriver::new(scripts);
    let command = start_command(question);
    start_when_idle(&fixture.kernel, &command, Arc::new(driver.clone())).await;
    (terminal(&fixture.kernel, &command).await, driver)
}

fn bodies(driver: &ScriptedDriver, request: usize) -> Vec<String> {
    driver.requests()[request]
        .turn
        .conversation
        .iter()
        .filter_map(|item| match item {
            ConversationItem::ToolResult { content, .. } => Some(content.clone()),
            _ => None,
        })
        .flatten()
        .filter_map(|part| match part {
            ContentPart::Structured { value } => value
                .get("body")
                .and_then(|body| body.as_str())
                .map(str::to_owned),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_body_arrives_only_when_the_model_asks_for_it_by_name() {
    let fixture = Fixture::with_packages(&["artifact-read", "skill-read"]);
    install(
        &fixture,
        "photo-triage",
        "Sort a folder of photos into dated albums.",
        "# Steps\n\nGroup by EXIF date.\n",
    );

    let (response, driver) = ask(
        &fixture,
        "Tidy my photos.",
        vec![read("photo-triage"), final_script(&["Sorted."])],
    )
    .await;
    assert_eq!(
        response.status,
        ditto_protocol::AgentRunStatus::Unverified,
        "the turn finishes: {:?} requests={}",
        response.failure_code,
        driver.requests().len()
    );

    // The first request carries the tool and no body; the second carries the
    // body the model asked for and nothing else did.
    assert!(
        driver.requests()[0]
            .tools
            .iter()
            .any(|tool| tool.id == "skill.read")
    );
    assert!(bodies(&driver, 0).is_empty());
    // The catalogue leads the request, one line per skill, before the model has
    // read anything.
    assert!(
        driver.requests()[0]
            .stable_system_prefix
            .segments
            .iter()
            .any(|segment| segment.contains("- photo-triage: Sort a folder of photos")),
        "the catalogue is in the prefix"
    );
    assert_eq!(
        bodies(&driver, 1),
        ["# Steps\n\nGroup by EXIF date.\n".to_owned()]
    );

    // Uninstalling the skill afterwards must not rewrite the turn: the prefix
    // it was given is in the journal, not on disk.
    std::fs::remove_dir_all(fixture.config.data_dir.join("skills").join("photo-triage"))
        .expect("uninstall");
    fixture.kernel.shutdown_agent_runs().await.unwrap();
    replay_artifact_read_turn(&fixture.events_for_session("personal"), &response.turn_id)
        .expect("replay agrees with the recorded read");
}

#[tokio::test]
async fn a_name_this_profile_does_not_carry_is_refused_without_failing_the_turn() {
    let fixture = Fixture::with_packages(&["artifact-read", "skill-read"]);
    install(&fixture, "photo-triage", "Sort photos.", "body\n");

    let (response, driver) = ask(
        &fixture,
        "Close the books.",
        vec![
            read("ledger-close"),
            final_script(&["I do not have that skill."]),
        ],
    )
    .await;
    assert_eq!(
        response.status,
        ditto_protocol::AgentRunStatus::Unverified,
        "a refusal is a tool result, not a turn failure"
    );
    assert!(bodies(&driver, 1).is_empty(), "a refusal carries no body");
    assert!(
        driver.requests()[1]
            .turn
            .conversation
            .iter()
            .any(|item| matches!(
                item,
                ConversationItem::ToolResult { content, .. }
                    if format!("{content:?}").contains("unknown_skill")
            )),
        "the model is told which refusal it was"
    );

    fixture.kernel.shutdown_agent_runs().await.unwrap();
    replay_artifact_read_turn(&fixture.events_for_session("personal"), &response.turn_id)
        .expect("replay agrees with the recorded refusal");
}

#[tokio::test]
async fn a_profile_with_no_skills_does_not_offer_the_tool() {
    // `web.browse` is the agent tool here; the point is that `skill.read` is
    // not in the list, because this profile carries no skill.
    let fixture = Fixture::with_packages(&["artifact-read", "web-browse", "skill-read"]);
    let (response, driver) = ask(&fixture, "Hello.", vec![final_script(&["Hi."])]).await;
    assert!(
        driver.requests()[0]
            .tools
            .iter()
            .all(|tool| tool.id != "skill.read"),
        "an empty catalogue costs no tool surface"
    );
    fixture.kernel.shutdown_agent_runs().await.unwrap();
    replay_artifact_read_turn(&fixture.events_for_session("personal"), &response.turn_id)
        .expect("replay of a turn without skills");
}
