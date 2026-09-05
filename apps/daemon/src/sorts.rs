use axum::{
    Json, Router,
    extract::{Query, State},
    http::StatusCode,
    routing::{get, post},
};
use ditto_protocol::{AgentRunQuery, SortRunResponse, SortRunStatus, StartSortCommand};

use super::{AppState, runs::RunApiError};

/// This route group is composed only for a loopback listener.
pub(super) fn routes(loopback: bool) -> Router<AppState> {
    if !loopback {
        return Router::new();
    }
    Router::new()
        .route("/v1/commands/sort", post(start))
        .route("/v1/sorts", get(inspect))
        .route("/v1/commands/sort/cancel", post(cancel))
}

async fn start(
    State(state): State<AppState>,
    Json(command): Json<StartSortCommand>,
) -> Result<(StatusCode, Json<SortRunResponse>), RunApiError> {
    let result = state.kernel.start_sort(command)?;
    let code = if result.status == SortRunStatus::Running {
        StatusCode::ACCEPTED
    } else {
        StatusCode::OK
    };
    Ok((code, Json(result)))
}

async fn inspect(
    State(state): State<AppState>,
    Query(query): Query<AgentRunQuery>,
) -> Result<Json<SortRunResponse>, RunApiError> {
    Ok(Json(state.kernel.inspect_sort(query)?))
}

async fn cancel(
    State(state): State<AppState>,
    Json(query): Json<AgentRunQuery>,
) -> Result<Json<SortRunResponse>, RunApiError> {
    Ok(Json(state.kernel.cancel_sort(query)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ditto_kernel::{DittoKernel, KernelConfig};
    use ditto_protocol::event_kind;
    use serde_json::json;

    async fn server(
        loopback: bool,
    ) -> (
        tempfile::TempDir,
        DittoKernel,
        String,
        tokio::task::JoinHandle<()>,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let kernel = DittoKernel::open(KernelConfig::new(
            dir.path(),
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../capabilities"),
        ))
        .unwrap();
        let state = AppState {
            kernel: kernel.clone(),
            driver: None,
            shutdown: ditto_model::CancellationToken::new(),
        };
        let app = Router::new()
            .merge(routes(loopback))
            .route("/v1/stream", get(crate::stream_events))
            .with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async {
            axum::serve(listener, app).await.unwrap();
        });
        (dir, kernel, url, task)
    }

    #[tokio::test]
    async fn http_sort_without_provider_rejects_authority_and_remote_routes() {
        let (_dir, kernel, url, server) = server(true).await;
        let client = reqwest::Client::new();
        let command = json!({"request_id":"01K4C2EV600000000000000001","session_id":"personal","text":"b\na\na","unique":true});
        for field in [
            "program", "argv", "cwd", "env", "actor", "effect", "lease_id", "verified",
        ] {
            let mut forged = command.clone();
            forged[field] = json!("forged");
            assert_eq!(
                client
                    .post(format!("{url}/v1/commands/sort"))
                    .json(&forged)
                    .send()
                    .await
                    .unwrap()
                    .status(),
                StatusCode::UNPROCESSABLE_ENTITY
            );
        }
        assert_eq!(kernel.event_count().unwrap(), 0);
        let response = client
            .post(format!("{url}/v1/commands/sort"))
            .json(&command)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let started: SortRunResponse = response.json().await.unwrap();
        kernel.shutdown_agent_runs().await.unwrap();
        // Shutdown may win the race; either outcome must be honest and inspectable.
        let query = AgentRunQuery {
            request_id: started.request_id,
            session_id: started.session_id,
        };
        let result: SortRunResponse = client
            .get(format!("{url}/v1/sorts"))
            .query(&query)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert!(matches!(
            result.status,
            SortRunStatus::Verified | SortRunStatus::Failed
        ));
        if result.status == SortRunStatus::Verified {
            assert_eq!(result.output.as_deref(), Some("a\nb\n"));
        }
        let count = kernel.event_count().unwrap();
        let retry = client
            .post(format!("{url}/v1/commands/sort"))
            .json(&command)
            .send()
            .await
            .unwrap();
        assert_eq!(retry.status(), StatusCode::OK);
        assert_eq!(retry.json::<SortRunResponse>().await.unwrap(), result);
        assert_eq!(kernel.event_count().unwrap(), count);
        let mut changed = command.clone();
        changed["unique"] = json!(false);
        assert_eq!(
            client
                .post(format!("{url}/v1/commands/sort"))
                .json(&changed)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::CONFLICT
        );
        let wrong = AgentRunQuery {
            session_id: "other".into(),
            ..query
        };
        assert_eq!(
            client
                .get(format!("{url}/v1/sorts"))
                .query(&wrong)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            client
                .post(format!("{url}/v1/commands/sort/cancel"))
                .json(&wrong)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::NOT_FOUND
        );
        server.abort();
        let (_dir, kernel, url, server) = self::server(false).await;
        assert_eq!(
            client
                .post(format!("{url}/v1/commands/sort"))
                .json(&command)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(kernel.event_count().unwrap(), 0);
        server.abort();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "requires the CLI binary; the canonical agent-check gate builds and runs this"]
    async fn built_cli_sort_wait_retry_status_and_cancel() {
        let (dir, kernel, url, server) = server(true).await;
        let cli = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("ditto");
        assert!(cli.is_file(), "build ditto-cli before this test");
        let file = dir.path().join("list.txt");
        std::fs::write(&file, "banana\napple\nbanana").unwrap();
        let run = |args: Vec<String>| {
            let cli = cli.clone();
            let url = url.clone();
            tokio::task::spawn_blocking(move || {
                std::process::Command::new(cli)
                    .env_clear()
                    .arg("--api")
                    .arg(url)
                    .args(args)
                    .output()
                    .unwrap()
            })
        };
        let id = "01K4C2EV600000000000000002";
        let output = run(vec![
            "sort".into(),
            file.display().to_string(),
            "--unique".into(),
            "--request-id".into(),
            id.into(),
        ])
        .await
        .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let result: SortRunResponse = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(result.status, SortRunStatus::Verified);
        assert_eq!(result.output.as_deref(), Some("apple\nbanana\n"));
        let count = kernel.event_count().unwrap();
        for args in [
            vec!["sort-status".into(), id.into()],
            vec!["sort-cancel".into(), id.into()],
            vec![
                "sort".into(),
                file.display().to_string(),
                "--unique".into(),
                "--request-id".into(),
                id.into(),
            ],
        ] {
            let output = run(args).await.unwrap();
            assert!(output.status.success());
            assert_eq!(
                serde_json::from_slice::<SortRunResponse>(&output.stdout).unwrap(),
                result
            );
        }
        assert_eq!(kernel.event_count().unwrap(), count);
        let output = run(vec![
            "sort".into(),
            file.display().to_string(),
            "--request-id".into(),
            id.into(),
        ])
        .await
        .unwrap();
        assert!(!output.status.success());
        assert_eq!(kernel.event_count().unwrap(), count);
        let events = kernel.list_events(&Default::default()).unwrap();
        assert!(
            !events
                .iter()
                .any(|event| event.kind == event_kind::MODEL_REQUESTED)
        );
        kernel.shutdown_agent_runs().await.unwrap();
        server.abort();
    }
}
