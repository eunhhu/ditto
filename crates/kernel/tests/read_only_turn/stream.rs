//! The version-7 journal (ADR 0028 Phase C): streamed text commits in chunks
//! without delaying the first words, and requests are recorded as digests of
//! what the driver received.
use super::agent_runs::{start_command, terminal};
use super::*;
use ditto_protocol::{AgentRunResponse, AgentRunStatus};

/// Durable model outputs of one task, in journal order.
fn outputs(fixture: &Fixture, task: &str) -> Vec<ModelOutputPayload> {
    fixture
        .events_for_task(task)
        .into_iter()
        .filter(|event| event.kind == event_kind::MODEL_OUTPUT)
        .map(|event| serde_json::from_value(event.payload).expect("model output"))
        .collect()
}

fn text_of(output: &ModelOutputPayload) -> Option<&str> {
    match &output.stream_event.event {
        ModelEvent::TextDelta { text } => Some(text),
        _ => None,
    }
}

/// Run one agent turn to its terminal status.
async fn run_agent(fixture: &Fixture, driver: Arc<dyn ModelDriver>) -> AgentRunResponse {
    let command = start_command("stream");
    fixture
        .kernel
        .start_agent_run(command.clone(), driver)
        .unwrap();
    let status = terminal(&fixture.kernel, &command).await;
    fixture.kernel.shutdown_agent_runs().await.unwrap();
    status
}

#[tokio::test]
async fn a_text_burst_commits_its_first_delta_alone_then_in_chunks() {
    let fixture = Fixture::new();
    let parts = vec!["tok "; 1_000];
    let status = run_agent(
        &fixture,
        Arc::new(ScriptedDriver::new(vec![final_script(&parts)])),
    )
    .await;
    assert_eq!(status.status, AgentRunStatus::Unverified);
    let turn = status.turn_id;
    let outputs = outputs(&fixture, &status.task_id);
    // The first delta is durable alone; the rest commit in chunks of at most
    // 2 KiB of text that cover the provider sequence exactly, then the
    // terminal. A slow machine may flush one more chunk on the timer.
    assert_eq!(text_of(&outputs[0]), Some("tok "));
    assert_eq!(outputs[0].through_sequence, None);
    let mut next = 1;
    for output in &outputs[1..outputs.len() - 1] {
        assert_eq!(output.stream_event.sequence, next);
        assert!(text_of(output).unwrap().len() <= 2_048);
        next = output.through_sequence.unwrap_or(next) + 1;
    }
    assert_eq!(next, 1_000);
    // Far fewer than the provider's 1,001 events, even on a loaded machine.
    assert!(outputs.len() <= 64, "{} outputs", outputs.len());
    assert!(
        outputs
            .iter()
            .any(|output| output.through_sequence.is_some())
    );
    assert!(matches!(
        outputs.last().unwrap().stream_event.event,
        ModelEvent::Completed { .. }
    ));
    let events = fixture.events_for_session("personal");
    let replay = replay_artifact_read_turn(&events, &turn).unwrap();
    let ArtifactReadTurnReplay::Finished { outcome } = replay.terminal else {
        panic!("finished replay")
    };
    assert_eq!(outcome.response, "tok ".repeat(1_000));

    // Chunks must cover the provider sequence exactly, and only text chunks.
    let position_of = |events: &[EventRecord], matches: &dyn Fn(&EventRecord) -> bool| {
        events
            .iter()
            .position(|event| event.kind == event_kind::MODEL_OUTPUT && matches(event))
            .unwrap()
    };
    let is_chunk = |event: &EventRecord| event.payload.get("through_sequence").is_some();
    let through = |event: &EventRecord| event.payload["through_sequence"].as_u64().unwrap();
    let mut gap = events.clone();
    let position = position_of(&gap, &is_chunk);
    gap[position].payload["through_sequence"] = json!(through(&gap[position]) - 1);
    assert!(replay_artifact_read_turn(&gap, &turn).is_err());
    let mut overlap = events.clone();
    let position = position_of(&overlap, &is_chunk);
    overlap[position].payload["through_sequence"] = json!(through(&overlap[position]) + 1);
    assert!(replay_artifact_read_turn(&overlap, &turn).is_err());
    let mut single = events.clone();
    let position = position_of(&single, &|_| true);
    single[position].payload["through_sequence"] = json!(0);
    assert!(replay_artifact_read_turn(&single, &turn).is_err());
    let mut terminal = events.clone();
    let position = position_of(&terminal, &|event| {
        event.payload["stream_event"]["event"]["type"] == "completed"
    });
    let sequence = terminal[position].payload["stream_event"]["sequence"]
        .as_u64()
        .unwrap();
    terminal[position].payload["through_sequence"] = json!(sequence + 1);
    assert!(replay_artifact_read_turn(&terminal, &turn).is_err());
    let mut changed_text = events.clone();
    let position = position_of(&changed_text, &is_chunk);
    changed_text[position].payload["stream_event"]["event"]["text"] = json!("forged");
    assert!(replay_artifact_read_turn(&changed_text, &turn).is_err());
}

