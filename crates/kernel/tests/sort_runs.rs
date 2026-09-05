use ditto_kernel::{AgentRunError, DittoKernel, KernelConfig};
use ditto_protocol::{
    AgentRunQuery, EventQuery, SortRunResponse, SortRunStatus, StartSortCommand,
    SubmitInputCommand, event_kind,
};

fn config(data: &std::path::Path) -> KernelConfig {
    KernelConfig::new(
        data,
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../capabilities"),
    )
}
fn command(text: &str) -> StartSortCommand {
    StartSortCommand {
        request_id: ulid::Ulid::new().to_string(),
        session_id: "personal".into(),
        text: text.into(),
        unique: false,
    }
}
fn query(command: &StartSortCommand) -> AgentRunQuery {
    AgentRunQuery {
        request_id: command.request_id.clone(),
        session_id: command.session_id.clone(),
    }
}
async fn terminal(kernel: &DittoKernel, command: &StartSortCommand) -> SortRunResponse {
    let mut events = kernel.subscribe();
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let result = kernel.inspect_sort(query(command)).unwrap();
            if result.status != SortRunStatus::Running {
                return result;
            }
            events.recv().await.unwrap();
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn actual_process_verifies_artifacts_and_survives_retry_and_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let kernel = DittoKernel::open(config(dir.path())).unwrap();
    let mut command = command("z\na\na");
    command.unique = true;
    // A task-scoped ordinary note cannot take the reserved run correlation.
    kernel
        .record_user_input(SubmitInputCommand {
            text: "note".into(),
            session_id: Some("personal".into()),
            task_id: Some(format!("sort_{}", command.request_id)),
        })
        .unwrap();
    let accepted = kernel.start_sort(command.clone()).unwrap();
    assert_eq!(accepted.status, SortRunStatus::Running);
    assert_eq!(kernel.start_sort(command.clone()).unwrap(), accepted);
    let result = terminal(&kernel, &command).await;
    assert_eq!(result.status, SortRunStatus::Verified);
    assert_eq!(result.output.as_deref(), Some("a\nz\n"));
    assert_eq!(
        result.output_reference.as_deref(),
        Some(ditto_artifact_sort::input_reference(b"a\nz\n").as_str())
    );
    let count = kernel.event_count().unwrap();
    assert_eq!(kernel.start_sort(command.clone()).unwrap(), result);
    assert_eq!(kernel.event_count().unwrap(), count);
    let events = kernel
        .list_events(&EventQuery {
            session_id: Some("personal".into()),
            task_id: Some(result.task_id.clone()),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|e| e.kind == event_kind::SORT_STARTED)
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| e.kind == event_kind::TASK_COMPLETED)
            .count(),
        1
    );
    assert!(!events.iter().any(|e| e.kind == event_kind::MODEL_REQUESTED));
    assert!(
        events
            .iter()
            .all(|e| !e.payload.to_string().contains("z\\na\\na"))
    );
    kernel.shutdown_agent_runs().await.unwrap();
    drop(kernel);
    let reopened = DittoKernel::open(config(dir.path())).unwrap();
    assert_eq!(reopened.inspect_sort(query(&command)).unwrap(), result);
    assert_eq!(reopened.start_sort(command).unwrap(), result);
    assert_eq!(reopened.event_count().unwrap(), count);
}

#[tokio::test]
async fn duplicate_conflict_busy_scope_and_cancel_do_not_dispatch_extra_work() {
    let dir = tempfile::tempdir().unwrap();
    let kernel = DittoKernel::open(config(dir.path())).unwrap();
    let first = command("b\na");
    let accepted = kernel.start_sort(first.clone()).unwrap();
    let same = kernel.start_sort(first.clone()).unwrap();
    assert_eq!(accepted, same);
    let mut changed = first.clone();
    changed.unique = true;
    assert!(matches!(
        kernel.start_sort(changed),
        Err(AgentRunError::Conflict)
    ));
    let mut changed = first.clone();
    changed.text.push('\n');
    assert!(matches!(
        kernel.start_sort(changed),
        Err(AgentRunError::Conflict)
    ));
    assert!(matches!(
        kernel.start_sort(command("other")),
        Err(AgentRunError::Busy)
    ));
    let mut wrong = query(&first);
    wrong.session_id = "different".into();
    assert!(matches!(
        kernel.inspect_sort(wrong.clone()),
        Err(AgentRunError::NotFound)
    ));
    assert!(matches!(
        kernel.cancel_sort(wrong),
        Err(AgentRunError::NotFound)
    ));
    assert!(
        kernel
            .cancel_sort(query(&first))
            .unwrap()
            .cancellation_requested
    );
    let result = terminal(&kernel, &first).await;
    assert_eq!(result.status, SortRunStatus::Failed);
    assert_eq!(result.failure_code.as_deref(), Some("cancelled"));
    let count = kernel.event_count().unwrap();
    assert_eq!(kernel.start_sort(first).unwrap(), result);
    assert_eq!(kernel.event_count().unwrap(), count);
    let events = kernel.list_events(&EventQuery::default()).unwrap();
    assert!(
        !events
            .iter()
            .any(|e| e.kind == event_kind::SORT_STARTED || e.kind == event_kind::TASK_COMPLETED)
    );
    let next = command("d\nc");
    kernel.start_sort(next.clone()).unwrap();
    assert_eq!(
        terminal(&kernel, &next).await.status,
        SortRunStatus::Verified
    );
}

