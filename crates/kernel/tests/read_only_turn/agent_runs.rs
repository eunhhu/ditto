use super::*;
use ditto_kernel::AgentRunError;
use ditto_protocol::{
    AgentRunQuery, AgentRunResponse, AgentRunStatus, RememberInputCommand, StartAgentRunCommand,
};

pub(super) fn start_command(text: &str) -> StartAgentRunCommand {
    StartAgentRunCommand {
        sort: None,
        request_id: ulid::Ulid::new().to_string(),
        session_id: "personal".into(),
        text: text.into(),
    }
}

#[tokio::test]
async fn legacy_precancel_does_not_materialize_context_during_admission_split() {
    struct UnusedCandidates;
    impl IntoIterator for UnusedCandidates {
        type Item = ContextCandidate;
        type IntoIter = std::vec::IntoIter<ContextCandidate>;
        fn into_iter(self) -> Self::IntoIter {
            panic!("cancelled turn must not materialize context");
        }
    }
    let fixture = Fixture::new();
    let token = CancellationToken::new();
    token.cancel();
    let driver = ScriptedDriver::new(vec![]);
    let error = fixture
        .kernel
        .run_artifact_read_turn(
            super::command("personal", "legacy-cancel"),
            UnusedCandidates,
            &driver,
            token,
            Default::default(),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(error, TurnRunError::Failed(failure) if failure.code == TurnFailureCode::Cancelled)
    );
    assert!(driver.requests().is_empty());
}

pub(super) fn query(command: &StartAgentRunCommand) -> AgentRunQuery {
    AgentRunQuery {
        request_id: command.request_id.clone(),
        session_id: command.session_id.clone(),
    }
}

pub(super) async fn terminal(
    kernel: &DittoKernel,
    command: &StartAgentRunCommand,
) -> AgentRunResponse {
    let mut events = kernel.subscribe();
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let status = kernel.inspect_agent_run(query(command)).unwrap();
            if status.status != AgentRunStatus::Running {
                return status;
            }
            let _ = events.recv().await.unwrap();
        }
    })
    .await
    .expect("terminal event")
}

fn save_memory(
    kernel: &DittoKernel,
    session: &str,
    text: &str,
    replaces: Option<String>,
) -> String {
    let event = kernel
        .record_user_input(SubmitInputCommand {
            text: text.into(),
            session_id: Some(session.into()),
            task_id: None,
        })
        .unwrap();
    kernel
        .remember_input(RememberInputCommand {
            session_id: session.into(),
            input_event_id: event.event_id,
            replaces,
        })
        .unwrap()
        .memory_id
}

