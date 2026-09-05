use axum::{
    Json, Router,
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use ditto_kernel::KernelError;
use ditto_protocol::{
    MemoryPage, MemoryQuery, MemoryWriteOutcome, RememberInputCommand, RememberInputResponse,
};

use super::AppState;

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/commands/memory", post(remember))
        .route("/v1/memories", get(list))
}

async fn remember(
    State(state): State<AppState>,
    Json(command): Json<RememberInputCommand>,
) -> Result<(StatusCode, Json<RememberInputResponse>), MemoryApiError> {
    let response = state.kernel.remember_input(command)?;
    let status = match response.outcome {
        MemoryWriteOutcome::Recorded => StatusCode::CREATED,
        MemoryWriteOutcome::AlreadyRecorded => StatusCode::OK,
        MemoryWriteOutcome::CommittedButProjectionUnavailable => StatusCode::ACCEPTED,
    };
    Ok((status, Json(response)))
}

async fn list(
    State(state): State<AppState>,
    Query(query): Query<MemoryQuery>,
) -> Result<Json<MemoryPage>, MemoryApiError> {
    Ok(Json(state.kernel.list_memories(query)?))
}

struct MemoryApiError(KernelError);

impl From<KernelError> for MemoryApiError {
    fn from(error: KernelError) -> Self {
        Self(error)
    }
}

impl IntoResponse for MemoryApiError {
    fn into_response(self) -> axum::response::Response {
        let (status, message) = match self.0 {
            KernelError::InvalidCommand(message) => (StatusCode::BAD_REQUEST, message),
            KernelError::MemoryConflict(message) => (StatusCode::CONFLICT, message.to_owned()),
            _ => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "memory storage or source verification is unavailable".to_owned(),
            ),
        };
        (status, Json(serde_json::json!({"error": message}))).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ditto_kernel::{DittoKernel, KernelConfig};
    use ditto_protocol::{MemoryPage, RememberInputResponse, SubmitInputCommand};
    use serde_json::json;
    use std::time::Duration;

    #[tokio::test]
    async fn http_memory_contract_reports_authority_errors_retries_conflicts_and_accepted_writes() {
        let root = tempfile::tempdir().unwrap();
        let config = KernelConfig::new(root.path().join("data"), root.path().join("capabilities"));
        let kernel = DittoKernel::open(config.clone()).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let api = format!("http://{}", listener.local_addr().unwrap());
        let app = routes().with_state(AppState {
            kernel: kernel.clone(),
            driver: None,
            shutdown: ditto_model::CancellationToken::new(),
        });
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();
        let input = kernel
            .record_user_input(SubmitInputCommand {
                text: "HTTP memory".into(),
                session_id: Some("personal".into()),
                task_id: None,
            })
            .unwrap();
        let command = json!({"session_id":"personal", "input_event_id":input.event_id});
        let mut forged = command.clone();
        forged["actor"] = json!("system");
        let before = kernel.event_count().unwrap();
        assert_eq!(
            client
                .post(format!("{api}/v1/commands/memory"))
                .json(&forged)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::UNPROCESSABLE_ENTITY
        );
        assert_eq!(kernel.event_count().unwrap(), before);
        let recorded = client
            .post(format!("{api}/v1/commands/memory"))
            .json(&command)
            .send()
            .await
            .unwrap();
        assert_eq!(recorded.status(), StatusCode::CREATED);
        let recorded: RememberInputResponse = recorded.json().await.unwrap();
        let retry = client
            .post(format!("{api}/v1/commands/memory"))
            .json(&command)
            .send()
            .await
            .unwrap();
        assert_eq!(retry.status(), StatusCode::OK);
        assert_eq!(
            retry
                .json::<RememberInputResponse>()
                .await
                .unwrap()
                .event_id,
            recorded.event_id
        );
        let mut conflict = command;
        conflict["replaces"] = json!(recorded.memory_id);
        assert_eq!(
            client
                .post(format!("{api}/v1/commands/memory"))
                .json(&conflict)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::CONFLICT
        );
        assert_eq!(
            client
                .get(format!("{api}/v1/memories?session_id=personal&limit=0"))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            client
                .get(format!(
                    "{api}/v1/memories?session_id=personal&actor=system"
                ))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::BAD_REQUEST
        );
        let page: MemoryPage = client
            .get(format!("{api}/v1/memories?session_id=personal"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(page.memories[0].text, "HTTP memory");

        let source = kernel
            .record_user_input(SubmitInputCommand {
                text: "accepted with pending projection".into(),
                session_id: Some("personal".into()),
                task_id: None,
            })
            .unwrap();
        let connection =
            rusqlite::Connection::open(config.data_dir.join("context-projection.db")).unwrap();
        connection.execute_batch("CREATE TRIGGER memory_api_failure BEFORE INSERT ON projected_nodes BEGIN SELECT RAISE(ABORT, 'private-storage-detail'); END;").unwrap();
        let command = json!({"session_id":"personal", "input_event_id":source.event_id});
        let pending = client
            .post(format!("{api}/v1/commands/memory"))
            .json(&command)
            .send()
            .await
            .unwrap();
        assert_eq!(pending.status(), StatusCode::ACCEPTED);
        let body = pending.text().await.unwrap();
        assert!(!body.contains("private-storage-detail"));
        let pending: RememberInputResponse = serde_json::from_str(&body).unwrap();
        assert_eq!(
            pending.outcome,
            MemoryWriteOutcome::CommittedButProjectionUnavailable
        );
        let failed_read = client
            .get(format!("{api}/v1/memories?session_id=personal"))
            .send()
            .await
            .unwrap();
        assert_eq!(failed_read.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert!(
            !failed_read
                .text()
                .await
                .unwrap()
                .contains("private-storage-detail")
        );
        connection
            .execute_batch("DROP TRIGGER memory_api_failure;")
            .unwrap();
        drop(connection);
        let recovered = client
            .post(format!("{api}/v1/commands/memory"))
            .json(&command)
            .send()
            .await
            .unwrap();
        assert_eq!(recovered.status(), StatusCode::OK);
        assert_eq!(
            recovered
                .json::<RememberInputResponse>()
                .await
                .unwrap()
                .event_id,
            pending.event_id
        );
        server.abort();
        let _ = server.await;
    }
}
