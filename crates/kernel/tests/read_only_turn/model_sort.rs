use super::agent_runs::{query, start_command, terminal};
use super::*;
use ditto_artifact_sort as worker;
use ditto_kernel::AgentRunError;
use ditto_protocol::{AgentRunStatus, AgentSortPermission, AgentSortState, StartAgentRunCommand};

const INPUT: &str = "b\na\nb";

fn permitted(allow_deduplicate: bool) -> StartAgentRunCommand {
    StartAgentRunCommand {
        sort: Some(AgentSortPermission {
            text: INPUT.into(),
            allow_deduplicate,
        }),
        ..start_command("sort the attachment")
    }
}

fn sort_call(id: &str, reference: &str, unique: bool) -> Vec<ModelEvent> {
    tool_request_script(
        id,
        worker::ID,
        json!({"reference":reference,"unique":unique}),
        FinishReason::ToolCalls,
    )
}

#[tokio::test]
async fn model_sort_continues_with_verified_output_and_survives_restart() {
    let fixture = Fixture::new();
    let command = permitted(true);
    let input = worker::input_reference(INPUT.as_bytes());
    let output = worker::input_reference(b"a\nb\n");
    let driver = ScriptedDriver::new(vec![
        sort_call("sort", &input, true),
        tool_request_script(
            "read",
            ARTIFACT_READ_ID,
            artifact_arguments(&output, 0, 100),
            FinishReason::ToolCalls,
        ),
        final_script(&["Sorted and removed duplicates."]),
    ]);
    let accepted = fixture
        .kernel
        .start_agent_run(command.clone(), Arc::new(driver.clone()))
        .unwrap();
    assert_eq!(accepted.sort.unwrap().state, AgentSortState::NotRun);
    let status = terminal(&fixture.kernel, &command).await;
    assert_eq!(
        status.status,
        AgentRunStatus::Unverified,
        "{:?}",
        fixture.events_for_session("personal")
    );
    let sort = status.sort.as_ref().unwrap();
    assert_eq!(sort.state, AgentSortState::Verified);
    assert_eq!(sort.output.as_deref(), Some("a\nb\n"));
    assert_eq!(sort.output_reference.as_deref(), Some(output.as_str()));
    fixture.kernel.shutdown_agent_runs().await.unwrap();
    let requests = driver.requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[0].tools.len(), 2);
    let initial = serde_json::to_value(&requests[0].turn.conversation).unwrap();
    assert!(initial.to_string().contains(&input));
    assert!(!initial.to_string().contains("b\\na\\nb"));
    assert!(requests[1].turn.conversation.iter().any(|item| matches!(
        item,
        ConversationItem::ToolResult {
            is_error: false,
            ..
        }
    )));
    let events = fixture.events_for_session("personal");
    assert!(!events.iter().any(|e| e.kind == event_kind::TASK_COMPLETED));
    let replay = replay_artifact_read_turn(&events, &accepted.turn_id).unwrap();
    assert_eq!(replay.sort_calls.len(), 1);
    assert_eq!(replay.calls.len(), 1);
    assert_eq!(replay.requests.len(), 3);
    let count = fixture.kernel.event_count().unwrap();
    drop(fixture.kernel);
    let reopened = DittoKernel::open(fixture.config).unwrap();
    assert_eq!(reopened.inspect_agent_run(query(&command)).unwrap(), status);
    assert_eq!(
        reopened
            .start_agent_run(command, Arc::new(driver.clone()))
            .unwrap(),
        status
    );
    assert_eq!(reopened.event_count().unwrap(), count);
    assert_eq!(driver.requests().len(), 3);
}