#[tokio::test]
async fn direct_answer_uses_corrected_scoped_memory_and_survives_restart() {
    let fixture = Fixture::new();
    let old = save_memory(&fixture.kernel, "personal", "cedar timezone is UTC", None);
    let corrected = save_memory(
        &fixture.kernel,
        "personal",
        "cedar timezone is KST",
        Some(old.clone()),
    );
    let irrelevant = save_memory(&fixture.kernel, "personal", "bananas ripen tomorrow", None);
    save_memory(
        &fixture.kernel,
        "elsewhere",
        "cedar timezone is private",
        None,
    );
    let driver = ScriptedDriver::new(vec![final_script(&["Cedar uses KST."])]);
    let command = start_command("cedar timezone");
    let accepted = fixture
        .kernel
        .start_agent_run(command.clone(), Arc::new(driver.clone()))
        .unwrap();
    assert_eq!(accepted.status, AgentRunStatus::Running);
    assert!(
        fixture
            .events_for_task(&accepted.task_id)
            .iter()
            .any(|event| event.kind == event_kind::INPUT_RECEIVED)
    );
    let status = terminal(&fixture.kernel, &command).await;
    assert_eq!(
        status.status,
        AgentRunStatus::Unverified,
        "{:?}",
        fixture.events_for_task(&accepted.task_id).last()
    );
    assert_eq!(status.response.as_deref(), Some("Cedar uses KST."));
    fixture.kernel.shutdown_agent_runs().await.unwrap();
    let requests = driver.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].generation.tool_use.choice, ToolChoice::Auto);
    // The whole current session set fits the budget, so it is sent complete:
    // the lexical match ranks first. Superseded and other-session memories
    // never enter the model context.
    let context = serde_json::to_string(&requests[0].turn.context).unwrap();
    assert_eq!(
        requests[0]
            .turn
            .context
            .nodes
            .iter()
            .map(|node| node.id.as_str())
            .collect::<Vec<_>>(),
        [corrected.as_str(), irrelevant.as_str()]
    );
    for excluded in [&old, "private", "is UTC"] {
        assert!(!context.contains(excluded));
    }
    let events = fixture.events_for_session("personal");
    let replay = replay_artifact_read_turn(&events, &accepted.turn_id).unwrap();
    assert!(matches!(
        replay.terminal,
        ArtifactReadTurnReplay::Finished { .. }
    ));
    assert!(
        !events
            .iter()
            .any(|event| event.kind == event_kind::TASK_COMPLETED)
    );
    let mut changed = events.clone();
    changed
        .iter_mut()
        .find(|event| event.kind == event_kind::INPUT_RECEIVED && event.correlation_id.is_some())
        .unwrap()
        .payload["agent_run"]["version"] = json!(2);
    assert!(replay_artifact_read_turn(&changed, &accepted.turn_id).is_err());
    let mut changed = events.clone();
    changed
        .iter_mut()
        .find(|event| event.kind == event_kind::MODEL_REQUESTED)
        .unwrap()
        .payload["request"]["generation"]["tool_use"]["choice"] =
        serde_json::to_value(ToolChoice::Required).unwrap();
    assert!(replay_artifact_read_turn(&changed, &accepted.turn_id).is_err());
    let config = fixture.config.clone();
    let count = fixture.kernel.event_count().unwrap();
    drop(fixture.kernel);
    let reopened = DittoKernel::open(config).unwrap();
    assert_eq!(reopened.inspect_agent_run(query(&command)).unwrap(), status);
    assert_eq!(
        reopened
            .start_agent_run(command, Arc::new(driver.clone()))
            .unwrap(),
        status
    );
    assert_eq!(reopened.event_count().unwrap(), count);
    assert_eq!(driver.requests().len(), 1);
}

#[tokio::test]
async fn agent_read_continues_and_replays_without_repeating_tool_or_model_work() {
    let fixture = Fixture::new();
    let reference = fixture.store(b"sample evidence", "personal", None);
    let driver = ScriptedDriver::new(vec![
        tool_request_script(
            "read",
            ARTIFACT_READ_ID,
            artifact_arguments(&reference, 0, 100),
            FinishReason::ToolCalls,
        ),
        final_script(&["Read sample evidence."]),
    ]);
    let command = start_command(&format!("read {reference}"));
    // Record-only input uses the task ID as correlation, not a kernel turn ID,
    // and must not steal an explicit run's durable retry identity.
    fixture
        .kernel
        .record_user_input(SubmitInputCommand {
            text: "uncorrelated task note".into(),
            session_id: Some("personal".into()),
            task_id: Some(format!("run_{}", command.request_id)),
        })
        .unwrap();
    let accepted = fixture
        .kernel
        .start_agent_run(command.clone(), Arc::new(driver.clone()))
        .unwrap();
    assert_eq!(
        terminal(&fixture.kernel, &command).await.status,
        AgentRunStatus::Unverified
    );
    fixture.kernel.shutdown_agent_runs().await.unwrap();
    let replay =
        replay_artifact_read_turn(&fixture.events_for_session("personal"), &accepted.turn_id)
            .unwrap();
    assert_eq!(replay.requests.len(), 2);
    assert_eq!(replay.calls.len(), 1);
    assert!(replay.calls[0].output.is_some());
    assert_eq!(driver.requests().len(), 2);
    assert!(
        driver.requests()[1]
            .turn
            .conversation
            .iter()
            .any(|item| matches!(
                item,
                ConversationItem::ToolResult {
                    is_error: false,
                    ..
                }
            ))
    );
}

#[derive(Clone)]
struct GatedDriver {
    descriptor: DriverDescriptor,
    started: Arc<tokio::sync::Notify>,
    release: CancellationToken,
}

impl GatedDriver {
    fn new() -> Self {
        Self {
            descriptor: ScriptedDriver::new(vec![]).descriptor,
            started: Arc::new(tokio::sync::Notify::new()),
            release: CancellationToken::new(),
        }
    }
}

