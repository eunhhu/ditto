//! The profile's standing instructions over HTTP (ADR 0045): read what is in
//! force, replace it with one call.
use axum::{Json, Router, extract::State, http::StatusCode, response::IntoResponse, routing::get};
use ditto_kernel::{KernelError, instructions::InstructionDocument};
use serde::Deserialize;

use super::{AppState, blocking};

pub(super) fn routes() -> Router<AppState> {
    Router::new().route("/v1/instructions", get(show).put(replace))
}

#[derive(Deserialize)]
struct ReplaceInstructions {
    text: String,
}

async fn replace(
    State(state): State<AppState>,
    Json(body): Json<ReplaceInstructions>,
) -> Result<(StatusCode, Json<InstructionDocument>), InstructionsApiError> {
    let document = blocking(&state.kernel, move |kernel| {
        kernel.set_instructions(&body.text)
    })
    .await?;
    Ok((StatusCode::CREATED, Json(document)))
}

async fn show(
    State(state): State<AppState>,
) -> Result<Json<Option<InstructionDocument>>, InstructionsApiError> {
    let document = blocking(&state.kernel, |kernel| {
        let seq = kernel.latest_event_seq()?;
        kernel.instructions_in_force(seq)
    })
    .await?;
    Ok(Json(document))
}

struct InstructionsApiError(KernelError);

impl From<KernelError> for InstructionsApiError {
    fn from(error: KernelError) -> Self {
        Self(error)
    }
}

impl IntoResponse for InstructionsApiError {
    fn into_response(self) -> axum::response::Response {
        let (status, message) = match self.0 {
            KernelError::InvalidCommand(message) => (StatusCode::BAD_REQUEST, message),
            _ => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "instruction storage is unavailable".to_owned(),
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
    async fn http_instructions_replace_and_read_back_what_is_in_force() {
        let root = tempfile::tempdir().unwrap();
        let config = KernelConfig::new(root.path().join("data"), root.path().join("capabilities"));
        let kernel = DittoKernel::open(config).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let api = format!("http://{}", listener.local_addr().unwrap());
        let app = routes().with_state(AppState {
            kernel: kernel.clone(),
            driver: None,
            shutdown: ditto_model::CancellationToken::new(),
        });
        let _server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();

        let empty: Option<InstructionDocument> = client
            .get(format!("{api}/v1/instructions"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert!(
            empty.is_none(),
            "a fresh profile has no standing instructions"
        );

        let created = client
            .put(format!("{api}/v1/instructions"))
            .json(&serde_json::json!({ "text": "Answer in Korean." }))
            .send()
            .await
            .unwrap();
        assert_eq!(created.status(), StatusCode::CREATED);
        let document: InstructionDocument = created.json().await.unwrap();
        assert_eq!(document.text, "Answer in Korean.");

        let current: Option<InstructionDocument> = client
            .get(format!("{api}/v1/instructions"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(current.expect("in force").event_id, document.event_id);

        let refused = client
            .put(format!("{api}/v1/instructions"))
            .json(&serde_json::json!({ "text": "x".repeat(2_001) }))
            .send()
            .await
            .unwrap();
        assert_eq!(refused.status(), StatusCode::BAD_REQUEST);
    }
}