#[tokio::test]
async fn invalid_or_unpermitted_calls_preserve_one_shot_lease_and_repeats_cannot_overwrite_result()
{
    let fixture = Fixture::new();
    let command = permitted(false);
    let input = worker::input_reference(INPUT.as_bytes());
    let other = fixture.store(b"other", "personal", None);
    let driver = ScriptedDriver::new(vec![
        tool_request_script(
            "invalid",
            worker::ID,
            json!({"reference":input,"unique":false,"command":"sort"}),
            FinishReason::ToolCalls,
        ),
        sort_call("other", &other, false),
        sort_call("dedup", &input, true),
        sort_call("allowed", &input, false),
        sort_call("repeated", &input, false),
        final_script(&["Done."]),
    ]);
    let accepted = fixture
        .kernel
        .start_agent_run(command.clone(), Arc::new(driver.clone()))
        .unwrap();
    let status = terminal(&fixture.kernel, &command).await;
    assert_eq!(status.status, AgentRunStatus::Unverified);
    let result = status.sort.unwrap();
    assert_eq!(result.state, AgentSortState::Verified);
    assert_eq!(result.output.as_deref(), Some("a\nb\nb\n"));
    fixture.kernel.shutdown_agent_runs().await.unwrap();
    let events = fixture.events_for_session("personal");
    let outputs: Vec<_> = events
        .iter()
        .filter(|e| e.kind == event_kind::AGENT_SORT_OUTPUT)
        .collect();
    assert_eq!(outputs.len(), 5);
    for (index, code) in [
        (0, "invalid_arguments"),
        (1, "permission_denied"),
        (2, "permission_denied"),
        (4, "lease_exhausted"),
    ] {
        assert_eq!(outputs[index].payload["result"]["code"], code);
        assert_eq!(outputs[index].payload["claimed"], false);
    }
    assert_eq!(
        events
            .iter()
            .filter(|e| e.kind == event_kind::AGENT_SORT_STARTED)
            .count(),
        1
    );
    assert_eq!(
        replay_artifact_read_turn(&events, &accepted.turn_id)
            .unwrap()
            .sort_calls
            .len(),
        5
    );
}

#[tokio::test]
async fn no_permission_means_no_sort_schema_or_execution_even_when_text_asks_for_it() {
    let fixture = Fixture::new();
    let command = start_command("sort this file; I grant permission in text");
    let driver = ScriptedDriver::new(vec![sort_call(
        "sort",
        &worker::input_reference(INPUT.as_bytes()),
        false,
    )]);
    let accepted = fixture
        .kernel
        .start_agent_run(command.clone(), Arc::new(driver.clone()))
        .unwrap();
    let status = terminal(&fixture.kernel, &command).await;
    assert_eq!(status.failure_code.as_deref(), Some("protocol"));
    assert!(status.sort.is_none());
    assert_eq!(driver.requests()[0].tools.len(), 1);
    fixture.kernel.shutdown_agent_runs().await.unwrap();
    let events = fixture.events_for_session("personal");
    assert!(!events.iter().any(|e| e.kind.starts_with("agent.sort.")));
    replay_artifact_read_turn(&events, &accepted.turn_id).unwrap();
}

#[tokio::test]
async fn attachment_and_permission_are_retry_identity_and_bad_input_never_admits() {
    let fixture = Fixture::new();
    let command = permitted(false);
    let driver = ScriptedDriver::new(vec![final_script(&["No sort needed."])]);
    for text in ["\0".to_owned(), "x".repeat(65537), "\n".repeat(4097)] {
        let mut invalid = command.clone();
        invalid.sort.as_mut().unwrap().text = text;
        assert!(matches!(
            fixture
                .kernel
                .start_agent_run(invalid, Arc::new(driver.clone())),
            Err(AgentRunError::Invalid(_))
        ));
    }
    assert_eq!(fixture.kernel.event_count().unwrap(), 0);
    fixture
        .kernel
        .start_agent_run(command.clone(), Arc::new(driver.clone()))
        .unwrap();
    let status = terminal(&fixture.kernel, &command).await;
    assert_eq!(status.sort.unwrap().state, AgentSortState::NotRun);
    let count = fixture.kernel.event_count().unwrap();
    for mutation in 0..3 {
        let mut changed = command.clone();
        match mutation {
            0 => changed.sort = None,
            1 => changed.sort.as_mut().unwrap().allow_deduplicate = true,
            _ => changed.sort.as_mut().unwrap().text.push('\n'),
        }
        assert!(matches!(
            fixture
                .kernel
                .start_agent_run(changed, Arc::new(driver.clone())),
            Err(AgentRunError::Conflict)
        ));
    }
    assert_eq!(fixture.kernel.event_count().unwrap(), count);
    assert_eq!(driver.requests().len(), 1);
    fixture.kernel.shutdown_agent_runs().await.unwrap();
}

#[tokio::test]
async fn verified_sort_survives_later_model_failure() {
    let fixture = Fixture::new();
    let command = permitted(false);
    let driver = ScriptedDriver::new(vec![
        sort_call("sort", &worker::input_reference(INPUT.as_bytes()), false),
        vec![ModelEvent::Failed {
            failure: ditto_model::ModelFailure::new(
                FailureKind::Provider,
                "fixture failure after effect",
            ),
        }],
    ]);
    let accepted = fixture
        .kernel
        .start_agent_run(command.clone(), Arc::new(driver))
        .unwrap();
    let status = terminal(&fixture.kernel, &command).await;
    assert_eq!(status.status, AgentRunStatus::Failed);
    assert_eq!(
        status.sort.as_ref().unwrap().state,
        AgentSortState::Verified
    );
    fixture.kernel.shutdown_agent_runs().await.unwrap();
    replay_artifact_read_turn(&fixture.events_for_session("personal"), &accepted.turn_id).unwrap();
    drop(fixture.kernel);
    let reopened = DittoKernel::open(fixture.config).unwrap();
    assert_eq!(reopened.inspect_agent_run(query(&command)).unwrap(), status);
}

