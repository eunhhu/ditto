use std::{convert::Infallible, net::SocketAddr, path::PathBuf, time::Duration};

use anyhow::Context;
use async_stream::stream;
use axum::{
    Json, Router,
    extract::{Query, State},
    http::StatusCode,
    response::{
        IntoResponse,
        sse::{Event as SseEvent, KeepAlive, Sse},
    },
    routing::{get, post},
};
use clap::Parser;
use ditto_kernel::{DittoKernel, KernelConfig, KernelError};
use ditto_protocol::{
    AppendEventResponse, CapabilitySearchQuery, EventQuery, EventRecord, HealthResponse, NewEvent,
};
use futures_core::Stream;
use tokio::sync::broadcast::error::RecvError;
use tower_http::trace::TraceLayer;
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(name = "ditto-daemon", version, about = "Ditto semantic agent microkernel")]
struct Args {
    #[arg(long, env = "DITTO_DATA_DIR", default_value = ".ditto")]
    data_dir: PathBuf,
    #[arg(
        long,
        env = "DITTO_CAPABILITIES_DIR",
        default_value = "capabilities"
    )]
    capabilities_dir: PathBuf,
    #[arg(long, env = "DITTO_BIND", default_value = "127.0.0.1:8787")]
    bind: SocketAddr,
}

#[derive(Clone)]
struct AppState {
    kernel: DittoKernel,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("ditto=info")),
        )
        .with_target(false)
        .compact()
        .init();

    let args = Args::parse();
    let kernel = DittoKernel::open(KernelConfig::new(
        args.data_dir,
        args.capabilities_dir,
    ))
    .context("failed to initialize Ditto kernel")?;
    kernel
        .record_runtime_started(&args.bind.to_string())
        .context("failed to record runtime start")?;

    let state = AppState { kernel };
    let app = Router::new()
        .route("/health", get(health))
        .route("/v1/events", post(append_event).get(list_events))
        .route("/v1/stream", get(stream_events))
        .route("/v1/capabilities", get(capabilities))
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(args.bind)
        .await
        .with_context(|| format!("failed to bind {}", args.bind))?;
    info!(address = %args.bind, "Ditto daemon listening");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("Ditto daemon stopped unexpectedly")?;
    Ok(())
}

async fn health(State(state): State<AppState>) -> Result<Json<HealthResponse>, ApiError> {
    Ok(Json(HealthResponse {
        status: "ok".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        durable_events: state.kernel.event_count()?,
    }))
}

async fn append_event(
    State(state): State<AppState>,
    Json(event): Json<NewEvent>,
) -> Result<(StatusCode, Json<AppendEventResponse>), ApiError> {
    if event.kind.trim().is_empty() {
        return Err(ApiError::bad_request("event kind must not be empty"));
    }
    let event = state.kernel.append_event(event)?;
    Ok((StatusCode::CREATED, Json(AppendEventResponse { event })))
}

async fn list_events(
    State(state): State<AppState>,
    Query(query): Query<EventQuery>,
) -> Result<Json<Vec<EventRecord>>, ApiError> {
    Ok(Json(state.kernel.list_events(&query)?))
}

async fn capabilities(
    State(state): State<AppState>,
    Query(query): Query<CapabilitySearchQuery>,
) -> Json<Vec<ditto_capability::CapabilityCard>> {
    let limit = query.limit.unwrap_or(20).clamp(1, 100);
    let cards = match query.query.as_deref().map(str::trim) {
        Some(query) if !query.is_empty() => state.kernel.search_capabilities(query, limit),
        _ => state
            .kernel
            .capability_cards()
            .into_iter()
            .take(limit)
            .collect(),
    };
    Json(cards)
}

async fn stream_events(
    State(state): State<AppState>,
    Query(query): Query<EventQuery>,
) -> Result<Sse<impl Stream<Item = Result<SseEvent, Infallible>>>, ApiError> {
    let initial = state.kernel.list_events(&query)?;
    let kernel = state.kernel.clone();
    let filter = query.clone();
    let mut receiver = kernel.subscribe();

    let output = stream! {
        let mut last_seq = filter.after_seq.unwrap_or(0);

        for event in initial {
            if event.seq > last_seq {
                last_seq = event.seq;
                yield Ok(encode_sse(&event));
            }
        }

        loop {
            match receiver.recv().await {
                Ok(event) => {
                    if event.seq > last_seq && filter.matches(&event) {
                        last_seq = event.seq;
                        yield Ok(encode_sse(&event));
                    }
                }
                Err(RecvError::Lagged(skipped)) => {
                    warn!(skipped, last_seq, "event stream lagged; replaying from durable store");
                    let replay_query = EventQuery {
                        after_seq: Some(last_seq),
                        limit: Some(1_000),
                        session_id: filter.session_id.clone(),
                        task_id: filter.task_id.clone(),
                    };
                    match kernel.list_events(&replay_query) {
                        Ok(events) => {
                            for event in events {
                                if event.seq > last_seq {
                                    last_seq = event.seq;
                                    yield Ok(encode_sse(&event));
                                }
                            }
                        }
                        Err(error) => {
                            error!(%error, "failed to recover lagged event stream");
                            break;
                        }
                    }
                }
                Err(RecvError::Closed) => break,
            }
        }
    };

    Ok(Sse::new(output).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keep-alive"),
    ))
}

fn encode_sse(event: &EventRecord) -> SseEvent {
    let payload = serde_json::to_string(event).unwrap_or_else(|error| {
        serde_json::json!({
            "kind": "stream.serialization_failed",
            "error": error.to_string(),
        })
        .to_string()
    });

    SseEvent::default()
        .id(event.seq.to_string())
        .event(event.kind.clone())
        .data(payload)
}

#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: message.into(),
        }
    }
}

impl From<KernelError> for ApiError {
    fn from(error: KernelError) -> Self {
        error!(%error, "kernel operation failed");
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: "kernel operation failed".into(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        (
            self.status,
            Json(serde_json::json!({ "error": self.message })),
        )
            .into_response()
    }
}

async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(error) = tokio::signal::ctrl_c().await {
            error!(%error, "failed to install Ctrl+C handler");
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(error) => error!(%error, "failed to install SIGTERM handler"),
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }

    info!("shutdown signal received");
}
