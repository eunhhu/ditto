//! Offline measurement server, compiled only into the test executable. No new
//! production provider, endpoint, policy, or kernel path.
use super::*;
use ditto_model::{
    CancellationToken, ContentPart, ConversationItem, DriverDescriptor, DriverId, FinishReason,
    ModelDriver, ModelEvent, ModelEventStream, ModelFeature, ModelRequest, ParallelToolCalls,
    ProviderCallId, RequestCapabilities, ToolChoiceKind,
};
use serde_json::json;
use std::{io::Write, sync::Arc};

struct OfflineDriver {
    descriptor: DriverDescriptor,
    call_log: PathBuf,
    context_log: Option<PathBuf>,
}

impl ModelDriver for OfflineDriver {
    fn descriptor(&self) -> &DriverDescriptor {
        &self.descriptor
    }

    fn stream(&self, request: ModelRequest, _: CancellationToken) -> ModelEventStream {
        // Independent invocation count, checked against model.requested events.
        let mut log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.call_log)
            .unwrap();
        writeln!(log, "{}", request.request_id).unwrap();
        if let Some(path) = &self.context_log {
            // Opt-in synthetic-workload observation at the actual driver boundary.
            // Keep the exact capsule serialization separate from call counting.
            let mut log = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .unwrap();
            let observation = json!({
                "request_id": request.request_id,
                "context_json": serde_json::to_string(&request.turn.context).unwrap(),
            });
            writeln!(log, "{observation}").unwrap();
        }
        let continuing = request
            .turn
            .conversation
            .iter()
            .any(|item| matches!(item, ConversationItem::ToolResult { .. }));
        let mut attachment = None;
        let mut blocked = false;
        for item in &request.turn.conversation {
            if let ConversationItem::Message { content, .. } = item {
                for part in content {
                    match part {
                        ContentPart::Structured { value } => {
                            if let Some(reference) = value["attachment"]["reference"].as_str() {
                                attachment = Some((
                                    reference.to_owned(),
                                    value["user_permission"]["allow_deduplicate"] == true,
                                ));
                            }
                        }
                        ContentPart::Text { text } if text == "baseline block" => blocked = true,
                        _ => {}
                    }
                }
            }
        }
        ModelEventStream::new(stream! {
            if blocked { std::future::pending::<()>().await; }
            if let Some((reference, unique)) = attachment.filter(|_| !continuing) {
                let id = ProviderCallId::new("baseline-sort").unwrap();
                let arguments = json!({"reference": reference, "unique": unique});
                yield ModelEvent::ToolCallStarted { call_id: id.clone(), capability_id: "artifact.sort".into() };
                yield ModelEvent::ToolCallArgumentDelta { call_id: id.clone(), delta: arguments.to_string() };
                yield ModelEvent::ToolCallReady { call_id: id, capability_id: "artifact.sort".into(), arguments };
                yield ModelEvent::Completed { finish_reason: FinishReason::ToolCalls, continuation: None };
            } else {
                yield ModelEvent::TextDelta { text: "Deterministic fixture answer; quality unmeasured.\u{1b}[31m".into() };
                yield ModelEvent::Completed { finish_reason: FinishReason::EndTurn, continuation: None };
            }
        })
    }
}

#[tokio::test]
#[ignore = "offline server launched and terminated by scripts/personal-baseline.py"]
async fn fixture_server() {
    let data =
        PathBuf::from(std::env::var_os("DITTO_BASELINE_DATA").expect("baseline data required"));
    let bind = std::env::var("DITTO_BASELINE_BIND").expect("baseline bind required");
    let bind: SocketAddr = bind.parse().unwrap();
    assert!(bind.ip().is_loopback());
    let kernel = DittoKernel::open(KernelConfig::new(
        &data,
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../capabilities"),
    ))
    .unwrap();
    let driver: Arc<dyn ModelDriver> = Arc::new(OfflineDriver {
        descriptor: DriverDescriptor {
            id: DriverId::new("offline-baseline-fixture").unwrap(),
            request_capabilities: RequestCapabilities {
                tool_choices: [ToolChoiceKind::Auto].into_iter().collect(),
                parallel_tool_calls: [ParallelToolCalls::Forbid].into_iter().collect(),
                ..Default::default()
            },
            emitted_features: [ModelFeature::Text, ModelFeature::ToolCalls]
                .into_iter()
                .collect(),
        },
        call_log: data.join("fixture-calls.txt"),
        context_log: std::env::var_os("DITTO_BASELINE_OBSERVE_CONTEXT")
            .map(|_| data.join("fixture-contexts.jsonl")),
    });
    let shutdown = CancellationToken::new();
    let app = Router::new()
        .merge(memory::routes())
        .merge(runs::routes())
        .merge(schedules::routes(true))
        .merge(sorts::routes(true))
        .route("/health", get(health))
        .route("/v1/events", get(list_events))
        .route("/v1/commands/input", post(submit_input))
        .route("/v1/stream", get(stream_events))
        .with_state(AppState {
            kernel: kernel.clone(),
            driver: Some(driver.clone()),
            shutdown: shutdown.clone(),
        });
    let listener = tokio::net::TcpListener::bind(bind).await.unwrap();
    // Test-only first-process phase: explicit runs still use the fixture driver,
    // but due schedule/repeat work cannot acquire a claim until an enabled restart.
    let scheduler_driver = if std::env::var_os("DITTO_BASELINE_DISABLE_SCHEDULER_DRIVER").is_some()
    {
        None
    } else {
        Some(driver)
    };
    let (_, scheduler) = tokio::join!(
        async { axum::serve(listener, app).await.unwrap() },
        kernel.run_scheduler(scheduler_driver, shutdown),
    );
    scheduler.unwrap();
}