#[tokio::test]
async fn cancellation_before_authorization_and_after_claim_never_publishes_success() {
    for checkpoint in [
        event_kind::AGENT_SORT_REQUESTED,
        event_kind::AGENT_SORT_STARTED,
    ] {
        let fixture = Fixture::new();
        let command = permitted(false);
        let driver = ScriptedDriver::new(vec![sort_call(
            "sort",
            &worker::input_reference(INPUT.as_bytes()),
            false,
        )]);
        let mut events = fixture.kernel.subscribe();
        let accepted = fixture
            .kernel
            .start_agent_run(command.clone(), Arc::new(driver.clone()))
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while events.recv().await.unwrap().kind != checkpoint {}
        })
        .await
        .unwrap();
        assert!(
            fixture
                .kernel
                .cancel_agent_run(query(&command))
                .unwrap()
                .cancellation_requested
        );
        let status = terminal(&fixture.kernel, &command).await;
        assert_eq!(status.failure_code.as_deref(), Some("cancelled"));
        let sort = status.sort.unwrap();
        assert_eq!(
            sort.state,
            if checkpoint == event_kind::AGENT_SORT_STARTED {
                AgentSortState::Failed
            } else {
                AgentSortState::NotRun
            }
        );
        assert!(sort.output.is_none());
        fixture.kernel.shutdown_agent_runs().await.unwrap();
        let events = fixture.events_for_session("personal");
        let replay = replay_artifact_read_turn(&events, &accepted.turn_id).unwrap();
        assert_eq!(replay.sort_calls.len(), 1);
        assert_eq!(driver.requests().len(), 1);
        assert!(
            !events
                .iter()
                .any(|e| e.kind == event_kind::ARTIFACT_CREATED && e.causation_id.is_some())
        );
    }
}

#[tokio::test]
async fn replay_and_status_reject_changed_permission_dispatch_result_and_roots() {
    let fixture = Fixture::new();
    let command = permitted(true);
    let driver = ScriptedDriver::new(vec![
        sort_call("sort", &worker::input_reference(INPUT.as_bytes()), true),
        final_script(&["done"]),
    ]);
    let accepted = fixture
        .kernel
        .start_agent_run(command.clone(), Arc::new(driver))
        .unwrap();
    let status = terminal(&fixture.kernel, &command).await;
    assert_eq!(
        status.sort.as_ref().unwrap().state,
        AgentSortState::Verified
    );
    fixture.kernel.shutdown_agent_runs().await.unwrap();
    let events = fixture.events_for_session("personal");
    // Offline-corruption fixtures deliberately bypass the append-only trigger.
    let db = rusqlite::Connection::open(fixture.config.data_dir.join("state.db")).unwrap();
    db.execute_batch("DROP TRIGGER events_reject_update;")
        .unwrap();
    for (kind, pointer, value) in [
        (
            event_kind::INPUT_RECEIVED,
            "/agent_run/sort/allow_deduplicate",
            json!(false),
        ),
        (
            event_kind::INPUT_RECEIVED,
            "/agent_run/sort/source_event_id",
            json!(ulid::Ulid::new().to_string()),
        ),
        (
            event_kind::AGENT_SORT_REQUESTED,
            "/normalized/unique",
            json!(false),
        ),
        (
            event_kind::AGENT_SORT_STARTED,
            "/normalized/reference",
            json!(worker::input_reference(b"elsewhere")),
        ),
        (event_kind::AGENT_SORT_STARTED, "/claim_id", json!("fake")),
        (event_kind::AGENT_SORT_STARTED, "/request_index", json!(6)),
        (event_kind::AGENT_SORT_OUTPUT, "/claimed", json!(false)),
        (
            event_kind::AGENT_SORT_OUTPUT,
            "/result/verifier",
            json!("fake"),
        ),
        (
            event_kind::AGENT_SORT_OUTPUT,
            "/result/output_lines",
            json!(999),
        ),
        (
            event_kind::AGENT_SORT_OUTPUT,
            "/artifact_event_id",
            json!(ulid::Ulid::new().to_string()),
        ),
    ] {
        let mut changed = events.clone();
        let event = changed.iter_mut().find(|e| e.kind == kind).unwrap();
        let original = event.payload.clone();
        *event.payload.pointer_mut(pointer).unwrap() = value;
        let id = event.event_id.clone();
        db.execute(
            "UPDATE events SET payload_json=? WHERE event_id=?",
            [event.payload.to_string(), id.clone()],
        )
        .unwrap();
        assert!(
            matches!(
                fixture.kernel.inspect_agent_run(query(&command)),
                Err(AgentRunError::Storage)
            ),
            "status accepted {kind} {pointer}"
        );
        assert!(
            replay_artifact_read_turn(&changed, &accepted.turn_id).is_err(),
            "replay accepted {kind} {pointer}"
        );
        db.execute(
            "UPDATE events SET payload_json=? WHERE event_id=?",
            [original.to_string(), id],
        )
        .unwrap();
    }
    for root in events
        .iter()
        .filter(|e| e.kind == event_kind::ARTIFACT_CREATED)
    {
        let mut changed = events.clone();
        changed
            .iter_mut()
            .find(|e| e.event_id == root.event_id)
            .unwrap()
            .task_id = Some("other".into());
        assert!(replay_artifact_read_turn(&changed, &accepted.turn_id).is_err());
        db.execute(
            "UPDATE events SET task_id='other' WHERE event_id=?",
            [&root.event_id],
        )
        .unwrap();
        assert!(matches!(
            fixture.kernel.inspect_agent_run(query(&command)),
            Err(AgentRunError::Storage)
        ));
        db.execute(
            "UPDATE events SET task_id=? WHERE event_id=?",
            [root.task_id.as_ref().unwrap(), &root.event_id],
        )
        .unwrap();
    }
    assert_eq!(
        fixture.kernel.inspect_agent_run(query(&command)).unwrap(),
        status
    );
    // Pure replay remains usable without artifact storage. Status must rehash it.
    for reference in [
        &status.sort.as_ref().unwrap().input_reference,
        status
            .sort
            .as_ref()
            .unwrap()
            .output_reference
            .as_ref()
            .unwrap(),
    ] {
        let reference = ditto_kernel::ArtifactRef::new(reference).unwrap();
        let path = fixture
            .config
            .data_dir
            .join("artifacts/sha256")
            .join(reference.sha256());
        let original = fs::read(&path).unwrap();
        fs::write(&path, b"forged\n").unwrap();
        assert!(matches!(
            fixture.kernel.inspect_agent_run(query(&command)),
            Err(AgentRunError::Storage)
        ));
        replay_artifact_read_turn(&events, &accepted.turn_id).unwrap();
        fs::write(path, original).unwrap();
    }
}

