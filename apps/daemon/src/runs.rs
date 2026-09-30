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
use ditto_model_openai::{
    ChatCompletionsConfig, ChatCompletionsDriver, ChatCompletionsEndpoint, OpenAiApiKey,
    OpenAiResponsesDriver, OpenAiStoragePolicy,
};
use ditto_protocol::{AgentRunQuery, AgentRunResponse, AgentRunStatus, StartAgentRunCommand};

use super::{AppState, blocking};

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub(super) enum Provider {
    Disabled,
    Openai,
    /// Any OpenAI-compatible `/chat/completions` server (Ollama, llama.cpp,
    /// vLLM, LM Studio, OpenRouter, xAI, ...).
    OpenaiCompatible,
}

/// Operator model selection. The API key only ever comes from the environment.
#[derive(Debug, clap::Args)]
pub(super) struct ModelArgs {
    /// Enable explicit model runs. Default startup makes no model request.
    #[arg(long, env = "DITTO_PROVIDER", value_enum, default_value = "disabled")]
    pub provider: Provider,
    /// Model name for `openai-compatible`, such as `qwen2.5:7b` or `grok-4`.
    #[arg(long, env = "DITTO_MODEL")]
    pub model: Option<String>,
    /// API root for `openai-compatible`, such as `http://127.0.0.1:11434/v1`.
    /// Plain HTTP is accepted only for loopback hosts.
    #[arg(long, env = "DITTO_BASE_URL")]
    pub base_url: Option<String>,
    /// Ask an `openai-compatible` server for token usage in the stream.
    #[arg(long, env = "DITTO_INCLUDE_USAGE")]
    pub include_usage: bool,
}