#[tokio::test]
async fn shutdown_cancels_sort_and_closes_shared_admission() {
    let dir = tempfile::tempdir().unwrap();
    let kernel = DittoKernel::open(config(dir.path())).unwrap();
    let request = command("b\na");
    kernel.start_sort(request.clone()).unwrap();
    let (one, two) = tokio::join!(kernel.shutdown_agent_runs(), kernel.shutdown_agent_runs());
    one.unwrap();
    two.unwrap();
    assert_eq!(
        kernel
            .inspect_sort(query(&request))
            .unwrap()
            .failure_code
            .as_deref(),
        Some("cancelled")
    );
    assert!(matches!(
        kernel.start_sort(command("a")),
        Err(AgentRunError::Stopping)
    ));
}

#[test]
fn lost_runtime_leaves_interrupted_work_and_retry_does_not_resume_it() {
    let dir = tempfile::tempdir().unwrap();
    let request = command("b\na");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let kernel = DittoKernel::open(config(dir.path())).unwrap();
    runtime.block_on(async {
        kernel.start_sort(request.clone()).unwrap();
    });
    drop(runtime);
    let count = kernel.event_count().unwrap();
    drop(kernel);
    let kernel = DittoKernel::open(config(dir.path())).unwrap();
    assert_eq!(
        kernel.inspect_sort(query(&request)).unwrap().status,
        SortRunStatus::Interrupted
    );
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let result = runtime.block_on(async { kernel.start_sort(request).unwrap() });
    assert_eq!(result.status, SortRunStatus::Interrupted);
    assert_eq!(kernel.event_count().unwrap(), count);
}

#[tokio::test]
async fn invalid_ingress_and_tampered_verifier_evidence_fail_closed() {
    let dir = tempfile::tempdir().unwrap();
    let kernel = DittoKernel::open(config(dir.path())).unwrap();
    for text in ["a".repeat(65537), "\n".repeat(4097), "a\0b".into()] {
        assert!(matches!(
            kernel.start_sort(command(&text)),
            Err(AgentRunError::Invalid(_))
        ));
    }
    let mut invalid = command("a");
    invalid.request_id.make_ascii_lowercase();
    assert!(kernel.start_sort(invalid).is_err());
    assert_eq!(kernel.event_count().unwrap(), 0);
    let request = command("b\na");
    kernel.start_sort(request.clone()).unwrap();
    let result = terminal(&kernel, &request).await;
    assert_eq!(result.status, SortRunStatus::Verified);
    let db = rusqlite::Connection::open(dir.path().join("state.db")).unwrap();
    // Deliberate offline-corruption simulation bypasses the production append-only guard.
    db.execute_batch("DROP TRIGGER events_reject_update;")
        .unwrap();
    let original: String = db
        .query_row(
            "SELECT payload_json FROM events WHERE kind = 'task.completed'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    for (field, value) in [
        ("verifier", serde_json::json!("fake")),
        ("input_lines", serde_json::json!(999)),
        ("started_event_id", serde_json::json!("missing")),
        ("exit_code", serde_json::json!(1)),
    ] {
        let mut payload: serde_json::Value = serde_json::from_str(&original).unwrap();
        payload[field] = value;
        db.execute(
            "UPDATE events SET payload_json = ? WHERE kind = 'task.completed'",
            [payload.to_string()],
        )
        .unwrap();
        assert!(matches!(
            kernel.inspect_sort(query(&request)),
            Err(AgentRunError::Storage)
        ));
    }
    db.execute(
        "UPDATE events SET payload_json = ? WHERE kind = 'task.completed'",
        [&original],
    )
    .unwrap();
    assert_eq!(kernel.inspect_sort(query(&request)).unwrap(), result);
    let reference = ditto_kernel::ArtifactRef::new(result.output_reference.unwrap()).unwrap();
    let object = dir.path().join("artifacts/sha256").join(reference.sha256());
    std::fs::write(&object, b"forged\n").unwrap();
    assert!(matches!(
        kernel.inspect_sort(query(&request)),
        Err(AgentRunError::Storage)
    ));
}
