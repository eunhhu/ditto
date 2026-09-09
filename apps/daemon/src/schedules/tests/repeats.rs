use super::*;
use ditto_protocol::{RepeatScheduleCommand, RepeatScheduleResponse, RepeatScheduleStatus};

fn repeated(delay_ms: u64) -> RepeatScheduleCommand {
    let base = command(delay_ms);
    RepeatScheduleCommand {
        request_id: base.request_id,
        session_id: base.session_id,
        text: base.text,
        due_at: base.due_at,
        expires_at: base.expires_at,
        every_seconds: 60,
        occurrences: 3,
    }
}

#[tokio::test]
async fn repeat_http_rejects_authority_scope_changed_retry_and_remote_routes() {
    let root = tempfile::tempdir().unwrap();
    let kernel = DittoKernel::open(config(root.path())).unwrap();
    let (api, stop, task) = server(kernel.clone(), false, true).await;
    let client = reqwest::Client::new();
    let command = repeated(60_000);
    for field in [
        "actor",
        "kind",
        "provider",
        "lease",
        "sort",
        "run_request_id",
        "task_id",
        "context",
        "next_occurrence",
        "catch_up",
    ] {
        let mut value = serde_json::to_value(&command).unwrap();
        value[field] = serde_json::json!("forged");
        assert_eq!(
            client
                .post(format!("{api}/v1/commands/repeat"))
                .json(&value)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }
    assert_eq!(kernel.event_count().unwrap(), 0);
    let accepted: RepeatScheduleResponse = client
        .post(format!("{api}/v1/commands/repeat"))
        .json(&command)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(accepted.status, RepeatScheduleStatus::Active);
    assert_eq!(
        accepted.waiting_for,
        Some(ScheduleWaitReason::ProviderDisabled)
    );
    let mut changed = command.clone();
    changed.occurrences += 1;
    assert_eq!(
        client
            .post(format!("{api}/v1/commands/repeat"))
            .json(&changed)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::CONFLICT
    );
    let other = AgentRunQuery {
        request_id: command.request_id.clone(),
        session_id: "other".into(),
    };
    assert_eq!(
        client
            .get(format!("{api}/v1/repeats"))
            .query(&other)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        client
            .post(format!("{api}/v1/commands/repeat/cancel"))
            .json(&other)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    let listed: Vec<RepeatScheduleResponse> = client
        .get(format!("{api}/v1/repeats/active"))
        .query(&ScheduleListQuery {
            session_id: "other".into(),
        })
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(listed.is_empty());
    assert_eq!(kernel.event_count().unwrap(), 1);
    stop.cancel();
    task.await.unwrap();
    let (api, stop, task) = server(kernel, false, false).await;
    for path in ["/v1/commands/repeat", "/v1/commands/repeat/cancel"] {
        assert_eq!(
            client
                .post(format!("{api}{path}"))
                .json(&command)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::NOT_FOUND
        );
    }
    for path in ["/v1/repeats", "/v1/repeats/active"] {
        assert_eq!(
            client
                .get(format!("{api}{path}"))
                .query(&other)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::NOT_FOUND
        );
    }
    stop.cancel();
    task.await.unwrap();
}

#[tokio::test]
#[ignore = "requires cargo build -p ditto-cli; actual repeat CLI with injected model and restart"]
async fn built_cli_repeat_restart_dispatch_inspect_retry_and_cancel() {
    let root = tempfile::tempdir().unwrap();
    let cfg = config(root.path());
    let kernel = DittoKernel::open(cfg.clone()).unwrap();
    let (api, stop, task) = server(kernel.clone(), false, true).await;
    let command = repeated(2_000);
    let due = command.due_at.to_rfc3339();
    let expires = command.expires_at.to_rfc3339();
    let args = vec![
        "repeat",
        &command.text,
        "--request-id",
        &command.request_id,
        "--at",
        &due,
        "--expires",
        &expires,
        "--every-seconds",
        "60",
        "--occurrences",
        "3",
    ];
    let accepted = built_cli(&api, args.clone()).await;
    assert!(
        accepted.status.success(),
        "{}",
        String::from_utf8_lossy(&accepted.stderr)
    );
    let accepted: RepeatScheduleResponse = serde_json::from_slice(&accepted.stdout).unwrap();
    assert_eq!(accepted.status, RepeatScheduleStatus::Active);
    assert_eq!(accepted.claimed_occurrences, 0);
    let listed = built_cli(&api, vec!["repeat-list"]).await;
    assert_eq!(
        serde_json::from_slice::<Vec<RepeatScheduleResponse>>(&listed.stdout)
            .unwrap()
            .len(),
        1
    );
    stop.cancel();
    task.await.unwrap();
    drop(kernel);

    let kernel = DittoKernel::open(cfg.clone()).unwrap();
    let mut events = kernel.subscribe();
    let (api, stop, task) = server(kernel.clone(), true, true).await;
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let response = kernel
                .inspect_repeat(AgentRunQuery {
                    request_id: command.request_id.clone(),
                    session_id: command.session_id.clone(),
                })
                .unwrap();
            if response
                .last_occurrence
                .as_ref()
                .is_some_and(|child| child.status == ScheduleStatus::Unverified)
            {
                break;
            }
            events.recv().await.unwrap();
        }
    })
    .await
    .unwrap();
    let result = built_cli(&api, vec!["repeat-status", &command.request_id]).await;
    assert!(result.status.success());
    let finished: RepeatScheduleResponse = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(finished.status, RepeatScheduleStatus::Active);
    assert_eq!(finished.claimed_occurrences, 1);
    assert_eq!(finished.next_occurrence, Some(2));
    let child = finished.last_occurrence.as_ref().unwrap();
    assert_eq!(
        child.run.as_ref().unwrap().response.as_deref(),
        Some("HTTP artifact answer")
    );
    let original = built_cli(&api, vec!["schedule-status", &child.request_id]).await;
    assert_eq!(
        &serde_json::from_slice::<ScheduleResponse>(&original.stdout).unwrap(),
        child
    );
    let listed = built_cli(&api, vec!["repeat-list"]).await;
    let listed: Vec<RepeatScheduleResponse> = serde_json::from_slice(&listed.stdout).unwrap();
    assert_eq!(listed[0].last_occurrence_id, finished.last_occurrence_id);
    assert!(listed[0].last_occurrence.is_none());
    let count = kernel.event_count().unwrap();
    let retry = built_cli(&api, args.clone()).await;
    assert!(retry.status.success());
    assert_eq!(
        serde_json::from_slice::<RepeatScheduleResponse>(&retry.stdout).unwrap(),
        finished
    );
    assert_eq!(kernel.event_count().unwrap(), count);
    let mut invalid_args = args.clone();
    *invalid_args.last_mut().unwrap() = "4";
    let conflict = built_cli(&api, invalid_args).await;
    assert!(!conflict.status.success());
    assert!(String::from_utf8_lossy(&conflict.stderr).contains("409"));
    let invalid = built_cli(
        &api,
        vec![
            "repeat",
            "work",
            "--at",
            "2026-10-01T12:00:00",
            "--expires",
            &expires,
            "--every-seconds",
            "60",
            "--occurrences",
            "3",
        ],
    )
    .await;
    assert!(!invalid.status.success());
    assert_eq!(kernel.event_count().unwrap(), count);
    stop.cancel();
    task.await.unwrap();
    drop(kernel);

    let kernel = DittoKernel::open(cfg.clone()).unwrap();
    let (api, stop, task) = server(kernel.clone(), false, true).await;
    let inspected = built_cli(&api, vec!["repeat-status", &command.request_id]).await;
    let inspected: RepeatScheduleResponse = serde_json::from_slice(&inspected.stdout).unwrap();
    assert_eq!(inspected.last_occurrence, finished.last_occurrence);
    assert_eq!(inspected.claimed_occurrences, 1);
    let cancelled = built_cli(&api, vec!["repeat-cancel", &command.request_id]).await;
    assert!(cancelled.status.success());
    let cancelled: RepeatScheduleResponse = serde_json::from_slice(&cancelled.stdout).unwrap();
    assert_eq!(cancelled.status, RepeatScheduleStatus::Cancelled);
    assert!(cancelled.next_due_at.is_none());
    assert_eq!(cancelled.last_occurrence, finished.last_occurrence);
    let count = kernel.event_count().unwrap();
    assert_eq!(
        serde_json::from_slice::<RepeatScheduleResponse>(&built_cli(&api, args).await.stdout)
            .unwrap(),
        cancelled
    );
    assert_eq!(kernel.event_count().unwrap(), count);
    let listed = built_cli(&api, vec!["repeat-list"]).await;
    assert!(
        serde_json::from_slice::<Vec<RepeatScheduleResponse>>(&listed.stdout)
            .unwrap()
            .is_empty()
    );
    stop.cancel();
    task.await.unwrap();
    drop(kernel);
    let kernel = DittoKernel::open(cfg).unwrap();
    assert_eq!(
        kernel
            .inspect_repeat(AgentRunQuery {
                request_id: command.request_id,
                session_id: command.session_id
            })
            .unwrap(),
        cancelled
    );
}
