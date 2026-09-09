use super::*;
use ditto_event_store::recurrence::{OCCURRENCE_CLAIMED, REPEAT_SKIPPED, RepeatTiming};
use ditto_protocol::{RepeatScheduleCommand, RepeatScheduleStatus};

fn repeated(count: u32) -> RepeatScheduleCommand {
    let base = command();
    RepeatScheduleCommand {
        request_id: base.request_id,
        session_id: base.session_id,
        text: base.text,
        due_at: base.due_at,
        expires_at: base.due_at + chrono::Duration::seconds(30),
        every_seconds: 60,
        occurrences: count,
    }
}
fn query(command: &RepeatScheduleCommand) -> AgentRunQuery {
    AgentRunQuery {
        request_id: command.request_id.clone(),
        session_id: command.session_id.clone(),
    }
}
async fn last_terminal(kernel: &DittoKernel, command: &RepeatScheduleCommand) -> ScheduleResponse {
    let mut events = kernel.subscribe();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(child) = kernel
                .inspect_repeat(query(command))
                .unwrap()
                .last_occurrence
                && child.status != ScheduleStatus::Running
            {
                return child;
            }
            events.recv().await.unwrap();
        }
    })
    .await
    .unwrap()
}
fn due(command: &RepeatScheduleCommand, ordinal: u32) -> DateTime<Utc> {
    DateTime::from_timestamp_millis(
        RepeatTiming::new(command)
            .unwrap()
            .window(ordinal)
            .unwrap()
            .0,
    )
    .unwrap()
}
fn claim_without_admission(
    kernel: &DittoKernel,
    command: &RepeatScheduleCommand,
) -> (String, String) {
    let parent = kernel
        .inner
        .events
        .repeat_entry(&command.session_id, &command.request_id)
        .unwrap()
        .unwrap();
    let child = ulid::Ulid::new().to_string();
    let run = ulid::Ulid::new().to_string();
    kernel.append_and_publish(NewEvent {
        session_id: Some(command.session_id.clone()), task_id: Some(format!("schedule_{child}")),
        actor: EventActor::Scheduler, kind: OCCURRENCE_CLAIMED.into(),
        payload: json!({"version":1,"parent_request_id":parent.request_id,"source_event_id":parent.source_event_id,
            "occurrence":parent.next_occurrence,"run_request_id":run,"observed_at_ms":due(command,parent.next_occurrence).timestamp_millis(),
            "progress":parent.claim_progress(&child)}),
        causation_id: Some(parent.last_event_id), correlation_id: None, span_id: None,
    }).unwrap();
    (child, run)
}

#[tokio::test]
async fn mixed_queues_select_earliest_due_and_skip_repeat_expiry_while_busy() {
    for offset in [-1, 0, 1] {
        let root = tempfile::tempdir().unwrap();
        let kernel = DittoKernel::open(config(root.path())).unwrap();
        let one = command();
        let mut repeat = repeated(2);
        repeat.due_at = one.due_at + chrono::Duration::seconds(offset);
        repeat.expires_at = repeat.due_at + chrono::Duration::seconds(30);
        kernel.schedule_run(one.clone()).unwrap();
        kernel.repeat_schedule(repeat.clone()).unwrap();
        let driver = Driver::new(true);
        let dyn_driver: Arc<dyn ModelDriver> = driver.clone();
        kernel
            .scheduler_step(Some(&dyn_driver), || one.due_at.max(repeat.due_at))
            .unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            while driver.calls.load(Ordering::SeqCst) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let progress = kernel.inspect_repeat(query(&repeat)).unwrap();
        assert_eq!(progress.claimed_occurrences, u32::from(offset < 0));
        assert_eq!(
            kernel.inspect_schedule(identity(&one)).unwrap().status,
            if offset < 0 {
                ScheduleStatus::Pending
            } else {
                ScheduleStatus::Running
            }
        );
        let expiry =
            due(&repeat, progress.next_occurrence.unwrap()) + chrono::Duration::seconds(30);
        kernel.scheduler_step(Some(&dyn_driver), || expiry).unwrap();
        assert_eq!(
            kernel
                .inspect_repeat(query(&repeat))
                .unwrap()
                .missed_occurrences,
            1
        );
        assert_eq!(driver.calls.load(Ordering::SeqCst), 1);
        kernel.cancel_repeat(query(&repeat)).unwrap();
        kernel.cancel_schedule(identity(&one)).unwrap();
        kernel.shutdown_agent_runs().await.unwrap();
    }
}

