use std::{net::SocketAddr, sync::Arc};

use axum::{
    Json, Router,
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use ditto_kernel::AgentRunError;
use ditto_model::ModelDriver;
use ditto_model_openai::{OpenAiApiKey, OpenAiResponsesDriver, OpenAiStoragePolicy};
use ditto_protocol::{AgentRunQuery, AgentRunResponse, AgentRunStatus, StartAgentRunCommand};

use super::AppState;

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub(super) enum Provider {
    Disabled,
    Openai,
}

pub(super) fn configured_driver(
    provider: Provider,
    bind: SocketAddr,
) -> anyhow::Result<Option<Arc<dyn ModelDriver>>> {
    match provider {
        Provider::Disabled => Ok(None),
        Provider::Openai => {
            anyhow::ensure!(
                bind.ip().is_loopback(),
                "enabled model runs require a loopback listener"
            );
            let value = std::env::var("OPENAI_API_KEY").map_err(|_| {
                anyhow::anyhow!("explicit OpenAI selection requires OPENAI_API_KEY")
            })?;
            let key = OpenAiApiKey::new(value)
                .map_err(|_| anyhow::anyhow!("OPENAI_API_KEY is invalid"))?;
            let driver = OpenAiResponsesDriver::gpt_5_6(
                key,
                Default::default(),
                OpenAiStoragePolicy::Ephemeral,
            )
            .map_err(|_| anyhow::anyhow!("OpenAI transport could not be configured"))?;
            Ok(Some(Arc::new(driver)))
        }
    }
}

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/commands/run", post(start))
        .route("/v1/runs", get(inspect))
        .route("/v1/commands/run/cancel", post(cancel))
}

async fn start(
    State(state): State<AppState>,
    Json(command): Json<StartAgentRunCommand>,
) -> Result<(StatusCode, Json<AgentRunResponse>), RunApiError> {
    // Inspection and cancellation remain available with the provider disabled.
    // An existing ID can be read, but POST never enables a provider implicitly.
    let driver = state.driver.ok_or(RunApiError::Disabled)?;
    let response = state.kernel.start_agent_run(command, driver)?;
    let status = if response.status == AgentRunStatus::Running {
        StatusCode::ACCEPTED
    } else {
        StatusCode::OK
    };
    Ok((status, Json(response)))
}

async fn inspect(
    State(state): State<AppState>,
    Query(query): Query<AgentRunQuery>,
) -> Result<Json<AgentRunResponse>, RunApiError> {
    Ok(Json(state.kernel.inspect_agent_run(query)?))
}

async fn cancel(
    State(state): State<AppState>,
    Json(query): Json<AgentRunQuery>,
) -> Result<Json<AgentRunResponse>, RunApiError> {
    Ok(Json(state.kernel.cancel_agent_run(query)?))
}

pub(super) enum RunApiError {
    Disabled,
    Run(AgentRunError),
}

impl From<AgentRunError> for RunApiError {
    fn from(error: AgentRunError) -> Self {
        Self::Run(error)
    }
}