impl ModelDriver for GatedDriver {
    fn descriptor(&self) -> &DriverDescriptor {
        &self.descriptor
    }
    fn stream(&self, _: ModelRequest, _: CancellationToken) -> ModelEventStream {
        self.started.notify_one();
        let release = self.release.clone();
        ModelEventStream::new(stream! {
            release.cancelled().await;
            for event in final_script(&["done"]) { yield event; }
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_retries_share_one_run_busy_is_not_queued_and_cancel_is_scoped() {
    let fixture = Fixture::new();
    let driver = Arc::new(GatedDriver::new());
    let command = start_command("cedar timezone");
    let mut handles = Vec::new();
    for _ in 0..4 {
        let kernel = fixture.kernel.clone();
        let command = command.clone();
        let driver = driver.clone();
        handles.push(tokio::spawn(async move {
            kernel.start_agent_run(command, driver).unwrap()
        }));
    }
    let mut starts = Vec::new();
    for handle in handles {
        starts.push(handle.await.unwrap());
    }
    assert!(
        starts
            .iter()
            .all(|start| start.turn_id == starts[0].turn_id)
    );
    driver.started.notified().await;
    assert_eq!(
        fixture
            .events_for_task(&starts[0].task_id)
            .iter()
            .filter(|e| e.kind == event_kind::INPUT_RECEIVED)
            .count(),
        1
    );
    let count = fixture.kernel.event_count().unwrap();
    assert!(matches!(
        fixture
            .kernel
            .start_agent_run(start_command("other"), driver.clone()),
        Err(AgentRunError::Busy)
    ));
    assert!(matches!(
        fixture.kernel.start_sort(ditto_protocol::StartSortCommand {
            request_id: ulid::Ulid::new().to_string(),
            session_id: "personal".into(),
            text: "b\na".into(),
            unique: false,
        }),
        Err(AgentRunError::Busy)
    ));
    let mut altered = command.clone();
    altered.text.push('!');
    assert!(matches!(
        fixture.kernel.start_agent_run(altered, driver.clone()),
        Err(AgentRunError::Conflict)
    ));
    let other_scope = AgentRunQuery {
        session_id: "elsewhere".into(),
        ..query(&command)
    };
    assert!(matches!(
        fixture.kernel.cancel_agent_run(other_scope),
        Err(AgentRunError::NotFound)
    ));
    assert_eq!(fixture.kernel.event_count().unwrap(), count);
    assert!(
        fixture
            .kernel
            .cancel_agent_run(query(&command))
            .unwrap()
            .cancellation_requested
    );
    let status = terminal(&fixture.kernel, &command).await;
    assert_eq!(status.status, AgentRunStatus::Failed);
    assert_eq!(status.failure_code.as_deref(), Some("cancelled"));
    fixture.kernel.shutdown_agent_runs().await.unwrap();
    replay_artifact_read_turn(&fixture.events_for_session("personal"), &starts[0].turn_id).unwrap();
}

#[tokio::test]
async fn graceful_shutdown_cancels_and_closes_admission() {
    let fixture = Fixture::new();
    let driver = Arc::new(GatedDriver::new());
    let command = start_command("hello");
    fixture
        .kernel
        .start_agent_run(command.clone(), driver.clone())
        .unwrap();
    driver.started.notified().await;
    let (first, second) = tokio::join!(
        fixture.kernel.shutdown_agent_runs(),
        fixture.kernel.shutdown_agent_runs()
    );
    first.unwrap();
    second.unwrap();
    assert_eq!(
        fixture
            .kernel
            .inspect_agent_run(query(&command))
            .unwrap()
            .failure_code
            .as_deref(),
        Some("cancelled")
    );
    assert!(matches!(
        fixture
            .kernel
            .start_agent_run(start_command("next"), driver),
        Err(AgentRunError::Stopping)
    ));
}

#[tokio::test]
async fn driver_panic_leaves_interrupted_identity_and_releases_live_slot() {
    struct PanicDriver {
        descriptor: DriverDescriptor,
        started: Arc<tokio::sync::Notify>,
    }
    impl ModelDriver for PanicDriver {
        fn descriptor(&self) -> &DriverDescriptor {
            &self.descriptor
        }
        fn stream(&self, _: ModelRequest, _: CancellationToken) -> ModelEventStream {
            self.started.notify_one();
            panic!("deliberate model-fixture panic");
        }
    }
    let fixture = Fixture::new();
    let started = Arc::new(tokio::sync::Notify::new());
    let driver = Arc::new(PanicDriver {
        descriptor: ScriptedDriver::new(vec![]).descriptor,
        started: started.clone(),
    });
    let command = start_command("interrupt");
    fixture
        .kernel
        .start_agent_run(command.clone(), driver)
        .unwrap();
    started.notified().await;
    assert_eq!(
        fixture
            .kernel
            .inspect_agent_run(query(&command))
            .unwrap()
            .status,
        AgentRunStatus::Interrupted
    );
    let healthy = ScriptedDriver::new(vec![final_script(&["new run works"])]);
    assert_eq!(
        fixture
            .kernel
            .start_agent_run(command, Arc::new(healthy.clone()))
            .unwrap()
            .status,
        AgentRunStatus::Interrupted
    );
    let next = start_command("new request");
    fixture
        .kernel
        .start_agent_run(next.clone(), Arc::new(healthy.clone()))
        .unwrap();
    assert_eq!(
        terminal(&fixture.kernel, &next).await.status,
        AgentRunStatus::Unverified
    );
    fixture.kernel.shutdown_agent_runs().await.unwrap();
    assert_eq!(healthy.requests().len(), 1);
}

#[tokio::test]
async fn interrupted_admission_is_never_automatically_restarted() {
    let fixture = Fixture::new();
    let command = start_command("recover me");
    // Crash fixture: durable admission exists, no terminal or live process owner.
    let mut input = NewEvent::user_input(
        "personal",
        Some(format!("run_{}", command.request_id)),
        &command.text,
    );
    input.correlation_id = Some(format!("turn_{}", ulid::Ulid::new()));
    input.payload["agent_run"] = json!({"version":1,"request_id":command.request_id});
    let config = fixture.config.clone();
    drop(fixture.kernel);
    let events = EventStore::open(config.data_dir.join("state.db")).unwrap();
    events.append(input).unwrap();
    drop(events);
    let kernel = DittoKernel::open(config).unwrap();
    let driver = ScriptedDriver::new(vec![]);
    let before = kernel.event_count().unwrap();
    assert_eq!(
        kernel.inspect_agent_run(query(&command)).unwrap().status,
        AgentRunStatus::Interrupted
    );
    assert_eq!(
        kernel
            .start_agent_run(command, Arc::new(driver.clone()))
            .unwrap()
            .status,
        AgentRunStatus::Interrupted
    );
    assert_eq!(kernel.event_count().unwrap(), before);
    assert!(driver.requests().is_empty());
}

#[tokio::test]
async fn input_bound_and_identity_fail_before_admission_or_model_work() {
    let fixture = Fixture::new();
    let driver = ScriptedDriver::new(vec![final_script(&["accepted"])]);
    let mut command = start_command(&"é".repeat(8192));
    let count = fixture.kernel.event_count().unwrap();
    let mut invalid = command.clone();
    invalid.text.push('x');
    assert!(matches!(
        fixture
            .kernel
            .start_agent_run(invalid, Arc::new(driver.clone())),
        Err(AgentRunError::Invalid(_))
    ));
    for request_id in ["not-an-id".to_owned(), command.request_id.to_lowercase()] {
        let mut invalid = command.clone();
        invalid.request_id = request_id;
        assert!(matches!(
            fixture
                .kernel
                .start_agent_run(invalid, Arc::new(driver.clone())),
            Err(AgentRunError::Invalid(_))
        ));
    }
    let mut invalid = command.clone();
    invalid.session_id = " personal ".into();
    assert!(matches!(
        fixture
            .kernel
            .start_agent_run(invalid, Arc::new(driver.clone())),
        Err(AgentRunError::Invalid(_))
    ));
    assert_eq!(fixture.kernel.event_count().unwrap(), count);
    fixture
        .kernel
        .start_agent_run(command.clone(), Arc::new(driver.clone()))
        .unwrap();
    assert_eq!(
        terminal(&fixture.kernel, &command).await.status,
        AgentRunStatus::Unverified
    );
    fixture.kernel.shutdown_agent_runs().await.unwrap();
    command.text = "changed".into();
    assert!(matches!(
        fixture
            .kernel
            .start_agent_run(command, Arc::new(driver.clone())),
        Err(AgentRunError::Conflict)
    ));
    assert_eq!(driver.requests().len(), 1);
}

#[tokio::test]
async fn invalid_context_source_fails_before_model_io_and_is_replayable() {
    let fixture = Fixture::new();
    let event_store = EventStore::open(fixture.config.data_dir.join("state.db")).unwrap();
    // Out-of-band writer deliberately simulates damaged canonical input. It is
    // not a supported runtime composition or a public ingress capability.
    event_store
        .append(NewEvent {
            session_id: Some("personal".into()),
            task_id: None,
            actor: EventActor::System,
            kind: event_kind::CONTEXT_NODE_RECORDED.into(),
            payload: json!({"invalid":true}),
            causation_id: None,
            correlation_id: None,
            span_id: None,
        })
        .unwrap();
    drop(event_store);
    let driver = ScriptedDriver::new(vec![]);
    let command = start_command("hello");
    let accepted = fixture
        .kernel
        .start_agent_run(command.clone(), Arc::new(driver.clone()))
        .unwrap();
    let status = terminal(&fixture.kernel, &command).await;
    assert_eq!(status.failure_code.as_deref(), Some("context_compilation"));
    assert!(driver.requests().is_empty());
    fixture.kernel.shutdown_agent_runs().await.unwrap();
    replay_artifact_read_turn(&fixture.events_for_session("personal"), &accepted.turn_id).unwrap();
}

/// Start a run once the previous run's task has released the single slot. A
/// terminal status is durable slightly before the finished task drops its guard.
async fn start_when_idle(
    kernel: &DittoKernel,
    command: &StartAgentRunCommand,
    driver: Arc<dyn ModelDriver>,
) {
    for _ in 0..10_000 {
        match kernel.start_agent_run(command.clone(), driver.clone()) {
            Err(AgentRunError::Busy) => tokio::task::yield_now().await,
            result => {
                result.unwrap();
                return;
            }
        }
    }
    panic!("the previous run never released the execution slot");
}

fn context_ids(request: &ModelRequest) -> Vec<String> {
    request
        .turn
        .context
        .nodes
        .iter()
        .map(|node| node.id.clone())
        .collect()
}

#[tokio::test]
async fn paraphrased_personal_questions_receive_the_complete_current_memory_set() {
    // Task 017 P1-P4 and the README example: none of these questions shares a
    // content word with the memory that answers it, which the original
    // positive-overlap selection could never include.
    let fixture = Fixture::new();
    let kernel = &fixture.kernel;
    let dog = save_memory(kernel, "personal", "The canine's name is Miso.", None);
    let old_club = save_memory(
        kernel,
        "personal",
        "The reading group meets at Alder Hall.",
        None,
    );
    let club = save_memory(
        kernel,
        "personal",
        "Our book club now gathers in Birch Room.",
        Some(old_club.clone()),
    );
    let key = save_memory(
        kernel,
        "personal",
        "The spare key is inside the saffron tin.",
        None,
    );
    let tin = save_memory(
        kernel,
        "personal",
        "The saffron tin sits above the fridge.",
        None,
    );
    let meeting = save_memory(kernel, "personal", "I prefer afternoon meetings", None);
    let contact = save_memory(kernel, "personal", "The household contact is Iona.", None);
    let other_contact = save_memory(kernel, "elsewhere", "The household contact is Mara.", None);
    let current =
        std::collections::BTreeSet::from([dog, club, key.clone(), tin, meeting, contact.clone()]);

    for (question, lexical_first) in [
        ("What do I call my pet dog?", None),
        (
            "Where should I go for the reading group get-together?",
            None,
        ),
        ("Where is the backup house key kept?", Some(&key)),
        ("What is my meeting preference?", None),
        ("Who is my household contact?", Some(&contact)),
    ] {
        let driver = ScriptedDriver::new(vec![final_script(&["answer"])]);
        let command = start_command(question);
        start_when_idle(kernel, &command, Arc::new(driver.clone())).await;
        let status = terminal(kernel, &command).await;
        assert_eq!(status.status, AgentRunStatus::Unverified, "{question}");
        let requests = driver.requests();
        let ids = context_ids(&requests[0]);
        assert_eq!(ids.len(), current.len(), "{question}");
        assert_eq!(
            ids.iter()
                .cloned()
                .collect::<std::collections::BTreeSet<_>>(),
            current,
            "{question}"
        );
        if let Some(first) = lexical_first {
            assert_eq!(&ids[0], first, "{question}");
        }
        let serialized = serde_json::to_string(&requests[0].turn.context).unwrap();
        for absent in [
            old_club.as_str(),
            other_contact.as_str(),
            "Alder Hall",
            "Mara",
        ] {
            assert!(!serialized.contains(absent), "{question}: {absent}");
        }
        let replay =
            replay_artifact_read_turn(&fixture.events_for_session("personal"), &status.turn_id)
                .unwrap();
        assert!(matches!(
            replay.terminal,
            ArtifactReadTurnReplay::Finished { .. }
        ));
    }
    kernel.shutdown_agent_runs().await.unwrap();
}

#[tokio::test]
async fn over_budget_sessions_keep_only_lexically_relevant_memory() {
    let fixture = Fixture::new();
    let kernel = &fixture.kernel;
    let cedar = save_memory(kernel, "personal", "cedar timezone is KST", None);
    for index in 0..16 {
        save_memory(
            kernel,
            "personal",
            &format!("synthetic unrelated pebble {index:06}"),
            None,
        );
    }

    let driver = ScriptedDriver::new(vec![final_script(&["KST"])]);
    let command = start_command("What is the cedar timezone?");
    start_when_idle(kernel, &command, Arc::new(driver.clone())).await;
    assert_eq!(
        terminal(kernel, &command).await.status,
        AgentRunStatus::Unverified
    );
    // Seventeen memories exceed the default budget, so selection falls back to
    // positive overlap without function words: noise matching only "is" is out.
    assert_eq!(context_ids(&driver.requests()[0]), [cedar.clone()]);

    // Documented boundary: at this size a pure paraphrase shares no content
    // word and receives no memory until semantic retrieval exists.
    let driver = ScriptedDriver::new(vec![final_script(&["unknown"])]);
    let command = start_command("Which clock offset applies to the lumber office?");
    start_when_idle(kernel, &command, Arc::new(driver.clone())).await;
    let status = terminal(kernel, &command).await;
    assert_eq!(status.status, AgentRunStatus::Unverified);
    assert!(context_ids(&driver.requests()[0]).is_empty());
    kernel.shutdown_agent_runs().await.unwrap();
    replay_artifact_read_turn(&fixture.events_for_session("personal"), &status.turn_id).unwrap();
}

#[tokio::test]
async fn version_one_turns_replay_with_legacy_rules_and_versions_never_mix() {
    let fixture = Fixture::new();
    let driver = ScriptedDriver::new(vec![final_script(&["hello"])]);
    let command = start_command("hello");
    fixture
        .kernel
        .start_agent_run(command.clone(), Arc::new(driver.clone()))
        .unwrap();
    let status = terminal(&fixture.kernel, &command).await;
    assert_eq!(status.status, AgentRunStatus::Unverified);
    fixture.kernel.shutdown_agent_runs().await.unwrap();
    let events = fixture.events_for_session("personal");
    let versioned = |event: &EventRecord| {
        event.correlation_id.as_deref() == Some(status.turn_id.as_str())
            && event.payload.get("event_version").is_some()
    };
    assert!(
        events
            .iter()
            .filter(|event| versioned(event))
            .all(|event| event.payload["event_version"]
                == json!(ditto_kernel::turn::TURN_PAYLOAD_VERSION))
    );
    replay_artifact_read_turn(&events, &status.turn_id).unwrap();

    // An empty capsule means the same under both selection contracts, so the
    // same transcript relabeled as version 1 replays through the legacy rules.
    let relabel = |version: u16, only_first: bool| {
        let mut relabeled = events.clone();
        for event in relabeled.iter_mut().filter(|event| versioned(event)) {
            event.payload["event_version"] = json!(version);
            if only_first {
                break;
            }
        }
        relabeled
    };
    replay_artifact_read_turn(&relabel(1, false), &status.turn_id).unwrap();
    assert!(replay_artifact_read_turn(&relabel(1, true), &status.turn_id).is_err());
    assert!(replay_artifact_read_turn(&relabel(3, false), &status.turn_id).is_err());
    assert!(replay_artifact_read_turn(&relabel(0, false), &status.turn_id).is_err());
}
