use axum::{
    Json, Router,
    extract::{Query, State},
    routing::{get, post},
};
use ditto_protocol::{AgentRunQuery, ScheduleListQuery, ScheduleResponse, ScheduleRunCommand};

use super::{AppState, runs::RunApiError};

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
    Ok(Json(state.kernel.schedule_run(command)?))
}
async fn inspect(
    State(state): State<AppState>,
    Query(query): Query<AgentRunQuery>,
) -> Result<Json<ScheduleResponse>, RunApiError> {
    Ok(Json(state.kernel.inspect_schedule(query)?))
}
async fn pending(
    State(state): State<AppState>,
    Query(query): Query<ScheduleListQuery>,
) -> Result<Json<Vec<ScheduleResponse>>, RunApiError> {
    Ok(Json(
        state.kernel.list_pending_schedules(&query.session_id)?,
    ))
}
async fn cancel(
    State(state): State<AppState>,
    Json(query): Json<AgentRunQuery>,
) -> Result<Json<ScheduleResponse>, RunApiError> {
    Ok(Json(state.kernel.cancel_schedule(query)?))
}

async fn create_repeat(
    State(state): State<AppState>,
    Json(command): Json<ditto_protocol::RepeatScheduleCommand>,
) -> Result<Json<ditto_protocol::RepeatScheduleResponse>, RunApiError> {
    Ok(Json(state.kernel.repeat_schedule(command)?))
}
async fn inspect_repeat(
    State(state): State<AppState>,
    Query(query): Query<AgentRunQuery>,
) -> Result<Json<ditto_protocol::RepeatScheduleResponse>, RunApiError> {
    Ok(Json(state.kernel.inspect_repeat(query)?))
}
async fn active_repeats(
    State(state): State<AppState>,
    Query(query): Query<ScheduleListQuery>,
) -> Result<Json<Vec<ditto_protocol::RepeatScheduleResponse>>, RunApiError> {
    Ok(Json(state.kernel.list_active_repeats(&query.session_id)?))
}
async fn cancel_repeat(
    State(state): State<AppState>,
    Json(query): Json<AgentRunQuery>,
) -> Result<Json<ditto_protocol::RepeatScheduleResponse>, RunApiError> {
    Ok(Json(state.kernel.cancel_repeat(query)?))
}

#[cfg(test)]
mod tests;
