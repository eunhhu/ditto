//! The learning ledger over HTTP (ADR 0050): every learned key of a session
//! and where it stands, and the user's undo.
use axum::{
    Json, Router,
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use ditto_kernel::KernelError;
use ditto_protocol::{LearnedKey, LearningLedger, LearningQuery, RetireLearnedCommand};

use super::{AppState, blocking};

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/learning", get(ledger))
        .route("/v1/commands/learning/retire", post(retire))
}

async fn ledger(
    State(state): State<AppState>,
    Query(query): Query<LearningQuery>,
) -> Result<Json<LearningLedger>, LearningApiError> {
    Ok(Json(
        blocking(&state.kernel, move |kernel| kernel.learning_ledger(query)).await?,
    ))
}

async fn retire(
    State(state): State<AppState>,
    Json(command): Json<RetireLearnedCommand>,
) -> Result<Json<LearnedKey>, LearningApiError> {
    Ok(Json(
        blocking(&state.kernel, move |kernel| {
            kernel.retire_learned_key(command)
        })
        .await?,
    ))
}

struct LearningApiError(KernelError);

impl From<KernelError> for LearningApiError {
    fn from(error: KernelError) -> Self {
        Self(error)
    }
}

impl IntoResponse for LearningApiError {
    fn into_response(self) -> axum::response::Response {
        let (status, message) = match self.0 {
            KernelError::InvalidCommand(message) => (StatusCode::BAD_REQUEST, message),
            _ => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "the learning ledger is unavailable".to_owned(),
            ),
        };
        (status, Json(serde_json::json!({ "error": message }))).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ditto_kernel::{DittoKernel, KernelConfig};
    use std::time::Duration;

    #[tokio::test]
    async fn the_ledger_lists_a_session_and_refuses_an_unknown_key() {
        let root = tempfile::tempdir().unwrap();
        let config = KernelConfig::new(root.path().join("data"), root.path().join("capabilities"));
        let kernel = DittoKernel::open(config).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let api = format!("http://{}", listener.local_addr().unwrap());
        let app = routes().with_state(AppState {
            kernel,
            driver: None,
            shutdown: ditto_model::CancellationToken::new(),
        });
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();

        let empty: LearningLedger = client
            .get(format!("{api}/v1/learning?session_id=personal"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(empty.session_id, "personal");
        assert!(empty.keys.is_empty());

        let unknown = client
            .post(format!("{api}/v1/commands/learning/retire"))
            .json(&serde_json::json!({ "session_id": "personal", "key_id": "no-such-key" }))
            .send()
            .await
            .unwrap();
        assert_eq!(unknown.status(), StatusCode::BAD_REQUEST);

        // A client cannot name an actor or kind: unknown fields are refused.
        let forged = client
            .post(format!("{api}/v1/commands/learning/retire"))
            .json(&serde_json::json!({
                "session_id": "personal", "key_id": "x", "actor": "system"
            }))
            .send()
            .await
            .unwrap();
        assert!(forged.status().is_client_error());

        let bad_session = client
            .get(format!("{api}/v1/learning?session_id="))
            .send()
            .await
            .unwrap();
        assert_eq!(bad_session.status(), StatusCode::BAD_REQUEST);
        server.abort();
    }
}
