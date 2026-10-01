//! The chat as a bridge (ADR 0035): four runs stream at once, from one
//! session or several; a later run sees the runs still in flight; and an
//! acknowledgment is recorded without a model call.
use super::agent_runs::{ask, message_texts, query, start_command, terminal};
use super::*;
use ditto_kernel::AgentRunError;
use ditto_protocol::{AgentRunStatus, StartAgentRunCommand};
use std::sync::atomic::{AtomicUsize, Ordering};

/// Streams "done" once released, counting and keeping the requests it has
/// started.
struct Gate {
    descriptor: DriverDescriptor,
    started: AtomicUsize,
    requests: Mutex<Vec<ModelRequest>>,
    release: CancellationToken,
}

impl ModelDriver for Gate {
    fn descriptor(&self) -> &DriverDescriptor {
        &self.descriptor
    }

    fn stream(&self, request: ModelRequest, cancellation: CancellationToken) -> ModelEventStream {
        self.requests.lock().unwrap().push(request);
        self.started.fetch_add(1, Ordering::SeqCst);
        let release = self.release.clone();
        ModelEventStream::new(stream! {
            tokio::select! {
                () = release.cancelled() => {}
                () = cancellation.cancelled() => {}
            }
            for event in final_script(&["done"]) { yield event; }
        })
    }
}

fn gate() -> Arc<Gate> {
    Arc::new(Gate {
        descriptor: ScriptedDriver::new(Vec::new()).descriptor,
        started: AtomicUsize::new(0),
        requests: Mutex::default(),
        release: CancellationToken::new(),
    })
}

fn in_session(session: &str) -> StartAgentRunCommand {
    StartAgentRunCommand {
        session_id: session.into(),
        ..start_command("hello")
    }
}

