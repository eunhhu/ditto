use std::{convert::Infallible, net::SocketAddr, path::PathBuf, time::Duration};

use anyhow::{Context, bail};
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
    CapabilitySearchQuery, EventQuery, EventRecord, HealthResponse, SubmitInputCommand,
    SubmitInputResponse,
};
use futures_core::Stream;
use futures_util::StreamExt;
use tokio::sync::broadcast::error::RecvError;
use tower_http::trace::TraceLayer;
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

const DEFAULT_REPLAY_PAGE_SIZE: usize = 500;

#[derive(Debug, Parser)]
#[command(
    name = "ditto-daemon",
    version,
    about = "Ditto semantic agent microkernel"
)]
struct Args {
    #[arg(long, env = "DITTO_DATA_DIR", default_value = ".ditto")]
    data_dir: PathBuf,
    #[arg(long, env = "DITTO_CAPABILITIES_DIR", default_value = "capabilities")]
    capabilities_dir: PathBuf,
    #[arg(long, env = "DITTO_BIND", default_value = "127.0.0.1:8787")]
    bind: SocketAddr,
    /// Explicit escape hatch until authenticated remote ingress exists.
    #[arg(long, env = "DITTO_ALLOW_UNAUTHENTICATED_REMOTE")]
    allow_unauthenticated_remote: bool,
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
    validate_bind(args.bind, args.allow_unauthenticated_remote)?;
    let kernel = DittoKernel::open(KernelConfig::new(args.data_dir, args.capabilities_dir))
        .context("failed to initialize Ditto kernel")?;
    kernel
        .record_runtime_started(&args.bind.to_string())
        .context("failed to record runtime start")?;