/// Yields each step after its pause and records, before every step, the text
/// that is already durable for the turn.
struct PacedDriver {
    descriptor: DriverDescriptor,
    kernel: DittoKernel,
    task: String,
    steps: Vec<(u64, ModelEvent)>,
    durable_before_step: Arc<Mutex<Vec<String>>>,
}

impl ModelDriver for PacedDriver {
    fn descriptor(&self) -> &DriverDescriptor {
        &self.descriptor
    }

    fn stream(&self, _request: ModelRequest, _cancellation: CancellationToken) -> ModelEventStream {
        let (kernel, task, steps, seen) = (
            self.kernel.clone(),
            self.task.clone(),
            self.steps.clone(),
            self.durable_before_step.clone(),
        );
        ModelEventStream::new(stream! {
            for (pause_ms, event) in steps {
                tokio::time::sleep(std::time::Duration::from_millis(pause_ms)).await;
                let durable = all_task_events(&kernel, &task)
                    .into_iter()
                    .filter(|event| event.kind == event_kind::MODEL_OUTPUT)
                    .filter_map(|event| {
                        event.payload["stream_event"]["event"]["text"]
                            .as_str()
                            .map(str::to_owned)
                    })
                    .collect::<String>();
                seen.lock().unwrap().push(durable);
                yield event;
            }
        })
    }
}

#[tokio::test]
async fn buffered_text_is_durable_within_the_flush_interval_while_the_provider_is_silent() {
    let fixture = Fixture::new();
    let text = |text: &str| ModelEvent::TextDelta { text: text.into() };
    let command = start_command("paced");
    let task = format!("run_{}", command.request_id);
    let driver = Arc::new(PacedDriver {
        descriptor: ScriptedDriver::new(Vec::new()).descriptor,
        kernel: fixture.kernel.clone(),
        task: task.clone(),
        steps: vec![
            (0, text("a")),
            (0, text("b")),
            (0, text("c")),
            (250, text("d")),
            (
                0,
                ModelEvent::Completed {
                    finish_reason: FinishReason::EndTurn,
                    continuation: None,
                },
            ),
        ],
        durable_before_step: Arc::new(Mutex::new(Vec::new())),
    });
    fixture
        .kernel
        .start_agent_run(command.clone(), driver.clone())
        .unwrap();
    let status = terminal(&fixture.kernel, &command).await;
    fixture.kernel.shutdown_agent_runs().await.unwrap();
    assert_eq!(status.response.as_deref(), Some("abcd"));
    // "a" is durable before the provider sends more; "b" and "c" wait in one
    // chunk that commits during the provider's silence, not with "d"; "d"
    // after the quiet interval is durable at once.
    assert_eq!(
        *driver.durable_before_step.lock().unwrap(),
        ["", "a", "a", "abc", "abcd"]
    );
    let outputs = outputs(&fixture, &task);
    assert_eq!(
        outputs.iter().map(text_of).collect::<Vec<_>>(),
        [Some("a"), Some("bc"), Some("d"), None]
    );
    assert_eq!(outputs[1].through_sequence, Some(2));
    replay_artifact_read_turn(&fixture.events_for_session("personal"), &status.turn_id).unwrap();
}