impl IntoResponse for RunApiError {
    fn into_response(self) -> axum::response::Response {
        let (status, message) = match self {
            Self::Disabled => (
                StatusCode::SERVICE_UNAVAILABLE,
                "model runs are disabled; explicitly configure a provider on the daemon".to_owned(),
            ),
            Self::Run(error) => {
                let status = match error {
                    AgentRunError::Invalid(_) => StatusCode::BAD_REQUEST,
                    AgentRunError::Conflict => StatusCode::CONFLICT,
                    AgentRunError::Busy => StatusCode::TOO_MANY_REQUESTS,
                    AgentRunError::Stopping => StatusCode::SERVICE_UNAVAILABLE,
                    AgentRunError::NotFound => StatusCode::NOT_FOUND,
                    AgentRunError::Storage => StatusCode::INTERNAL_SERVER_ERROR,
                };
                (status, error.to_string())
            }
        };
        (status, Json(serde_json::json!({"error":message}))).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ditto_kernel::{DittoKernel, KernelConfig};
    use ditto_model::{
        CancellationToken, DriverDescriptor, DriverId, FinishReason, ModelEvent, ModelEventStream,
        ModelFeature, ModelRequest, ParallelToolCalls, ProviderCallId, RequestCapabilities,
        ToolChoiceKind,
    };
    use ditto_protocol::{EventQuery, event_kind};
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct HttpDriver {
        descriptor: DriverDescriptor,
        calls: AtomicUsize,
        reference: String,
        block: bool,
    }

    impl HttpDriver {
        fn new(reference: String, block: bool) -> Self {
            Self {
                descriptor: DriverDescriptor {
                    id: DriverId::new("http-test").unwrap(),
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
                reference,
                block,
            }
        }
    }

    impl ModelDriver for HttpDriver {
        fn descriptor(&self) -> &DriverDescriptor {
            &self.descriptor
        }
        fn stream(&self, _request: ModelRequest, _: CancellationToken) -> ModelEventStream {
            let index = self.calls.fetch_add(1, Ordering::SeqCst);
            let reference = self.reference.clone();
            let block = self.block;
            ModelEventStream::new(async_stream::stream! {
                if block { std::future::pending::<()>().await; }
                if index == 0 {
                    let id = ProviderCallId::new("http-read").unwrap();
                    let arguments = json!({"reference":reference,"offset":0,"length":32});
                    yield ModelEvent::ToolCallStarted { call_id: id.clone(), capability_id: "artifact.read".into() };
                    yield ModelEvent::ToolCallArgumentDelta { call_id: id.clone(), delta: arguments.to_string() };
                    yield ModelEvent::ToolCallReady { call_id:id, capability_id:"artifact.read".into(), arguments };
                    yield ModelEvent::Completed { finish_reason: FinishReason::ToolCalls, continuation: None };
                } else {
                    yield ModelEvent::TextDelta { text: "HTTP artifact answer".into() };
                    yield ModelEvent::Completed { finish_reason: FinishReason::EndTurn, continuation: None };
                }
            })
        }
    }

    async fn server(
        kernel: DittoKernel,
        driver: Option<Arc<dyn ModelDriver>>,
    ) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let api = format!("http://{}", listener.local_addr().unwrap());
        let app = routes()
            .route("/v1/commands/input", post(super::super::submit_input))
            .route("/v1/stream", get(super::super::stream_events))
            .with_state(AppState {
                kernel,
                driver,
                shutdown: CancellationToken::new(),
            });
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (api, task)
    }

    async fn built_cli(api: &str, args: Vec<&str>) -> std::process::Output {
        let cli = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("ditto");
        assert!(cli.is_file(), "build ditto-cli before this smoke test");
        let api = api.to_owned();
        let args = args.into_iter().map(str::to_owned).collect::<Vec<_>>();
        tokio::task::spawn_blocking(move || {
            std::process::Command::new(cli)
                .arg("--api")
                .arg(api)
                .args(args)
                .output()
                .unwrap()
        })
        .await
        .unwrap()
    }

    #[tokio::test]
    #[ignore = "requires cargo build -p ditto-cli; runs the actual CLI against the injected daemon router"]
    async fn built_cli_run_wait_retry_status_and_cancel() {
        let root = tempfile::tempdir().unwrap();
        let config = KernelConfig::new(
            root.path().join("data"),
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../capabilities"),
        );
        let kernel = DittoKernel::open(config).unwrap();
        let reference = kernel
            .store_artifact(
                b"CLI evidence",
                ditto_kernel::ArtifactWriteContext {
                    session_id: Some("personal".into()),
                    ..Default::default()
                },
            )
            .unwrap()
            .metadata
            .reference
            .to_string();
        let driver = Arc::new(HttpDriver::new(reference, false));
        let (api, server_task) = server(kernel.clone(), Some(driver.clone())).await;
        let id = "01K00000000000000000000003";
        let output = built_cli(&api, vec!["run", "read evidence", "--request-id", id]).await;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let result: AgentRunResponse = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(result.status, AgentRunStatus::Unverified);
        assert_eq!(result.response.as_deref(), Some("HTTP artifact answer"));
        let retry = built_cli(&api, vec!["run", "read evidence", "--request-id", id]).await;
        assert!(retry.status.success());
        assert_eq!(driver.calls.load(Ordering::SeqCst), 2);
        let status = built_cli(&api, vec!["run-status", id]).await;
        assert!(status.status.success());
        assert_eq!(
            serde_json::from_slice::<AgentRunResponse>(&status.stdout).unwrap(),
            result
        );
        let conflict = built_cli(&api, vec!["run", "changed", "--request-id", id]).await;
        assert!(!conflict.status.success());
        assert!(String::from_utf8_lossy(&conflict.stderr).contains("409"));
        kernel.shutdown_agent_runs().await.unwrap();
        server_task.abort();
        let _ = server_task.await;

        let kernel = DittoKernel::open(KernelConfig::new(
            root.path().join("cancel-data"),
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../capabilities"),
        ))
        .unwrap();
        let (api, server_task) = server(
            kernel.clone(),
            Some(Arc::new(HttpDriver::new(String::new(), true))),
        )
        .await;
        let detached = built_cli(&api, vec!["run", "wait", "--request-id", id, "--detach"]).await;
        assert!(detached.status.success());
        assert_eq!(
            serde_json::from_slice::<AgentRunResponse>(&detached.stdout)
                .unwrap()
                .status,
            AgentRunStatus::Running
        );
        let cancelled = built_cli(&api, vec!["run-cancel", id]).await;
        // Cancellation may already be terminal by the time HTTP translates it.
        let cancelled: AgentRunResponse = serde_json::from_slice(&cancelled.stdout).unwrap();
        assert!(cancelled.cancellation_requested || cancelled.status == AgentRunStatus::Failed);
        kernel.shutdown_agent_runs().await.unwrap();
        let failed = built_cli(&api, vec!["run-status", id]).await;
        assert!(!failed.status.success());
        assert_eq!(
            serde_json::from_slice::<AgentRunResponse>(&failed.stdout)
                .unwrap()
                .failure_code
                .as_deref(),
            Some("cancelled")
        );
        server_task.abort();
        let _ = server_task.await;
    }

    #[test]
    fn provider_is_disabled_by_default_and_remote_execution_fails_before_credentials() {
        use clap::Parser;
        let args = super::super::Args::try_parse_from(["ditto-daemon"]).unwrap();
        assert!(matches!(args.provider, Provider::Disabled));
        assert!(
            configured_driver(Provider::Disabled, "127.0.0.1:0".parse().unwrap())
                .unwrap()
                .is_none()
        );
        assert!(configured_driver(Provider::Openai, "0.0.0.0:0".parse().unwrap()).is_err());
    }

    #[tokio::test]
    async fn http_admission_read_continuation_retry_scope_and_disabled_restart() {
        let root = tempfile::tempdir().unwrap();
        let config = KernelConfig::new(
            root.path().join("data"),
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../capabilities"),
        );
        let kernel = DittoKernel::open(config.clone()).unwrap();
        let reference = kernel
            .store_artifact(
                b"HTTP evidence",
                ditto_kernel::ArtifactWriteContext {
                    session_id: Some("personal".into()),
                    ..Default::default()
                },
            )
            .unwrap()
            .metadata
            .reference
            .to_string();
        let driver = Arc::new(HttpDriver::new(reference, false));
        let (api, task) = server(kernel.clone(), Some(driver.clone())).await;
        let client = reqwest::Client::new();
        let command = json!({"request_id":"01K00000000000000000000001","session_id":"personal","text":"read evidence"});
        let before = kernel.event_count().unwrap();
        for field in [
            "actor",
            "kind",
            "provider",
            "context",
            "lease",
            "task_id",
            "agent_run",
        ] {
            let mut forged = command.clone();
            forged[field] = json!("forged");
            assert_eq!(
                client
                    .post(format!("{api}/v1/commands/run"))
                    .json(&forged)
                    .send()
                    .await
                    .unwrap()
                    .status(),
                StatusCode::UNPROCESSABLE_ENTITY
            );
        }
        assert_eq!(kernel.event_count().unwrap(), before);
        assert_eq!(driver.calls.load(Ordering::SeqCst), 0);
        let mut receiver = kernel.subscribe();
        let response = client
            .post(format!("{api}/v1/commands/run"))
            .json(&command)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let accepted: AgentRunResponse = response.json().await.unwrap();
        // A detached/disconnected submitter has no ownership of the task.
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                if receiver.recv().await.unwrap().kind == event_kind::TURN_FINISHED {
                    break;
                }
            }
        })
        .await
        .unwrap();
        let query = AgentRunQuery {
            request_id: accepted.request_id.clone(),
            session_id: "personal".into(),
        };
        let finished: AgentRunResponse = client
            .get(format!("{api}/v1/runs"))
            .query(&query)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(finished.status, AgentRunStatus::Unverified);
        assert_eq!(finished.response.as_deref(), Some("HTTP artifact answer"));
        assert_eq!(driver.calls.load(Ordering::SeqCst), 2);
        let count = kernel.event_count().unwrap();
        assert_eq!(
            client
                .post(format!("{api}/v1/commands/run"))
                .json(&command)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
        assert_eq!(kernel.event_count().unwrap(), count);
        let mut conflict = command.clone();
        conflict["text"] = json!("different");
        assert_eq!(
            client
                .post(format!("{api}/v1/commands/run"))
                .json(&conflict)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::CONFLICT
        );
        let other = AgentRunQuery {
            session_id: "other".into(),
            ..query.clone()
        };
        assert_eq!(
            client
                .get(format!("{api}/v1/runs"))
                .query(&other)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::NOT_FOUND
        );
        let events = kernel
            .list_events(&EventQuery {
                session_id: Some("personal".into()),
                limit: Some(1000),
                ..Default::default()
            })
            .unwrap();
        ditto_kernel::replay_artifact_read_turn(&events, &accepted.turn_id).unwrap();
        kernel.shutdown_agent_runs().await.unwrap();
        task.abort();
        let _ = task.await;
        drop(kernel);
        let reopened = DittoKernel::open(config).unwrap();
        let (api, task) = server(reopened.clone(), None).await;
        let actual: AgentRunResponse = client
            .get(format!("{api}/v1/runs"))
            .query(&query)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(actual, finished);
        assert_eq!(
            client
                .post(format!("{api}/v1/commands/run"))
                .json(&command)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        let input = json!({"text":"ordinary input","session_id":"personal"});
        assert_eq!(
            client
                .post(format!("{api}/v1/commands/input"))
                .json(&input)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::CREATED
        );
        assert_eq!(driver.calls.load(Ordering::SeqCst), 2);
        assert_eq!(reopened.event_count().unwrap(), count + 1);
        task.abort();
        let _ = task.await;
    }

    #[tokio::test]
    async fn http_busy_invalid_identity_and_cancellation_are_explicit() {
        let root = tempfile::tempdir().unwrap();
        let kernel = DittoKernel::open(KernelConfig::new(
            root.path().join("data"),
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../capabilities"),
        ))
        .unwrap();
        let driver = Arc::new(HttpDriver::new(String::new(), true));
        let (api, task) = server(kernel.clone(), Some(driver)).await;
        let client = reqwest::Client::new();
        let mut command = json!({"request_id":"invalid","session_id":"personal","text":"hello"});
        assert_eq!(
            client
                .post(format!("{api}/v1/commands/run"))
                .json(&command)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::BAD_REQUEST
        );
        command["request_id"] = json!("01K00000000000000000000001");
        assert_eq!(
            client
                .post(format!("{api}/v1/commands/run"))
                .json(&command)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::ACCEPTED
        );
        command["request_id"] = json!("01K00000000000000000000002");
        assert_eq!(
            client
                .post(format!("{api}/v1/commands/run"))
                .json(&command)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::TOO_MANY_REQUESTS
        );
        let query = AgentRunQuery {
            request_id: "01K00000000000000000000001".into(),
            session_id: "personal".into(),
        };
        let cancelled: AgentRunResponse = client
            .post(format!("{api}/v1/commands/run/cancel"))
            .json(&query)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert!(cancelled.cancellation_requested || cancelled.status == AgentRunStatus::Failed);
        kernel.shutdown_agent_runs().await.unwrap();
        let status = kernel.inspect_agent_run(query).unwrap();
        assert_eq!(status.failure_code.as_deref(), Some("cancelled"));
        task.abort();
        let _ = task.await;
    }
}