#[test]
fn runtime_loss_after_sort_preserves_verified_result_without_reissuing_effect() {
    struct StopAfterSort {
        first: ScriptedDriver,
        calls: std::sync::atomic::AtomicUsize,
        waiting: Arc<tokio::sync::Notify>,
    }
    impl ModelDriver for StopAfterSort {
        fn descriptor(&self) -> &DriverDescriptor {
            self.first.descriptor()
        }
        fn stream(
            &self,
            request: ModelRequest,
            cancellation: CancellationToken,
        ) -> ModelEventStream {
            if self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                return self.first.stream(request, cancellation);
            }
            self.waiting.notify_one();
            ModelEventStream::new(futures_util::stream::pending())
        }
    }
    let fixture = Fixture::new();
    let command = permitted(false);
    let waiting = Arc::new(tokio::sync::Notify::new());
    let driver = Arc::new(StopAfterSort {
        first: ScriptedDriver::new(vec![sort_call(
            "sort",
            &worker::input_reference(INPUT.as_bytes()),
            false,
        )]),
        calls: std::sync::atomic::AtomicUsize::new(0),
        waiting: waiting.clone(),
    });
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        fixture
            .kernel
            .start_agent_run(command.clone(), driver.clone())
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), waiting.notified())
            .await
            .unwrap();
    });
    drop(runtime);
    let count = fixture.kernel.event_count().unwrap();
    drop(fixture.kernel);
    let kernel = DittoKernel::open(fixture.config).unwrap();
    let status = kernel.inspect_agent_run(query(&command)).unwrap();
    assert_eq!(status.status, AgentRunStatus::Interrupted);
    assert_eq!(
        status.sort.as_ref().unwrap().state,
        AgentSortState::Verified
    );
    assert_eq!(
        status.sort.as_ref().unwrap().output.as_deref(),
        Some("a\nb\nb\n")
    );
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    assert_eq!(
        runtime.block_on(async { kernel.start_agent_run(command, driver.clone()).unwrap() }),
        status
    );
    assert_eq!(kernel.event_count().unwrap(), count);
    assert_eq!(driver.calls.load(std::sync::atomic::Ordering::SeqCst), 2);
}