    let state = AppState { kernel };
    let app = Router::new()
        .route("/health", get(health))
        .route("/v1/commands/input", post(submit_input))
        .route("/v1/events", get(list_events))
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

fn validate_bind(bind: SocketAddr, allow_unauthenticated_remote: bool) -> anyhow::Result<()> {
    if !bind.ip().is_loopback() && !allow_unauthenticated_remote {
        bail!(
            "refusing unauthenticated non-loopback bind {bind}; use an authenticated gateway or pass --allow-unauthenticated-remote explicitly"
        );
    }
    Ok(())
}

async fn health(State(state): State<AppState>) -> Result<Json<HealthResponse>, ApiError> {
    Ok(Json(HealthResponse {
        status: "ok".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        durable_events: state.kernel.event_count()?,
        latest_seq: state.kernel.latest_event_seq()?,
    }))
}

async fn submit_input(
    State(state): State<AppState>,
    Json(command): Json<SubmitInputCommand>,
) -> Result<(StatusCode, Json<SubmitInputResponse>), ApiError> {
    let event = state.kernel.record_user_input(command)?;
    Ok((StatusCode::CREATED, Json(SubmitInputResponse { event })))
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
    // Subscribe before capturing the high-water mark. Events appended in between are
    // present in both the durable replay and the receiver and are deduplicated by seq.
    let kernel = state.kernel.clone();
    let mut receiver = kernel.subscribe();
    let initial_high_water = kernel.latest_event_seq()?;
    let page_size = query
        .limit
        .unwrap_or(DEFAULT_REPLAY_PAGE_SIZE)
        .clamp(1, 1_000);
    let filter = query;

    let output = stream! {
        let mut cursor = filter.after_seq.unwrap_or(0).max(0);

        let initial = replay_events(
            kernel.clone(),
            filter.clone(),
            cursor,
            initial_high_water,
            page_size,
        );
        futures_util::pin_mut!(initial);
        while let Some(result) = initial.next().await {
            match result {
                Ok(event) => yield Ok(encode_sse(&event)),
                Err(error) => {
                    error!(%error, cursor, initial_high_water, "initial event replay failed");
                    return;
                }
            }
        }
        cursor = initial_high_water;

        'live: loop {
            match receiver.recv().await {
                Ok(event) => {
                    if event.seq <= cursor {
                        continue;
                    }
                    if event.seq > cursor.saturating_add(1) {
                        let gap_high_water = event.seq - 1;
                        warn!(cursor, gap_high_water, "event stream observed a sequence gap; replaying durable events");
                        let gap = replay_events(
                            kernel.clone(),
                            filter.clone(),
                            cursor,
                            gap_high_water,
                            page_size,
                        );
                        futures_util::pin_mut!(gap);
                        while let Some(result) = gap.next().await {
                            match result {
                                Ok(replayed) => yield Ok(encode_sse(&replayed)),
                                Err(error) => {
                                    error!(%error, cursor, gap_high_water, "gap recovery failed");
                                    break 'live;
                                }
                            }
                        }
                        cursor = gap_high_water;
                    }

                    if event.seq > cursor {
                        if filter.matches_scope(&event) {
                            yield Ok(encode_sse(&event));
                        }
                        cursor = event.seq;
                    }
                }
                Err(RecvError::Lagged(skipped)) => {
                    let catch_up_high_water = match kernel.latest_event_seq() {
                        Ok(seq) => seq,
                        Err(error) => {
                            error!(%error, "failed to capture lag-recovery high-water mark");
                            break;
                        }
                    };
                    warn!(skipped, cursor, catch_up_high_water, "event stream lagged; replaying from durable storage");
                    let catch_up = replay_events(
                        kernel.clone(),
                        filter.clone(),
                        cursor,
                        catch_up_high_water,
                        page_size,
                    );
                    futures_util::pin_mut!(catch_up);
                    while let Some(result) = catch_up.next().await {
                        match result {
                            Ok(event) => yield Ok(encode_sse(&event)),
                            Err(error) => {
                                error!(%error, cursor, catch_up_high_water, "lag recovery failed");
                                break 'live;
                            }
                        }
                    }
                    cursor = catch_up_high_water;
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

fn replay_events(
    kernel: DittoKernel,
    filter: EventQuery,
    after_seq: i64,
    through_seq: i64,
    page_size: usize,
) -> impl Stream<Item = Result<EventRecord, KernelError>> {
    stream! {
        let mut cursor = after_seq.max(0);
        while cursor < through_seq {
            let page = kernel.list_events_through(
                &EventQuery {
                    after_seq: Some(cursor),
                    limit: Some(page_size),
                    session_id: filter.session_id.clone(),
                    task_id: filter.task_id.clone(),
                },
                through_seq,
            );
            match page {
                Ok(events) => {
                    if events.is_empty() {
                        break;
                    }
                    for event in events {
                        cursor = event.seq;
                        yield Ok(event);
                    }
                }
                Err(error) => {
                    yield Err(error);
                    break;
                }
            }
        }
    }
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

impl From<KernelError> for ApiError {
    fn from(error: KernelError) -> Self {
        let status = if matches!(&error, KernelError::InvalidCommand(_)) {
            StatusCode::BAD_REQUEST
        } else {
            error!(%error, "kernel operation failed");
            StatusCode::INTERNAL_SERVER_ERROR
        };
        Self {
            status,
            message: error.to_string(),
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

#[cfg(test)]
mod tests {
    use futures_util::StreamExt;
    use tempfile::tempdir;

    use super::{replay_events, validate_bind};
    use ditto_kernel::{DittoKernel, KernelConfig};
    use ditto_protocol::{EventQuery, SubmitInputCommand};

    #[test]
    fn refuses_remote_bind_without_an_explicit_escape_hatch() {
        let remote = "0.0.0.0:8787".parse().expect("socket address");
        assert!(validate_bind(remote, false).is_err());
        assert!(validate_bind(remote, true).is_ok());
    }

    #[tokio::test]
    async fn durable_replay_crosses_multiple_pages_without_gaps() {
        let directory = tempdir().expect("temporary directory");
        let kernel = DittoKernel::open(KernelConfig::new(
            directory.path().join("data"),
            directory.path().join("capabilities"),
        ))
        .expect("open kernel");
        for index in 0..2_005 {
            kernel
                .record_user_input(SubmitInputCommand {
                    text: format!("event-{index}"),
                    session_id: Some("session".into()),
                    task_id: None,
                })
                .expect("record fixture");
        }
        let high_water = kernel.latest_event_seq().expect("latest sequence");
        let replay = replay_events(
            kernel,
            EventQuery {
                session_id: Some("session".into()),
                ..EventQuery::default()
            },
            0,
            high_water,
            137,
        );
        futures_util::pin_mut!(replay);
        let mut events = Vec::new();
        while let Some(event) = replay.next().await {
            events.push(event.expect("replay event"));
        }

        assert_eq!(events.len(), 2_005);
        assert!(events.windows(2).all(|pair| pair[1].seq == pair[0].seq + 1));
    }
}
