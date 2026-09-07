use super::*;
use crate::runs::tests::{HttpDriver, built_cli};
use ditto_kernel::{DittoKernel, KernelConfig};
use ditto_model::CancellationToken;
use ditto_protocol::{ScheduleStatus, ScheduleWaitReason};
use reqwest::StatusCode;
use std::{
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

fn command(delay_ms: u64) -> ScheduleRunCommand {
    let due = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
        + delay_ms;
    ScheduleRunCommand {
        request_id: "01K00000000000000000000021".into(),
        session_id: "personal".into(),
        text: "read evidence".into(),
        due_at: (UNIX_EPOCH + Duration::from_millis(due)).into(),
        expires_at: (UNIX_EPOCH + Duration::from_millis(due + 10_000)).into(),
    }
}
fn config(root: &std::path::Path) -> KernelConfig {
    KernelConfig::new(
        root.join("data"),
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../capabilities"),
    )
}
async fn server(
    kernel: DittoKernel,
    enabled: bool,
    loopback: bool,
) -> (String, CancellationToken, tokio::task::JoinHandle<()>) {
    let driver = if enabled {
        let reference = kernel
            .store_artifact(
                b"schedule evidence",
                ditto_kernel::ArtifactWriteContext {
                    session_id: Some("personal".into()),
                    ..Default::default()
                },
            )
            .unwrap()
            .metadata
            .reference
            .to_string();
        Some(Arc::new(HttpDriver::new(reference, false)) as Arc<dyn ditto_model::ModelDriver>)
    } else {
        None
    };
    let shutdown = CancellationToken::new();
    let token = shutdown.clone();
    let app = routes(loopback).with_state(AppState {
        kernel: kernel.clone(),
        driver: driver.clone(),
        shutdown: token.clone(),
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let api = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let stop = token.clone();
        let ((), scheduler) = tokio::join!(
            async {
                axum::serve(listener, app)
                    .with_graceful_shutdown(async move { stop.cancelled().await })
                    .await
                    .unwrap();
            },
            kernel.run_scheduler(driver, token),
        );
        scheduler.unwrap();
        kernel.shutdown_agent_runs().await.unwrap();
    });
    (api, shutdown, task)
}

#[tokio::test]
async fn typed_http_rejects_authority_scope_and_remote_scheduling() {
    let root = tempfile::tempdir().unwrap();
    let kernel = DittoKernel::open(config(root.path())).unwrap();
    let (api, stop, task) = server(kernel.clone(), false, true).await;
    let client = reqwest::Client::new();
    let command = command(60_000);
    for field in [
        "actor",
        "kind",
        "provider",
        "lease",
        "sort",
        "run_request_id",
        "task_id",
        "context",
    ] {
        let mut value = serde_json::to_value(&command).unwrap();
        value[field] = serde_json::json!("forged");
        assert_eq!(
            client
                .post(format!("{api}/v1/commands/schedule"))
                .json(&value)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }
    assert_eq!(kernel.event_count().unwrap(), 0);
    let accepted: ScheduleResponse = client
        .post(format!("{api}/v1/commands/schedule"))
        .json(&command)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(accepted.status, ScheduleStatus::Pending);
    assert_eq!(
        accepted.waiting_for,
        Some(ScheduleWaitReason::ProviderDisabled)
    );
    let other = AgentRunQuery {
        request_id: command.request_id.clone(),
        session_id: "other".into(),
    };
    assert_eq!(
        client
            .get(format!("{api}/v1/schedules"))
            .query(&other)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        client
            .post(format!("{api}/v1/commands/schedule/cancel"))
            .json(&other)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    stop.cancel();
    task.await.unwrap();
    let (api, stop, task) = server(kernel, false, false).await;
    assert_eq!(
        client
            .post(format!("{api}/v1/commands/schedule"))
            .json(&command)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    stop.cancel();
    task.await.unwrap();
}

#[tokio::test]
#[ignore = "requires cargo build -p ditto-cli; actual CLI with injected model and restart"]
async fn built_cli_schedule_restart_dispatch_retry_and_cancel() {
    let root = tempfile::tempdir().unwrap();
    let cfg = config(root.path());
    let kernel = DittoKernel::open(cfg.clone()).unwrap();
    let (api, stop, task) = server(kernel.clone(), false, true).await;
    let command = command(2_000);
    let due = command.due_at.to_rfc3339();
    let expires = command.expires_at.to_rfc3339();
    let args = vec![
        "schedule",
        &command.text,
        "--request-id",
        &command.request_id,
        "--at",
        &due,
        "--expires",
        &expires,
    ];
    let result = built_cli(&api, args.clone()).await;
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let accepted: ScheduleResponse = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(accepted.status, ScheduleStatus::Pending);
    let listed = built_cli(&api, vec!["schedule-list"]).await;
    assert_eq!(
        serde_json::from_slice::<Vec<ScheduleResponse>>(&listed.stdout)
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
            let result = kernel
                .inspect_schedule(AgentRunQuery {
                    request_id: command.request_id.clone(),
                    session_id: command.session_id.clone(),
                })
                .unwrap();
            if result.status == ScheduleStatus::Unverified {
                break;
            }
            events.recv().await.unwrap();
        }
    })
    .await
    .unwrap();
    let result = built_cli(&api, vec!["schedule-status", &command.request_id]).await;
    assert!(result.status.success());
    let finished: ScheduleResponse = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(
        finished.run.as_ref().unwrap().response.as_deref(),
        Some("HTTP artifact answer")
    );
    let count = kernel.event_count().unwrap();
    let retry = built_cli(&api, args).await;
    assert!(retry.status.success());
    assert_eq!(
        serde_json::from_slice::<ScheduleResponse>(&retry.stdout).unwrap(),
        finished
    );
    assert_eq!(kernel.event_count().unwrap(), count);
    let invalid = built_cli(
        &api,
        vec![
            "schedule",
            "work",
            "--at",
            "2026-10-01T12:00:00",
            "--expires",
            &expires,
        ],
    )
    .await;
    assert!(!invalid.status.success());
    assert_eq!(kernel.event_count().unwrap(), count);
    stop.cancel();
    task.await.unwrap();
    drop(kernel);
    let kernel = DittoKernel::open(cfg).unwrap();
    let (api, stop, task) = server(kernel.clone(), false, true).await;
    let result = built_cli(&api, vec!["schedule-status", &command.request_id]).await;
    assert_eq!(
        serde_json::from_slice::<ScheduleResponse>(&result.stdout).unwrap(),
        finished
    );
    let mut next = super::tests::command(60_000);
    next.request_id = "01K00000000000000000000022".into();
    kernel.schedule_run(next.clone()).unwrap();
    let cancelled = built_cli(&api, vec!["schedule-cancel", &next.request_id]).await;
    assert!(cancelled.status.success());
    assert_eq!(
        serde_json::from_slice::<ScheduleResponse>(&cancelled.stdout)
            .unwrap()
            .status,
        ScheduleStatus::Cancelled
    );
    stop.cancel();
    task.await.unwrap();
}