pub(super) fn configured_driver(
    model: &ModelArgs,
    bind: SocketAddr,
) -> anyhow::Result<Option<Arc<dyn ModelDriver>>> {
    if !matches!(model.provider, Provider::Disabled) {
        anyhow::ensure!(
            bind.ip().is_loopback(),
            "enabled model runs require a loopback listener"
        );
    }
    match model.provider {
        Provider::Disabled => Ok(None),
        Provider::OpenaiCompatible => {
            let (Some(name), Some(base_url)) = (&model.model, &model.base_url) else {
                anyhow::bail!("openai-compatible requires --model and --base-url");
            };
            let endpoint = ChatCompletionsEndpoint::new(base_url)
                .map_err(|error| anyhow::anyhow!("--base-url: {error}"))?;
            let mut config = ChatCompletionsConfig::new(endpoint, name.clone())
                .map_err(|error| anyhow::anyhow!("--model: {error}"))?;
            if model.include_usage {
                config = config.with_usage();
            }
            let api_key = match std::env::var("DITTO_MODEL_API_KEY") {
                Ok(value) if !value.is_empty() => Some(
                    OpenAiApiKey::new(value)
                        .map_err(|_| anyhow::anyhow!("DITTO_MODEL_API_KEY is invalid"))?,
                ),
                _ => None,
            };
            let driver = ChatCompletionsDriver::new(config, api_key).map_err(|_| {
                anyhow::anyhow!("chat completions transport could not be configured")
            })?;
            Ok(Some(Arc::new(driver)))
        }
        Provider::Openai => {
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
    let response = blocking(&state.kernel, move |kernel| {
        kernel.start_agent_run(command, driver)
    })
    .await?;
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
    Ok(Json(
        blocking(&state.kernel, move |kernel| kernel.inspect_agent_run(query)).await?,
    ))
}

async fn cancel(
    State(state): State<AppState>,
    Json(query): Json<AgentRunQuery>,
) -> Result<Json<AgentRunResponse>, RunApiError> {
    Ok(Json(
        blocking(&state.kernel, move |kernel| kernel.cancel_agent_run(query)).await?,
    ))
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
                    AgentRunError::Busy | AgentRunError::ScheduleFull => {
                        StatusCode::TOO_MANY_REQUESTS
                    }
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
pub(crate) mod tests {
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

    pub(crate) struct HttpDriver {
        descriptor: DriverDescriptor,
        calls: AtomicUsize,
        reference: String,
        block: bool,
    }

    impl HttpDriver {
        pub(crate) fn new(reference: String, block: bool) -> Self {
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
        fn stream(&self, request: ModelRequest, _: CancellationToken) -> ModelEventStream {
            let index = self.calls.fetch_add(1, Ordering::SeqCst);
            let reference = self.reference.clone();
            let grant = request.turn.conversation.iter().find_map(|item| {
                let ditto_model::ConversationItem::Message { content, .. } = item else {
                    return None;
                };
                content.iter().find_map(|part| {
                    let ditto_model::ContentPart::Structured { value } = part else {
                        return None;
                    };
                    Some((
                        value["attachment"]["reference"].as_str()?.to_owned(),
                        value["user_permission"]["allow_deduplicate"].as_bool()?,
                    ))
                })
            });
            let block = self.block;
            ModelEventStream::new(async_stream::stream! {
                if block { std::future::pending::<()>().await; }
                if index == 0 {
                    let id = ProviderCallId::new("http-read").unwrap();
                    let (capability,arguments) = match grant {
                        Some((reference,unique)) => ("artifact.sort",json!({"reference":reference,"unique":unique})),
                        None => ("artifact.read",json!({"reference":reference,"offset":0,"length":32})),
                    };
                    yield ModelEvent::ToolCallStarted { call_id: id.clone(), capability_id: capability.into() };
                    yield ModelEvent::ToolCallArgumentDelta { call_id: id.clone(), delta: arguments.to_string() };
                    yield ModelEvent::ToolCallReady { call_id:id, capability_id:capability.into(), arguments };
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

    pub(crate) async fn built_cli(api: &str, args: Vec<&str>) -> std::process::Output {
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

    /// A canned OpenAI-compatible server: the first request answers with one
    /// streamed tool call, the second with final text. Bodies are captured.
    async fn compatible_server(
        reference: String,
    ) -> (String, Arc<std::sync::Mutex<Vec<serde_json::Value>>>) {
        let captured = Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = Arc::clone(&captured);
        let app = Router::new().route(
            "/v1/chat/completions",
            post(move |Json(body): Json<serde_json::Value>| {
                let log = Arc::clone(&log);
                let reference = reference.clone();
                async move {
                    let count = {
                        let mut log = log.lock().unwrap();
                        log.push(body);
                        log.len()
                    };
                    let arguments =
                        json!({"reference": reference, "offset": 0, "length": 32}).to_string();
                    let (first, second) = arguments.split_at(arguments.len() / 2);
                    let chunks = if count == 1 {
                        vec![
                            json!({"choices": [{"index": 0, "delta": {"role": "assistant", "tool_calls": [{"index": 0, "id": "call_1", "type": "function", "function": {"name": "artifact_read", "arguments": first}}]}}]}),
                            json!({"choices": [{"index": 0, "delta": {"tool_calls": [{"index": 0, "function": {"arguments": second}}]}}]}),
                            json!({"choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}]}),
                        ]
                    } else {
                        vec![
                            json!({"choices": [{"index": 0, "delta": {"content": "The file says "}}]}),
                            json!({"choices": [{"index": 0, "delta": {"content": "hello."}, "finish_reason": "stop"}]}),
                        ]
                    };
                    let mut body = chunks
                        .iter()
                        .map(|chunk| format!("data: {chunk}\n\n"))
                        .collect::<String>();
                    body.push_str("data: [DONE]\n\n");
                    ([("content-type", "text/event-stream")], body)
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/v1", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (base, captured)
    }

    #[tokio::test]
    async fn openai_compatible_driver_completes_a_tool_continuation_end_to_end() {
        let root = tempfile::tempdir().unwrap();
        let kernel = DittoKernel::open(KernelConfig::new(
            root.path().join("data"),
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../capabilities"),
        ))
        .unwrap();
        let reference = kernel
            .store_artifact(
                b"hello from a local model",
                ditto_kernel::ArtifactWriteContext {
                    session_id: Some("personal".into()),
                    mime: Some("text/plain".into()),
                    ..Default::default()
                },
            )
            .unwrap()
            .metadata
            .reference
            .to_string();
        let (base, captured) = compatible_server(reference.clone()).await;
        let driver = configured_driver(
            &model(Provider::OpenaiCompatible, Some("local-mock"), Some(&base)),
            "127.0.0.1:0".parse().unwrap(),
        )
        .unwrap()
        .unwrap();
        let command = StartAgentRunCommand {
            request_id: "01K5Z9X3Y4W5V6T7S8R9Q0P1N2".into(),
            session_id: "personal".into(),
            text: "What does the stored file say?".into(),
            sort: None,
        };
        let query = AgentRunQuery {
            request_id: command.request_id.clone(),
            session_id: "personal".into(),
        };
        let mut events = kernel.subscribe();
        kernel.start_agent_run(command, driver).unwrap();
        let status = tokio::time::timeout(std::time::Duration::from_secs(20), async {
            loop {
                let status = kernel.inspect_agent_run(query.clone()).unwrap();
                if status.status != AgentRunStatus::Running {
                    return status;
                }
                let _ = events.recv().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(status.status, AgentRunStatus::Unverified, "{status:?}");
        assert_eq!(status.response.as_deref(), Some("The file says hello."));

        let captured = captured.lock().unwrap().clone();
        assert_eq!(captured.len(), 2);
        assert_eq!(captured[0]["model"], "local-mock");
        assert_eq!(captured[0]["tools"][0]["function"]["name"], "artifact_read");
        assert_eq!(captured[0]["tool_choice"], "auto");
        assert_eq!(captured[0]["parallel_tool_calls"], false);
        let continuation = captured[1]["messages"].as_array().unwrap();
        let call = &continuation[continuation.len() - 2];
        assert_eq!(call["role"], "assistant");
        assert_eq!(call["tool_calls"][0]["id"], "call_1");
        assert_eq!(call["tool_calls"][0]["function"]["name"], "artifact_read");
        let result = &continuation[continuation.len() - 1];
        assert_eq!(result["role"], "tool");
        assert_eq!(result["tool_call_id"], "call_1");
        assert!(result["content"].as_str().unwrap().contains(&reference));

        kernel.shutdown_agent_runs().await.unwrap();
        let events = kernel
            .list_events(&EventQuery {
                session_id: Some("personal".into()),
                limit: Some(1_000),
                ..EventQuery::default()
            })
            .unwrap();
        ditto_kernel::replay_artifact_read_turn(&events, &status.turn_id).unwrap();
    }

    fn model(provider: Provider, model: Option<&str>, base_url: Option<&str>) -> ModelArgs {
        ModelArgs {
            provider,
            model: model.map(str::to_owned),
            base_url: base_url.map(str::to_owned),
            include_usage: false,
        }
    }

    #[test]
    fn provider_is_disabled_by_default_and_remote_execution_fails_before_credentials() {
        use clap::Parser;
        let args = super::super::Args::try_parse_from(["ditto-daemon"]).unwrap();
        assert!(matches!(args.model.provider, Provider::Disabled));
        assert!(!args.disable_web_fetch);
        assert!(
            super::super::Args::try_parse_from(["ditto-daemon", "--disable-web-fetch"])
                .unwrap()
                .disable_web_fetch
        );
        let args = super::super::Args::try_parse_from([
            "ditto-daemon",
            "--provider",
            "openai-compatible",
            "--base-url",
            "http://127.0.0.1:11434/v1",
            "--model",
            "qwen2.5:7b",
            "--include-usage",
        ])
        .unwrap();
        assert!(matches!(args.model.provider, Provider::OpenaiCompatible));
        assert_eq!(
            args.model.base_url.as_deref(),
            Some("http://127.0.0.1:11434/v1")
        );
        assert_eq!(args.model.model.as_deref(), Some("qwen2.5:7b"));
        assert!(args.model.include_usage);
        let loopback = "127.0.0.1:0".parse().unwrap();
        let remote = "0.0.0.0:0".parse().unwrap();
        assert!(
            configured_driver(&model(Provider::Disabled, None, None), loopback)
                .unwrap()
                .is_none()
        );
        assert!(configured_driver(&model(Provider::Openai, None, None), remote).is_err());
        let local = Some("http://127.0.0.1:11434/v1");
        assert!(
            configured_driver(&model(Provider::OpenaiCompatible, Some("m"), local), remote)
                .is_err()
        );
        // The compatible provider needs an explicit model and base URL, and
        // plain HTTP only reaches loopback servers.
        for (name, base) in [
            (None, local),
            (Some("m"), None),
            (Some("m"), Some("http://example.com/v1")),
            (Some(" m"), local),
        ] {
            assert!(
                configured_driver(&model(Provider::OpenaiCompatible, name, base), loopback)
                    .is_err()
            );
        }
        let driver = configured_driver(
            &model(Provider::OpenaiCompatible, Some("qwen2.5:7b"), local),
            loopback,
        )
        .unwrap()
        .unwrap();
        assert_eq!(driver.descriptor().id.as_str(), "openai-compatible.chat");
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
    #[tokio::test]
    #[ignore = "requires cargo build -p ditto-cli; runs model-directed OS sort via the actual CLI"]
    async fn built_cli_model_sort_permission_retry_and_disabled_status() {
        let root = tempfile::tempdir().unwrap();
        let config = KernelConfig::new(
            root.path().join("data"),
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../capabilities"),
        );
        let kernel = DittoKernel::open(config.clone()).unwrap();
        let driver = Arc::new(HttpDriver::new(String::new(), false));
        let (api, server_task) = server(kernel.clone(), Some(driver.clone())).await;
        let file = root.path().join("input.txt");
        std::fs::write(&file, b"b\na\nb").unwrap();
        let path = file.to_str().unwrap();
        let id = "01K00000000000000000000011";
        let args = vec![
            "run",
            "sort attachment",
            "--sort-file",
            path,
            "--allow-deduplicate",
            "--request-id",
            id,
        ];
        let output = built_cli(&api, args.clone()).await;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("sort attached file once; deduplication allowed")
        );
        let result: AgentRunResponse = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(result.status, AgentRunStatus::Unverified);
        assert_eq!(
            result.sort.as_ref().unwrap().state,
            ditto_protocol::AgentSortState::Verified
        );
        assert_eq!(
            result.sort.as_ref().unwrap().output.as_deref(),
            Some("a\nb\n")
        );
        let count = kernel.event_count().unwrap();
        let retry = built_cli(&api, args).await;
        assert!(retry.status.success());
        assert_eq!(
            serde_json::from_slice::<AgentRunResponse>(&retry.stdout).unwrap(),
            result
        );
        let conflict = built_cli(
            &api,
            vec![
                "run",
                "sort attachment",
                "--sort-file",
                path,
                "--request-id",
                id,
            ],
        )
        .await;
        assert!(!conflict.status.success());
        let invalid = built_cli(&api, vec!["run", "sort attachment", "--allow-deduplicate"]).await;
        assert!(!invalid.status.success());
        assert_eq!(kernel.event_count().unwrap(), count);
        assert_eq!(driver.calls.load(Ordering::SeqCst), 2);
        kernel.shutdown_agent_runs().await.unwrap();
        server_task.abort();
        let _ = server_task.await;
        drop(kernel);
        let reopened = DittoKernel::open(config).unwrap();
        let (api, server_task) = server(reopened.clone(), None).await;
        let status = built_cli(&api, vec!["run-status", id]).await;
        assert!(status.status.success());
        assert_eq!(
            serde_json::from_slice::<AgentRunResponse>(&status.stdout).unwrap(),
            result
        );
        let disabled = built_cli(&api, vec!["run", "new work", "--sort-file", path]).await;
        assert!(!disabled.status.success());
        assert_eq!(reopened.event_count().unwrap(), count);
        server_task.abort();
        let _ = server_task.await;
    }

    #[tokio::test]
    async fn http_sort_permission_cannot_supply_internal_authority_or_bypass_disabled_provider() {
        let root = tempfile::tempdir().unwrap();
        let kernel = DittoKernel::open(KernelConfig::new(
            root.path().join("data"),
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../capabilities"),
        ))
        .unwrap();
        let driver = Arc::new(HttpDriver::new(String::new(), false));
        let (api, server_task) = server(kernel.clone(), Some(driver.clone())).await;
        let client = reqwest::Client::new();
        let command = json!({"request_id":"01K00000000000000000000011","session_id":"personal","text":"sort", "sort":{"text":"b\na","allow_deduplicate":false}});
        for field in [
            "reference",
            "source_event_id",
            "lease",
            "maximum_calls",
            "actor",
        ] {
            let mut forged = command.clone();
            forged["sort"][field] = json!("forged");
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
        let mut missing = command.clone();
        missing["sort"]
            .as_object_mut()
            .unwrap()
            .remove("allow_deduplicate");
        assert_eq!(
            client
                .post(format!("{api}/v1/commands/run"))
                .json(&missing)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::UNPROCESSABLE_ENTITY
        );
        assert_eq!(driver.calls.load(Ordering::SeqCst), 0);
        assert_eq!(kernel.event_count().unwrap(), 0);
        server_task.abort();
        let _ = server_task.await;
        let (api, server_task) = server(kernel.clone(), None).await;
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
        assert_eq!(kernel.event_count().unwrap(), 0);
        server_task.abort();
        let _ = server_task.await;
    }
}