#[tokio::test]
async fn anchored_occurrences_have_distinct_runs_and_recover_original_evidence() {
    let root = tempfile::tempdir().unwrap();
    let cfg = config(root.path());
    let kernel = DittoKernel::open(cfg.clone()).unwrap();
    let command = repeated(3);
    kernel.repeat_schedule(command.clone()).unwrap();
    let driver = Driver::new(false);
    let dyn_driver: Arc<dyn ModelDriver> = driver.clone();
    let mut results = Vec::new();
    for ordinal in 1..=3 {
        kernel
            .scheduler_step(Some(&dyn_driver), || due(&command, ordinal))
            .unwrap();
        results.push(last_terminal(&kernel, &command).await);
        let status = kernel.inspect_repeat(query(&command)).unwrap();
        assert_eq!(status.claimed_occurrences, ordinal);
        assert_eq!(status.missed_occurrences, 0);
        assert_eq!(status.next_occurrence, (ordinal < 3).then_some(ordinal + 1));
    }
    assert_eq!(driver.calls.load(Ordering::SeqCst), 3);
    assert_eq!(
        results
            .iter()
            .map(|r| &r.run.as_ref().unwrap().request_id)
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        3
    );
    for result in &results {
        assert_eq!(result.status, ScheduleStatus::Unverified);
        let events = kernel
            .list_events(&ditto_protocol::EventQuery {
                session_id: Some(command.session_id.clone()),
                limit: Some(1000),
                ..Default::default()
            })
            .unwrap();
        crate::replay_artifact_read_turn(&events, &result.run.as_ref().unwrap().turn_id).unwrap();
        assert!(
            events
                .iter()
                .filter(|e| e.kind == OCCURRENCE_CLAIMED)
                .all(|e| e.payload.get("text").is_none() && e.payload.get("command").is_none())
        );
    }
    let final_status = kernel.inspect_repeat(query(&command)).unwrap();
    assert_eq!(final_status.status, RepeatScheduleStatus::Exhausted);
    kernel.shutdown_agent_runs().await.unwrap();
    drop(kernel);
    let kernel = DittoKernel::open(cfg).unwrap();
    assert_eq!(
        kernel.repeat_schedule(command.clone()).unwrap(),
        final_status
    );
    for result in results {
        assert_eq!(
            kernel
                .inspect_schedule(AgentRunQuery {
                    request_id: result.request_id.clone(),
                    session_id: command.session_id.clone()
                })
                .unwrap(),
            result
        );
    }
    assert!(matches!(
        kernel
            .scheduler_step(Some(&dyn_driver), || due(&command, 3))
            .unwrap(),
        Step::Wait(None)
    ));
    assert_eq!(driver.calls.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn downtime_skips_one_range_then_dispatches_only_the_eligible_occurrence() {
    let root = tempfile::tempdir().unwrap();
    let cfg = config(root.path());
    let kernel = DittoKernel::open(cfg.clone()).unwrap();
    let command = repeated(1000);
    kernel.repeat_schedule(command.clone()).unwrap();
    drop(kernel);
    let kernel = DittoKernel::open(cfg).unwrap();
    let driver = Driver::new(false);
    let dyn_driver: Arc<dyn ModelDriver> = driver.clone();
    let now = due(&command, 999) + chrono::Duration::seconds(10);
    let before = kernel.event_count().unwrap();
    kernel.scheduler_step(Some(&dyn_driver), || now).unwrap();
    assert_eq!(kernel.event_count().unwrap(), before + 1);
    let skipped = kernel.inspect_repeat(query(&command)).unwrap();
    assert_eq!(skipped.missed_occurrences, 998);
    assert_eq!(skipped.claimed_occurrences, 0);
    assert!(skipped.last_occurrence.is_none());
    assert_eq!(driver.calls.load(Ordering::SeqCst), 0);
    kernel.scheduler_step(Some(&dyn_driver), || now).unwrap();
    assert_eq!(
        last_terminal(&kernel, &command).await.status,
        ScheduleStatus::Unverified
    );
    let done = due(&command, 1000) + chrono::Duration::seconds(30);
    kernel.scheduler_step(None, || done).unwrap();
    let result = kernel.inspect_repeat(query(&command)).unwrap();
    assert_eq!(result.status, RepeatScheduleStatus::Exhausted);
    assert_eq!(
        (result.claimed_occurrences, result.missed_occurrences),
        (1, 999)
    );
    assert_eq!(driver.calls.load(Ordering::SeqCst), 1);
    let events = kernel
        .list_events(&ditto_protocol::EventQuery {
            limit: Some(1000),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|e| e.kind == OCCURRENCE_CLAIMED)
            .count(),
        1
    );
    assert_eq!(
        events.iter().filter(|e| e.kind == REPEAT_SKIPPED).count(),
        2
    );
}

#[tokio::test]
async fn claim_without_admission_consumes_one_ordinal_and_reserves_its_run_forever() {
    let root = tempfile::tempdir().unwrap();
    let cfg = config(root.path());
    let kernel = DittoKernel::open(cfg.clone()).unwrap();
    let command = repeated(2);
    kernel.repeat_schedule(command.clone()).unwrap();
    let (child, run) = claim_without_admission(&kernel, &command);
    drop(kernel);
    let kernel = DittoKernel::open(cfg).unwrap();
    let driver = Driver::new(false);
    let dyn_driver: Arc<dyn ModelDriver> = driver.clone();
    let recovered = kernel.inspect_repeat(query(&command)).unwrap();
    assert_eq!(recovered.next_occurrence, Some(2));
    assert_eq!(
        recovered.last_occurrence.unwrap().status,
        ScheduleStatus::Interrupted
    );
    assert!(matches!(
        kernel.start_agent_run(
            StartAgentRunCommand {
                request_id: run,
                session_id: command.session_id.clone(),
                text: command.text.clone(),
                sort: None
            },
            driver.clone()
        ),
        Err(AgentRunError::Conflict)
    ));
    kernel
        .scheduler_step(Some(&dyn_driver), || due(&command, 1))
        .unwrap();
    assert_eq!(driver.calls.load(Ordering::SeqCst), 0);
    kernel
        .scheduler_step(Some(&dyn_driver), || due(&command, 2))
        .unwrap();
    let later = last_terminal(&kernel, &command).await;
    assert_ne!(later.request_id, child);
    assert_eq!(later.status, ScheduleStatus::Unverified);
    assert_eq!(driver.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        kernel
            .inspect_schedule(AgentRunQuery {
                request_id: child,
                session_id: command.session_id
            })
            .unwrap()
            .status,
        ScheduleStatus::Interrupted
    );
}

#[test]
fn runtime_loss_does_not_replay_an_accepted_occurrence() {
    let root = tempfile::tempdir().unwrap();
    let cfg = config(root.path());
    let kernel = DittoKernel::open(cfg.clone()).unwrap();
    let command = repeated(2);
    kernel.repeat_schedule(command.clone()).unwrap();
    let driver = Driver::new(true);
    let dyn_driver: Arc<dyn ModelDriver> = driver.clone();
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        kernel
            .scheduler_step(Some(&dyn_driver), || due(&command, 1))
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
        kernel
            .inspect_repeat(query(&command))
            .unwrap()
            .last_occurrence
            .unwrap()
            .status,
        ScheduleStatus::Interrupted
    );
    kernel
        .scheduler_step(Some(&dyn_driver), || due(&command, 1))
        .unwrap();
    assert_eq!(driver.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn child_cancellation_preserves_series_but_parent_cancellation_stops_future_work() {
    let root = tempfile::tempdir().unwrap();
    let cfg = config(root.path());
    let kernel = DittoKernel::open(cfg.clone()).unwrap();
    let command = repeated(3);
    kernel.repeat_schedule(command.clone()).unwrap();
    let driver: Arc<dyn ModelDriver> = Driver::new(true);
    kernel
        .scheduler_step(Some(&driver), || due(&command, 1))
        .unwrap();
    let first = kernel
        .inspect_repeat(query(&command))
        .unwrap()
        .last_occurrence
        .unwrap();
    kernel
        .cancel_schedule(AgentRunQuery {
            request_id: first.request_id,
            session_id: command.session_id.clone(),
        })
        .unwrap();
    assert_eq!(
        last_terminal(&kernel, &command).await.status,
        ScheduleStatus::Failed
    );
    assert_eq!(
        kernel.inspect_repeat(query(&command)).unwrap().status,
        RepeatScheduleStatus::Active
    );
    kernel
        .scheduler_step(Some(&driver), || due(&command, 2))
        .unwrap();
    assert_eq!(
        kernel.cancel_repeat(query(&command)).unwrap().status,
        RepeatScheduleStatus::Cancelled
    );
    let last = last_terminal(&kernel, &command).await;
    assert_eq!(last.run.unwrap().failure_code.as_deref(), Some("cancelled"));
    kernel.shutdown_agent_runs().await.unwrap();
    drop(kernel);
    let kernel = DittoKernel::open(cfg).unwrap();
    let result = kernel.repeat_schedule(command.clone()).unwrap();
    assert_eq!(result.status, RepeatScheduleStatus::Cancelled);
    assert_eq!(result.claimed_occurrences, 2);
    assert!(matches!(
        kernel
            .scheduler_step(Some(&driver), || due(&command, 3))
            .unwrap(),
        Step::Wait(None)
    ));
}

#[tokio::test]
async fn last_active_occurrence_can_be_cancelled_after_timetable_exhaustion() {
    let root = tempfile::tempdir().unwrap();
    let kernel = DittoKernel::open(config(root.path())).unwrap();
    let command = repeated(2);
    kernel.repeat_schedule(command.clone()).unwrap();
    let driver: Arc<dyn ModelDriver> = Driver::new(true);
    kernel.scheduler_step(None, || due(&command, 2)).unwrap();
    kernel
        .scheduler_step(Some(&driver), || due(&command, 2))
        .unwrap();
    assert_eq!(
        kernel.inspect_repeat(query(&command)).unwrap().status,
        RepeatScheduleStatus::Exhausted
    );
    assert_eq!(
        kernel.cancel_repeat(query(&command)).unwrap().status,
        RepeatScheduleStatus::Cancelled
    );
    assert_eq!(
        last_terminal(&kernel, &command).await.status,
        ScheduleStatus::Failed
    );
}

#[test]
fn one_shots_and_repeats_share_capacity_and_retries_cannot_change_scope_or_timetable() {
    let root = tempfile::tempdir().unwrap();
    let kernel = DittoKernel::open(config(root.path())).unwrap();
    let repeat = repeated(2);
    kernel.repeat_schedule(repeat.clone()).unwrap();
    for _ in 1..MAX_PENDING_SCHEDULES {
        kernel.schedule_run(command()).unwrap();
    }
    assert!(matches!(
        kernel.repeat_schedule(repeated(2)),
        Err(AgentRunError::ScheduleFull)
    ));
    assert!(matches!(
        kernel.schedule_run(command()),
        Err(AgentRunError::ScheduleFull)
    ));
    let count = kernel.event_count().unwrap();
    kernel.repeat_schedule(repeat.clone()).unwrap();
    assert_eq!(kernel.event_count().unwrap(), count);
    for changed in [
        RepeatScheduleCommand {
            every_seconds: 120,
            ..repeat.clone()
        },
        RepeatScheduleCommand {
            occurrences: 3,
            ..repeat.clone()
        },
        RepeatScheduleCommand {
            text: "changed".into(),
            ..repeat.clone()
        },
    ] {
        assert!(matches!(
            kernel.repeat_schedule(changed),
            Err(AgentRunError::Conflict)
        ));
    }
    assert!(matches!(
        kernel.cancel_repeat(AgentRunQuery {
            request_id: repeat.request_id.clone(),
            session_id: "other".into()
        }),
        Err(AgentRunError::NotFound)
    ));
    kernel.cancel_repeat(query(&repeat)).unwrap();
    kernel.schedule_run(command()).unwrap();
    assert!(kernel.list_active_repeats("personal").unwrap().is_empty());
}

#[test]
fn invalid_repeat_parameters_fail_before_persistence_and_cache_drift_fails_closed() {
    let root = tempfile::tempdir().unwrap();
    let cfg = config(root.path());
    let kernel = DittoKernel::open(cfg.clone()).unwrap();
    let repeat = repeated(2);
    for invalid in [
        RepeatScheduleCommand {
            every_seconds: 0,
            ..repeat.clone()
        },
        RepeatScheduleCommand {
            occurrences: 1001,
            ..repeat.clone()
        },
        RepeatScheduleCommand {
            expires_at: repeat.due_at + chrono::Duration::seconds(61),
            ..repeat.clone()
        },
        RepeatScheduleCommand {
            due_at: repeat.due_at + chrono::Duration::nanoseconds(1),
            ..repeat.clone()
        },
        RepeatScheduleCommand {
            every_seconds: 31 * 86400,
            occurrences: 1000,
            ..repeat.clone()
        },
    ] {
        assert!(matches!(
            kernel.repeat_schedule(invalid),
            Err(AgentRunError::Invalid(_))
        ));
    }
    assert_eq!(kernel.event_count().unwrap(), 0);
    kernel.repeat_schedule(repeat.clone()).unwrap();
    let db = rusqlite::Connection::open(cfg.data_dir.join("state.db")).unwrap();
    db.execute("UPDATE repeat_index SET interval_ms = 0", [])
        .unwrap();
    assert!(matches!(
        kernel.inspect_repeat(query(&repeat)),
        Err(AgentRunError::Storage)
    ));
    assert!(matches!(
        kernel.scheduler_step(None, || due(&repeat, 1)),
        Err(AgentRunError::Storage)
    ));
    drop(db);
    drop(kernel);
    let kernel = DittoKernel::open(cfg).unwrap();
    assert_eq!(
        kernel
            .inspect_repeat(query(&repeat))
            .unwrap()
            .next_occurrence,
        Some(1)
    );
}

#[tokio::test]
async fn scheduler_rechecks_repeat_window_after_verification_and_skips_when_late() {
    let root = tempfile::tempdir().unwrap();
    let kernel = DittoKernel::open(config(root.path())).unwrap();
    let command = repeated(2);
    kernel.repeat_schedule(command.clone()).unwrap();
    let driver = Driver::new(false);
    let dyn_driver: Arc<dyn ModelDriver> = driver.clone();
    let clocks = AtomicUsize::new(0);
    kernel
        .scheduler_step(Some(&dyn_driver), || {
            if clocks.fetch_add(1, Ordering::SeqCst) == 0 {
                command.due_at
            } else {
                command.expires_at
            }
        })
        .unwrap();
    let result = kernel.inspect_repeat(query(&command)).unwrap();
    assert_eq!(
        (result.claimed_occurrences, result.missed_occurrences),
        (0, 1)
    );
    assert_eq!(driver.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn coherent_repeat_cache_rewind_fails_before_reexecution_and_reopen_recovers() {
    let root = tempfile::tempdir().unwrap();
    let cfg = config(root.path());
    let kernel = DittoKernel::open(cfg.clone()).unwrap();
    let repeat = repeated(2);
    kernel.repeat_schedule(repeat.clone()).unwrap();
    claim_without_admission(&kernel, &repeat);
    let count = kernel.event_count().unwrap();
    let db = rusqlite::Connection::open(cfg.data_dir.join("state.db")).unwrap();
    db.execute("UPDATE repeat_index SET last_event_id = source_event_id,next_occurrence = 1,claimed = 0,missed = 0,last_child_id = NULL,state = 'active'", []).unwrap();
    drop(db);
    let driver = Driver::new(false);
    let dyn_driver: Arc<dyn ModelDriver> = driver.clone();
    assert!(matches!(
        kernel.inspect_repeat(query(&repeat)),
        Err(AgentRunError::Storage)
    ));
    assert!(matches!(
        kernel.scheduler_step(Some(&dyn_driver), || due(&repeat, 1)),
        Err(AgentRunError::Storage)
    ));
    assert_eq!(kernel.event_count().unwrap(), count);
    assert_eq!(driver.calls.load(Ordering::SeqCst), 0);
    drop(kernel);
    let kernel = DittoKernel::open(cfg).unwrap();
    let restored = kernel.inspect_repeat(query(&repeat)).unwrap();
    assert_eq!(restored.claimed_occurrences, 1);
    assert_eq!(restored.next_occurrence, Some(2));
    assert_eq!(
        restored.last_occurrence.unwrap().status,
        ScheduleStatus::Interrupted
    );
    kernel
        .scheduler_step(Some(&dyn_driver), || due(&repeat, 2))
        .unwrap();
    assert_eq!(
        last_terminal(&kernel, &repeat).await.status,
        ScheduleStatus::Unverified
    );
    assert_eq!(driver.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn existing_owner_timer_and_slot_release_drive_repeats_without_a_second_loop() {
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
    let mut repeat = repeated(2);
    repeat.due_at = DateTime::from_timestamp_millis(Utc::now().timestamp_millis() + 150).unwrap();
    repeat.expires_at = repeat.due_at + chrono::Duration::seconds(10);
    kernel.repeat_schedule(repeat.clone()).unwrap();
    let driver = Driver::new(false);
    let dyn_driver: Arc<dyn ModelDriver> = driver.clone();
    let cloned = kernel.clone();
    let stop = CancellationToken::new();
    let token = stop.clone();
    let task = tokio::spawn(async move { cloned.run_scheduler(Some(dyn_driver), token).await });
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        kernel.inspect_repeat(query(&repeat)).unwrap().waiting_for,
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
        last_terminal(&kernel, &repeat).await.status,
        ScheduleStatus::Unverified
    );
    assert_eq!(driver.calls.load(Ordering::SeqCst), 1);
    let listed = kernel.list_active_repeats("personal").unwrap();
    assert!(listed[0].last_occurrence.is_none());
    assert!(listed[0].last_occurrence_id.is_some());
    kernel.cancel_repeat(query(&repeat)).unwrap();
    stop.cancel();
    task.await.unwrap().unwrap();
}
