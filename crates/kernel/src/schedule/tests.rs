use super::*;
use crate::KernelConfig;
use ditto_model::{
    DriverDescriptor, DriverId, FinishReason, ModelEvent, ModelEventStream, ModelFeature,
    ModelRequest, ParallelToolCalls, RequestCapabilities, ToolChoiceKind,
};
use std::sync::{Mutex, atomic::AtomicUsize};

mod repeats;

struct Driver {
    descriptor: DriverDescriptor,
    calls: AtomicUsize,
    requests: Mutex<Vec<ModelRequest>>,
    block: bool,
}
impl Driver {
    fn new(block: bool) -> Arc<Self> {
        Arc::new(Self {
            descriptor: DriverDescriptor {
                id: DriverId::new("schedule-fixture").unwrap(),
                request_capabilities: RequestCapabilities {
                    tool_choices: [ToolChoiceKind::Auto].into_iter().collect(),
                    parallel_tool_calls: [ParallelToolCalls::Forbid].into_iter().collect(),
                    ..Default::default()
                },
                emitted_features: [ModelFeature::Text, ModelFeature::ToolCalls]
                    .into_iter()
                    .collect(),
            },
            calls: AtomicUsize::new(0),
            requests: Mutex::new(Vec::new()),
            block,
        })
    }
}
impl ModelDriver for Driver {
    fn descriptor(&self) -> &DriverDescriptor {
        &self.descriptor
    }
    fn stream(&self, request: ModelRequest, _: CancellationToken) -> ModelEventStream {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.requests.lock().unwrap().push(request);
        let block = self.block;
        ModelEventStream::new(async_stream::stream! {
            if block { std::future::pending::<()>().await; }
            yield ModelEvent::TextDelta { text: "scheduled answer".into() };
            yield ModelEvent::Completed { finish_reason: FinishReason::EndTurn, continuation: None };
        })
    }
}
fn config(root: &std::path::Path) -> KernelConfig {
    KernelConfig::new(
        root.join("data"),
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../capabilities"),
    )
}
fn command() -> ScheduleRunCommand {
    let due_at = DateTime::from_timestamp_millis(Utc::now().timestamp_millis() + 60_000).unwrap();
    ScheduleRunCommand {
        request_id: ulid::Ulid::new().to_string(),
        session_id: "personal".into(),
        text: "cedar timezone".into(),
        due_at,
        expires_at: due_at + chrono::Duration::minutes(5),
    }
}
async fn terminal(kernel: &DittoKernel, command: &ScheduleRunCommand) -> ScheduleResponse {
    let mut events = kernel.subscribe();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let result = kernel.inspect_schedule(identity(command)).unwrap();
            if !matches!(
                result.status,
                ScheduleStatus::Pending | ScheduleStatus::Running
            ) {
                return result;
            }
            events.recv().await.unwrap();
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn restart_before_dispatch_uses_current_context_once_and_replays_after_terminal() {
    let root = tempfile::tempdir().unwrap();
    let cfg = config(root.path());
    let kernel = DittoKernel::open(cfg.clone()).unwrap();
    let command = command();
    kernel.schedule_run(command.clone()).unwrap();
    drop(kernel);
    let kernel = DittoKernel::open(cfg.clone()).unwrap();
    let input = kernel
        .record_user_input(ditto_protocol::SubmitInputCommand {
            text: "cedar timezone is KST".into(),
            session_id: Some("personal".into()),
            task_id: None,
        })
        .unwrap();
    kernel
        .remember_input(ditto_protocol::RememberInputCommand {
            session_id: "personal".into(),
            input_event_id: input.event_id,
            replaces: None,
        })
        .unwrap();
    let driver = Driver::new(false);
    let dyn_driver: Arc<dyn ModelDriver> = driver.clone();
    assert!(matches!(
        kernel
            .scheduler_step(Some(&dyn_driver), || command.due_at
                - chrono::Duration::milliseconds(1))
            .unwrap(),
        Step::Wait(_)
    ));
    assert_eq!(driver.calls.load(Ordering::SeqCst), 0);
    kernel
        .scheduler_step(Some(&dyn_driver), || command.due_at)
        .unwrap();
    let result = terminal(&kernel, &command).await;
    assert_eq!(result.status, ScheduleStatus::Unverified);
    assert!(
        serde_json::to_string(&driver.requests.lock().unwrap()[0])
            .unwrap()
            .contains("cedar timezone is KST")
    );
    let events = kernel
        .list_events(&ditto_protocol::EventQuery {
            limit: Some(1000),
            ..Default::default()
        })
        .unwrap();
    crate::replay_artifact_read_turn(&events, &result.run.as_ref().unwrap().turn_id).unwrap();
    kernel.shutdown_agent_runs().await.unwrap();
    drop(kernel);
    let kernel = DittoKernel::open(cfg).unwrap();
    assert_eq!(kernel.schedule_run(command.clone()).unwrap(), result);
    assert!(matches!(
        kernel
            .scheduler_step(Some(&dyn_driver), || command.due_at)
            .unwrap(),
        Step::Wait(None)
    ));
    assert_eq!(driver.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn durable_claim_before_admission_is_interrupted_and_reserved_identity_cannot_be_reused() {
    let root = tempfile::tempdir().unwrap();
    let cfg = config(root.path());
    let kernel = DittoKernel::open(cfg.clone()).unwrap();
    let command = command();
    kernel.schedule_run(command.clone()).unwrap();
    let entry = kernel.schedule_entry(&identity(&command)).unwrap();
    let driver = Driver::new(false);
    assert!(matches!(
        kernel.start_agent_run(
            StartAgentRunCommand {
                request_id: entry.run_request_id.clone(),
                session_id: command.session_id.clone(),
                text: command.text.clone(),
                sort: None
            },
            driver.clone()
        ),
        Err(AgentRunError::Conflict)
    ));
    kernel
        .schedule_transition(&entry, ScheduleState::Claimed)
        .unwrap();
    drop(kernel);
    let kernel = DittoKernel::open(cfg).unwrap();
    assert_eq!(
        kernel.inspect_schedule(identity(&command)).unwrap().status,
        ScheduleStatus::Interrupted
    );
    let dyn_driver: Arc<dyn ModelDriver> = driver.clone();
    kernel
        .scheduler_step(Some(&dyn_driver), || command.due_at)
        .unwrap();
    assert_eq!(
        kernel.schedule_run(command).unwrap().status,
        ScheduleStatus::Interrupted
    );
    assert_eq!(driver.calls.load(Ordering::SeqCst), 0);
}

#[test]
fn runtime_owner_loss_during_dispatch_is_interrupted_without_rerun() {
    let root = tempfile::tempdir().unwrap();
    let cfg = config(root.path());
    let kernel = DittoKernel::open(cfg.clone()).unwrap();
    let command = command();
    kernel.schedule_run(command.clone()).unwrap();
    let driver = Driver::new(true);
    let dyn_driver: Arc<dyn ModelDriver> = driver.clone();
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        kernel
            .scheduler_step(Some(&dyn_driver), || command.due_at)
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while driver.calls.load(Ordering::SeqCst) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    });
    drop(rt);
    drop(kernel);
    let kernel = DittoKernel::open(cfg).unwrap();
    assert_eq!(
        kernel.inspect_schedule(identity(&command)).unwrap().status,
        ScheduleStatus::Interrupted
    );
    assert!(matches!(
        kernel
            .scheduler_step(Some(&dyn_driver), || command.due_at)
            .unwrap(),
        Step::Wait(None)
    ));
    assert_eq!(driver.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn cancellation_is_durable_before_dispatch_and_signals_an_owned_run() {
    let root = tempfile::tempdir().unwrap();
    let cfg = config(root.path());
    let kernel = DittoKernel::open(cfg.clone()).unwrap();
    let first = command();
    kernel.schedule_run(first.clone()).unwrap();
    assert_eq!(
        kernel.cancel_schedule(identity(&first)).unwrap().status,
        ScheduleStatus::Cancelled
    );
    let driver: Arc<dyn ModelDriver> = Driver::new(true);
    kernel
        .scheduler_step(Some(&driver), || first.due_at)
        .unwrap();
    let second = command();
    kernel.schedule_run(second.clone()).unwrap();
    kernel
        .scheduler_step(Some(&driver), || second.due_at)
        .unwrap();
    assert!(
        kernel
            .cancel_schedule(identity(&second))
            .unwrap()
            .cancellation_requested
    );
    let result = terminal(&kernel, &second).await;
    assert_eq!(result.status, ScheduleStatus::Failed);
    assert_eq!(
        result.run.as_ref().unwrap().failure_code.as_deref(),
        Some("cancelled")
    );
    kernel.shutdown_agent_runs().await.unwrap();
    drop(kernel);
    let kernel = DittoKernel::open(cfg).unwrap();
    assert_eq!(
        kernel.inspect_schedule(identity(&first)).unwrap().status,
        ScheduleStatus::Cancelled
    );
    assert_eq!(kernel.inspect_schedule(identity(&second)).unwrap(), result);
}

#[tokio::test]
async fn busy_slot_defers_work_and_expiry_is_exclusive_even_without_provider() {
    let root = tempfile::tempdir().unwrap();
    let kernel = DittoKernel::open(config(root.path())).unwrap();
    let driver: Arc<dyn ModelDriver> = Driver::new(true);
    kernel
        .start_agent_run(
            StartAgentRunCommand {
                request_id: ulid::Ulid::new().to_string(),
                session_id: "personal".into(),
                text: "wait".into(),
                sort: None,
            },
            driver.clone(),
        )
        .unwrap();
    let first = command();
    kernel.schedule_run(first.clone()).unwrap();
    assert!(
        matches!(kernel.scheduler_step(Some(&driver), || first.due_at).unwrap(), Step::Wait(Some(at)) if at == first.expires_at)
    );
    assert_eq!(
        kernel.inspect_schedule(identity(&first)).unwrap().status,
        ScheduleStatus::Pending
    );
    kernel.scheduler_step(None, || first.expires_at).unwrap();
    assert_eq!(
        kernel.inspect_schedule(identity(&first)).unwrap().status,
        ScheduleStatus::Missed
    );
    kernel.shutdown_agent_runs().await.unwrap();
}

#[tokio::test]
async fn timer_and_admission_wake_one_owner_and_disabled_provider_only_expires() {
    let root = tempfile::tempdir().unwrap();
    let kernel = DittoKernel::open(config(root.path())).unwrap();
    let shutdown = CancellationToken::new();
    let cloned = kernel.clone();
    let token = shutdown.clone();
    let driver = Driver::new(false);
    let dyn_driver: Arc<dyn ModelDriver> = driver.clone();
    let task = tokio::spawn(async move { cloned.run_scheduler(Some(dyn_driver), token).await });
    tokio::time::timeout(Duration::from_secs(5), async {
        while kernel.inner.scheduler_state.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(matches!(
        kernel.run_scheduler(None, CancellationToken::new()).await,
        Err(AgentRunError::Busy)
    ));
    let mut first = command();
    first.due_at = DateTime::from_timestamp_millis(Utc::now().timestamp_millis() + 200).unwrap();
    first.expires_at = first.due_at + chrono::Duration::seconds(5);
    kernel.schedule_run(first.clone()).unwrap();
    assert_eq!(
        terminal(&kernel, &first).await.status,
        ScheduleStatus::Unverified
    );
    assert_eq!(driver.calls.load(Ordering::SeqCst), 1);
    shutdown.cancel();
    task.await.unwrap().unwrap();
    let mut second = command();
    second.due_at = DateTime::from_timestamp_millis(Utc::now().timestamp_millis() + 100).unwrap();
    second.expires_at = second.due_at + chrono::Duration::milliseconds(100);
    kernel.schedule_run(second.clone()).unwrap();
    let cloned = kernel.clone();
    let token = CancellationToken::new();
    let stop = token.clone();
    let task = tokio::spawn(async move { cloned.run_scheduler(None, token).await });
    assert_eq!(
        terminal(&kernel, &second).await.status,
        ScheduleStatus::Missed
    );
    stop.cancel();
    task.await.unwrap().unwrap();
    assert_eq!(driver.calls.load(Ordering::SeqCst), 1);
}

#[test]
fn bounded_pending_index_rebuilds_from_events_and_rejects_changed_retries_and_scopes() {
    let root = tempfile::tempdir().unwrap();
    let cfg = config(root.path());
    let kernel = DittoKernel::open(cfg.clone()).unwrap();
    let first = command();
    kernel.schedule_run(first.clone()).unwrap();
    let count = kernel.event_count().unwrap();
    kernel.schedule_run(first.clone()).unwrap();
    assert_eq!(kernel.event_count().unwrap(), count);
    let mut changed = first.clone();
    changed.text.push('!');
    assert!(matches!(
        kernel.schedule_run(changed),
        Err(AgentRunError::Conflict)
    ));
    assert!(matches!(
        kernel.inspect_schedule(AgentRunQuery {
            request_id: first.request_id.clone(),
            session_id: "other".into()
        }),
        Err(AgentRunError::NotFound)
    ));
    for _ in 1..MAX_PENDING_SCHEDULES {
        kernel.schedule_run(command()).unwrap();
    }
    assert!(matches!(
        kernel.schedule_run(command()),
        Err(AgentRunError::ScheduleFull)
    ));
    kernel.cancel_schedule(identity(&first)).unwrap();
    kernel.schedule_run(command()).unwrap();
    let pending = kernel.list_pending_schedules("personal").unwrap();
    assert_eq!(pending.len(), MAX_PENDING_SCHEDULES);
    drop(kernel);
    rusqlite::Connection::open(cfg.data_dir.join("state.db"))
        .unwrap()
        .execute("DROP TABLE schedule_index", [])
        .unwrap();
    let kernel = DittoKernel::open(cfg).unwrap();
    assert_eq!(kernel.list_pending_schedules("personal").unwrap(), pending);
    assert_eq!(
        kernel.inspect_schedule(identity(&first)).unwrap().status,
        ScheduleStatus::Cancelled
    );
}

#[test]
fn invalid_time_scope_and_index_source_drift_fail_before_dispatch() {
    let root = tempfile::tempdir().unwrap();
    let cfg = config(root.path());
    let kernel = DittoKernel::open(cfg.clone()).unwrap();
    let first = command();
    for invalid in [
        ScheduleRunCommand {
            expires_at: first.due_at,
            ..first.clone()
        },
        ScheduleRunCommand {
            due_at: first.due_at + chrono::Duration::nanoseconds(1),
            ..first.clone()
        },
        ScheduleRunCommand {
            session_id: " bad ".into(),
            ..first.clone()
        },
    ] {
        assert!(matches!(
            kernel.schedule_run(invalid),
            Err(AgentRunError::Invalid(_))
        ));
    }
    assert_eq!(kernel.event_count().unwrap(), 0);
    kernel.schedule_run(first.clone()).unwrap();
    rusqlite::Connection::open(cfg.data_dir.join("state.db"))
        .unwrap()
        .execute("UPDATE schedule_index SET due_at_ms = 0", [])
        .unwrap();
    assert!(matches!(
        kernel.inspect_schedule(identity(&first)),
        Err(AgentRunError::Storage)
    ));
    let driver: Arc<dyn ModelDriver> = Driver::new(false);
    assert!(matches!(
        kernel.scheduler_step(Some(&driver), || first.due_at),
        Err(AgentRunError::Storage)
    ));
}

#[tokio::test]
async fn releasing_a_manual_slot_wakes_due_work_without_overlap_or_polling() {
    let root = tempfile::tempdir().unwrap();
    let kernel = DittoKernel::open(config(root.path())).unwrap();
    let manual = StartAgentRunCommand {
        request_id: ulid::Ulid::new().to_string(),
        session_id: "personal".into(),
        text: "wait".into(),
        sort: None,
    };
    kernel
        .start_agent_run(manual.clone(), Driver::new(true))
        .unwrap();
    let mut scheduled = command();
    scheduled.due_at =
        DateTime::from_timestamp_millis(Utc::now().timestamp_millis() + 100).unwrap();
    scheduled.expires_at = scheduled.due_at + chrono::Duration::seconds(5);
    kernel.schedule_run(scheduled.clone()).unwrap();
    let driver = Driver::new(false);
    let dyn_driver: Arc<dyn ModelDriver> = driver.clone();
    let stop = CancellationToken::new();
    let token = stop.clone();
    let cloned = kernel.clone();
    let task = tokio::spawn(async move { cloned.run_scheduler(Some(dyn_driver), token).await });
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(
        kernel
            .inspect_schedule(identity(&scheduled))
            .unwrap()
            .waiting_for,
        Some(ScheduleWaitReason::RuntimeBusy)
    );
    assert_eq!(driver.calls.load(Ordering::SeqCst), 0);
    kernel
        .cancel_agent_run(AgentRunQuery {
            request_id: manual.request_id,
            session_id: manual.session_id,
        })
        .unwrap();
    assert_eq!(
        terminal(&kernel, &scheduled).await.status,
        ScheduleStatus::Unverified
    );
    assert_eq!(driver.calls.load(Ordering::SeqCst), 1);
    // Shutdown of an empty runtime also wakes its scheduler without a timer.
    kernel.shutdown_agent_runs().await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    stop.cancel();
}

#[tokio::test]
async fn start_window_is_rechecked_after_source_verification() {
    let root = tempfile::tempdir().unwrap();
    let kernel = DittoKernel::open(config(root.path())).unwrap();
    let command = command();
    kernel.schedule_run(command.clone()).unwrap();
    let clock_reads = AtomicUsize::new(0);
    let driver = Driver::new(false);
    let dyn_driver: Arc<dyn ModelDriver> = driver.clone();
    kernel
        .scheduler_step(Some(&dyn_driver), || {
            if clock_reads.fetch_add(1, Ordering::SeqCst) == 0 {
                command.due_at
            } else {
                command.expires_at
            }
        })
        .unwrap();
    assert_eq!(
        kernel.inspect_schedule(identity(&command)).unwrap().status,
        ScheduleStatus::Missed
    );
    assert_eq!(driver.calls.load(Ordering::SeqCst), 0);
}