#[tokio::test]
async fn text_chunks_count_every_provider_event_against_the_request_bound() {
    let fixture = Fixture::new();
    let script = (0..ditto_kernel::turn::MAX_MODEL_EVENTS_PER_REQUEST)
        .map(|_| ModelEvent::TextDelta { text: "x".into() })
        .collect::<Vec<_>>();
    let status = run_agent(&fixture, Arc::new(ScriptedDriver::new(vec![script]))).await;
    assert_eq!(status.status, AgentRunStatus::Failed);
    assert_eq!(status.failure_code.as_deref(), Some("bound_exceeded"));
    let outputs = outputs(&fixture, &status.task_id);
    let covered = outputs
        .iter()
        .map(|output| {
            output
                .through_sequence
                .unwrap_or(output.stream_event.sequence)
                - output.stream_event.sequence
                + 1
        })
        .sum::<u64>();
    assert_eq!(
        covered,
        ditto_kernel::turn::MAX_MODEL_EVENTS_PER_REQUEST as u64
    );
    assert!(outputs.len() <= 64, "{} outputs", outputs.len());
    let events = fixture.events_for_session("personal");
    replay_artifact_read_turn(&events, &status.turn_id).unwrap();

    // A chunk may not claim more provider events than the bound admits.
    let mut forged = events.clone();
    let last = forged
        .iter_mut()
        .rev()
        .find(|event| event.kind == event_kind::MODEL_OUTPUT)
        .unwrap();
    let through = last.payload["through_sequence"].as_u64().unwrap();
    last.payload["through_sequence"] = json!(through + 1);
    assert!(replay_artifact_read_turn(&forged, &status.turn_id).is_err());
}

#[tokio::test]
async fn each_request_is_recorded_as_the_digest_of_what_the_driver_received() {
    let fixture = Fixture::new();
    let reference = fixture.store(b"abcdef", "session-1", Some("task-1"));
    let driver = ScriptedDriver::new(vec![
        tool_request_script(
            "call-1",
            ARTIFACT_READ_ID,
            artifact_arguments(&reference, 1, 4),
            FinishReason::ToolCalls,
        ),
        final_script(&["read ", "done"]),
    ]);
    let outcome = fixture
        .kernel
        .run_artifact_read_turn(
            command("session-1", "task-1"),
            Vec::new(),
            &driver,
            CancellationToken::new(),
            ReadOnlyTurnControl::default(),
        )
        .await
        .unwrap();
    let requested = fixture
        .events_for_task("task-1")
        .into_iter()
        .filter(|event| event.kind == event_kind::MODEL_REQUESTED)
        .collect::<Vec<_>>();
    let sent = driver.requests();
    assert_eq!(requested.len(), 2);
    for (event, request) in requested.iter().zip(&sent) {
        assert_eq!(
            event.payload["request_sha256"],
            json!(ditto_kernel::turn::request_sha256(request))
        );
        assert_eq!(event.payload["request_id"], json!(request.request_id));
        assert!(event.payload.get("request").is_none());
        assert!(serde_json::to_vec(&event.payload).unwrap().len() < 512);
    }
    // Replay rebuilds both requests exactly, tool result included.
    let replay =
        replay_artifact_read_turn(&fixture.events_for_session("session-1"), &outcome.turn_id)
            .unwrap();
    let rebuilt = replay
        .requests
        .iter()
        .map(|payload| serde_json::to_value(&payload.request).unwrap())
        .collect::<Vec<_>>();
    let expected = sent
        .iter()
        .map(|request| serde_json::to_value(request).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(rebuilt, expected);
}