async fn streaming(gate: &Gate, count: usize) {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while gate.started.load(Ordering::SeqCst) < count {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("runs reach the provider");
}

/// The text of a request's latest message, notes included.
fn latest(request: &ModelRequest) -> String {
    message_texts(request).pop().expect("a latest message").1
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn four_runs_stream_at_once_from_one_session_or_several() {
    let fixture = Fixture::new();
    let kernel = &fixture.kernel;
    let gate = gate();
    let driver: Arc<dyn ModelDriver> = gate.clone();
    let commands = ["one", "one", "two", "three"].map(in_session);
    for command in &commands {
        let accepted = kernel
            .start_agent_run(command.clone(), driver.clone())
            .unwrap();
        assert_eq!(accepted.status, AgentRunStatus::Running);
    }
    // All four reach the provider before any finishes; two share a session.
    streaming(&gate, 4).await;

    // A fifth run, in a running session or another, or a sort, is not queued.
    for fifth in [in_session("one"), in_session("five")] {
        assert!(matches!(
            kernel.start_agent_run(fifth, driver.clone()),
            Err(AgentRunError::Busy)
        ));
    }
    assert!(matches!(
        kernel.start_sort(ditto_protocol::StartSortCommand {
            request_id: ulid::Ulid::new().to_string(),
            session_id: "one".into(),
            text: "b\na".into(),
            unique: false,
        }),
        Err(AgentRunError::Busy)
    ));

    // Cancelling one run ends that run alone, even in its own session, and
    // frees a slot.
    kernel.cancel_agent_run(query(&commands[0])).unwrap();
    let cancelled = terminal(kernel, &commands[0]).await;
    assert_eq!(cancelled.status, AgentRunStatus::Failed);
    assert_eq!(cancelled.failure_code.as_deref(), Some("cancelled"));
    for command in &commands[1..] {
        assert_eq!(
            kernel.inspect_agent_run(query(command)).unwrap().status,
            AgentRunStatus::Running
        );
    }
    // The slot frees once the cancelled run's task ends, just after its
    // terminal is durable; a client retries a busy start.
    let fifth = in_session("five");
    let started = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            match kernel.start_agent_run(fifth.clone(), driver.clone()) {
                Err(AgentRunError::Busy) => {
                    tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                }
                result => return result.unwrap(),
            }
        }
    })
    .await
    .expect("the freed slot admits a fifth run");
    assert_eq!(started.status, AgentRunStatus::Running);
    streaming(&gate, 5).await;

    gate.release.cancel();
    let events = fixture.events_for_session("one");
    replay_artifact_read_turn(&events, &cancelled.turn_id).unwrap();
    for command in commands[1..].iter().chain([&fifth]) {
        let finished = terminal(kernel, command).await;
        assert_eq!(finished.status, AgentRunStatus::Unverified);
        assert_eq!(finished.response.as_deref(), Some("done"));
        replay_artifact_read_turn(
            &fixture.events_for_session(&command.session_id),
            &finished.turn_id,
        )
        .unwrap();
    }
    kernel.shutdown_agent_runs().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_cancels_and_drains_every_session() {
    let fixture = Fixture::new();
    let kernel = &fixture.kernel;
    let gate = gate();
    let driver: Arc<dyn ModelDriver> = gate.clone();
    let commands = ["one", "two"].map(in_session);
    for command in &commands {
        kernel
            .start_agent_run(command.clone(), driver.clone())
            .unwrap();
    }
    streaming(&gate, 2).await;
    kernel.shutdown_agent_runs().await.unwrap();
    for command in &commands {
        let status = kernel.inspect_agent_run(query(command)).unwrap();
        assert_eq!(status.status, AgentRunStatus::Failed);
        assert_eq!(status.failure_code.as_deref(), Some("cancelled"));
    }
    assert!(matches!(
        kernel.start_agent_run(in_session("three"), driver),
        Err(AgentRunError::Stopping)
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_later_run_sees_the_runs_still_in_flight() {
    let fixture = Fixture::new();
    let kernel = &fixture.kernel;
    let gate = gate();
    let driver: Arc<dyn ModelDriver> = gate.clone();
    let first = start_command("Build the release   and\npublish it");
    kernel
        .start_agent_run(first.clone(), driver.clone())
        .unwrap();
    streaming(&gate, 1).await;
    let second = start_command("How is it going?");
    let accepted = kernel
        .start_agent_run(second.clone(), driver.clone())
        .unwrap();
    assert_eq!(accepted.status, AgentRunStatus::Running);
    streaming(&gate, 2).await;

    // The second message carries a second note after the local time; the
    // request it quotes is one line.
    let requests = gate.requests.lock().unwrap().clone();
    assert!(!latest(&requests[0]).contains("still working"));
    let text = latest(&requests[1]);
    let (time, rest) = text.split_once('\n').unwrap();
    assert!(time.starts_with("[Ditto: local time ") && time.ends_with(")]"));
    assert_eq!(
        rest,
        "[Ditto: still working on \"Build the release and publish it\" (started just now)]\n\nHow is it going?"
    );

    gate.release.cancel();
    let finished = [
        terminal(kernel, &first).await,
        terminal(kernel, &second).await,
    ];
    let events = fixture.events_for_session("personal");
    for run in &finished {
        assert_eq!(run.status, AgentRunStatus::Unverified);
        replay_artifact_read_turn(&events, &run.turn_id).unwrap();
    }
    let input_of = |turn: &str| {
        events
            .iter()
            .position(|event| {
                event.kind == event_kind::INPUT_RECEIVED
                    && event.correlation_id.as_deref() == Some(turn)
            })
            .unwrap()
    };
    let (first_input, second_input) = (
        input_of(&finished[0].turn_id),
        input_of(&finished[1].turn_id),
    );
    assert_eq!(
        events[second_input].payload["agent_run"]["in_flight"],
        json!([events[first_input].event_id])
    );
    assert!(
        events[first_input].payload["agent_run"]
            .get("in_flight")
            .is_none()
    );

    // A listed run must be an earlier agent run of the session, and the note
    // follows the list exactly.
    let replays = |forged: &[EventRecord], turn: &str| replay_artifact_read_turn(forged, turn);
    for listed in [
        json!(["01J00000000000000000000000"]),
        json!([events[second_input].event_id]),
        json!([events[first_input + 1].event_id]),
    ] {
        let mut forged = events.clone();
        forged[second_input].payload["agent_run"]["in_flight"] = listed;
        assert!(replays(&forged, &finished[1].turn_id).is_err());
    }
    let mut forged = events.clone();
    forged[second_input].payload["agent_run"]
        .as_object_mut()
        .unwrap()
        .remove("in_flight");
    assert!(replays(&forged, &finished[1].turn_id).is_err());
    let mut forged = events.clone();
    forged[first_input].payload["agent_run"]["in_flight"] = json!([events[second_input].event_id]);
    assert!(replays(&forged, &finished[0].turn_id).is_err());

    // A listed run that had ended before the message is left out of the
    // note, at run time and in replay alike: moving the first run's end
    // before the second message changes the note the request was built with.
    let renumbered = |mut events: Vec<EventRecord>| {
        for (seq, event) in events.iter_mut().enumerate() {
            event.seq = seq as i64 + 1;
        }
        events
    };
    replays(&renumbered(events.clone()), &finished[1].turn_id).unwrap();
    let first_end = events
        .iter()
        .position(|event| {
            event.kind == event_kind::TURN_FINISHED
                && event.correlation_id.as_deref() == Some(finished[0].turn_id.as_str())
        })
        .unwrap();
    let mut moved = events.clone();
    let end = moved.remove(first_end);
    moved.insert(second_input, end);
    assert!(replays(&renumbered(moved), &finished[1].turn_id).is_err());

    // A run after both have ended has no such note.
    let third = start_command("What else?");
    kernel
        .start_agent_run(third.clone(), driver.clone())
        .unwrap();
    let third = terminal(kernel, &third).await;
    assert!(!latest(&gate.requests.lock().unwrap()[2]).contains("still working"));
    replay_artifact_read_turn(&fixture.events_for_session("personal"), &third.turn_id).unwrap();
    kernel.shutdown_agent_runs().await.unwrap();
}

#[tokio::test]
async fn an_acknowledgment_costs_no_model_call_unless_it_answers_a_question() {
    let fixture = Fixture::new();
    let kernel = &fixture.kernel;
    let (built, _) = ask(
        kernel,
        "personal",
        "Build it",
        final_script(&["Your build is done."]),
    )
    .await;
    assert_eq!(built.status, AgentRunStatus::Unverified);

    // After an answer that asked nothing, "ㅇㅋ!" is recorded and answered
    // by no model request.
    let silent = ScriptedDriver::new(Vec::new());
    let okay = start_command("ㅇㅋ!");
    let acknowledged = kernel
        .start_agent_run(okay.clone(), Arc::new(silent.clone()))
        .unwrap();
    assert_eq!(acknowledged.status, AgentRunStatus::Acknowledged);
    assert!(acknowledged.response.is_none());
    assert!(silent.requests().is_empty());
    let retried = kernel
        .start_agent_run(okay.clone(), Arc::new(silent.clone()))
        .unwrap();
    assert_eq!(retried, acknowledged);
    assert_eq!(
        kernel.inspect_agent_run(query(&okay)).unwrap(),
        acknowledged
    );
    let recorded = fixture.events_for_task(&acknowledged.task_id);
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].kind, event_kind::INPUT_RECEIVED);
    assert_eq!(recorded[0].payload["agent_run"]["acknowledged"], true);
    let events = fixture.events_for_session("personal");
    assert!(replay_artifact_read_turn(&events, &acknowledged.turn_id).is_err());

    // An answer that ends by asking makes the same word an answer, which
    // runs; the acknowledgment is not part of the thread.
    let (asked, _) = ask(
        kernel,
        "personal",
        "Can you publish it?",
        final_script(&["It is ready.\nShall I publish it now? 🙂"]),
    )
    .await;
    assert_eq!(asked.status, AgentRunStatus::Unverified);
    let (published, driver) = ask(kernel, "personal", "ok", final_script(&["Published."])).await;
    assert_eq!(published.status, AgentRunStatus::Unverified);
    assert_eq!(published.response.as_deref(), Some("Published."));
    let users = message_texts(&driver.requests()[0])
        .into_iter()
        .filter(|(role, _)| role == "User")
        .map(|(_, text)| text)
        .collect::<Vec<_>>();
    assert_eq!(users[..2], ["Build it", "Can you publish it?"]);
    assert!(users[2].ends_with("\n\nok"));
    replay_artifact_read_turn(&fixture.events_for_session("personal"), &published.turn_id).unwrap();
    kernel.shutdown_agent_runs().await.unwrap();
}
