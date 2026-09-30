//! Sessions run at once (ADR 0028 Phase D): each session keeps one run at a
//! time, and different sessions stream in parallel up to four.
use super::agent_runs::{query, start_command, terminal};
use super::*;
use ditto_kernel::AgentRunError;
use ditto_protocol::{AgentRunStatus, StartAgentRunCommand};
use std::sync::atomic::{AtomicUsize, Ordering};

/// Streams "done" once released, counting the streams it has started.
struct Gate {
    descriptor: DriverDescriptor,
    started: AtomicUsize,
    release: CancellationToken,
}

impl ModelDriver for Gate {
    fn descriptor(&self) -> &DriverDescriptor {
        &self.descriptor
    }

    fn stream(&self, _: ModelRequest, cancellation: CancellationToken) -> ModelEventStream {
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn different_sessions_stream_at_once_up_to_four() {
    let fixture = Fixture::new();
    let kernel = &fixture.kernel;
    let gate = gate();
    let driver: Arc<dyn ModelDriver> = gate.clone();
    let commands = ["one", "two", "three", "four"].map(in_session);
    for command in &commands {
        let accepted = kernel
            .start_agent_run(command.clone(), driver.clone())
            .unwrap();
        assert_eq!(accepted.status, AgentRunStatus::Running);
    }
    // All four reach the provider before any finishes.
    streaming(&gate, 4).await;

    // A second run in a busy session, or a fifth session, is not queued.
    assert!(matches!(
        kernel.start_agent_run(start_command("again"), driver.clone()),
        Err(AgentRunError::Busy)
    ));
    assert!(matches!(
        kernel.start_agent_run(
            StartAgentRunCommand {
                session_id: "one".into(),
                ..start_command("again")
            },
            driver.clone()
        ),
        Err(AgentRunError::Busy)
    ));
    let fifth = in_session("five");
    assert!(matches!(
        kernel.start_agent_run(fifth.clone(), driver.clone()),
        Err(AgentRunError::Busy)
    ));

    // Cancelling one run ends that run alone and frees a slot.
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
    assert_eq!(
        kernel
            .start_agent_run(fifth.clone(), driver.clone())
            .unwrap()
            .status,
        AgentRunStatus::Running
    );
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
