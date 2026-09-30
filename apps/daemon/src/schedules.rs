use axum::{
    Json, Router,
    extract::{Query, State},
    routing::{get, post},
};
use ditto_protocol::{AgentRunQuery, ScheduleListQuery, ScheduleResponse, ScheduleRunCommand};

use super::{AppState, blocking, runs::RunApiError};

pub(super) fn routes(loopback: bool) -> Router<AppState> {
    if !loopback {
        return Router::new();
    }
    Router::new()
        .route("/v1/commands/schedule", post(create))
        .route("/v1/schedules", get(inspect))
        .route("/v1/schedules/pending", get(pending))
        .route("/v1/commands/schedule/cancel", post(cancel))
        .route("/v1/commands/repeat", post(create_repeat))
        .route("/v1/repeats", get(inspect_repeat))
        .route("/v1/repeats/active", get(active_repeats))
        .route("/v1/commands/repeat/cancel", post(cancel_repeat))
}

async fn create(
    State(state): State<AppState>,
    Json(command): Json<ScheduleRunCommand>,
) -> Result<Json<ScheduleResponse>, RunApiError> {
    Ok(Json(
        blocking(&state.kernel, move |kernel| kernel.schedule_run(command)).await?,
    ))
}
async fn inspect(
    State(state): State<AppState>,
    Query(query): Query<AgentRunQuery>,
) -> Result<Json<ScheduleResponse>, RunApiError> {
    Ok(Json(
        blocking(&state.kernel, move |kernel| kernel.inspect_schedule(query)).await?,
    ))
}
async fn pending(
    State(state): State<AppState>,
    Query(query): Query<ScheduleListQuery>,
) -> Result<Json<Vec<ScheduleResponse>>, RunApiError> {
    Ok(Json(
        blocking(&state.kernel, move |kernel| {
            kernel.list_pending_schedules(&query.session_id)
        })
        .await?,
    ))
}
async fn cancel(
    State(state): State<AppState>,
    Json(query): Json<AgentRunQuery>,
) -> Result<Json<ScheduleResponse>, RunApiError> {
    Ok(Json(
        blocking(&state.kernel, move |kernel| kernel.cancel_schedule(query)).await?,
    ))
}

async fn create_repeat(
    State(state): State<AppState>,
    Json(command): Json<ditto_protocol::RepeatScheduleCommand>,
) -> Result<Json<ditto_protocol::RepeatScheduleResponse>, RunApiError> {
    Ok(Json(
        blocking(&state.kernel, move |kernel| kernel.repeat_schedule(command)).await?,
    ))
}
async fn inspect_repeat(
    State(state): State<AppState>,
    Query(query): Query<AgentRunQuery>,
) -> Result<Json<ditto_protocol::RepeatScheduleResponse>, RunApiError> {
    Ok(Json(
        blocking(&state.kernel, move |kernel| kernel.inspect_repeat(query)).await?,
    ))
}
async fn active_repeats(
    State(state): State<AppState>,
    Query(query): Query<ScheduleListQuery>,
) -> Result<Json<Vec<ditto_protocol::RepeatScheduleResponse>>, RunApiError> {
    Ok(Json(
        blocking(&state.kernel, move |kernel| {
            kernel.list_active_repeats(&query.session_id)
        })
        .await?,
    ))
}
async fn cancel_repeat(
    State(state): State<AppState>,
    Json(query): Json<AgentRunQuery>,
) -> Result<Json<ditto_protocol::RepeatScheduleResponse>, RunApiError> {
    Ok(Json(
        blocking(&state.kernel, move |kernel| kernel.cancel_repeat(query)).await?,
    ))
}

#[cfg(test)]
mod tests;
