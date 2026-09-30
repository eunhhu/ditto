//! Real HTTP round trips through the chat completions reqwest transport.
use std::{
    collections::BTreeSet,
    sync::{Arc, Mutex},
};

use ditto_context::ContextCapsule;
use ditto_model::{
    CancellationToken, ContentPart, ConversationItem, ExecutionEpochId, FinishReason, MessageRole,
    ModelDriver, ModelEvent, ModelFeature, ModelRequest, ModelRequestId, ModelTurn,
    OutputConstraint, StableSystemPrefix,
};
use ditto_model_openai::{
    ChatCompletionsConfig, ChatCompletionsDriver, ChatCompletionsEndpoint, OpenAiApiKey,
};
use futures_util::StreamExt;
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

type Captured = Arc<Mutex<Vec<(String, Value)>>>;

/// Answer each connection once with a canned SSE body; keep request headers
/// and JSON bodies for assertions.
async fn server(bodies: Vec<String>) -> (String, Captured) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    let captured: Captured = Arc::default();
    let log = Arc::clone(&captured);
    tokio::spawn(async move {
        for body in bodies {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buffer = Vec::new();
            let mut chunk = [0_u8; 8192];
            let (head, content_length) = loop {
                let read = socket.read(&mut chunk).await.unwrap();
                buffer.extend_from_slice(&chunk[..read]);
                if let Some(end) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&buffer[..end]).to_string();
                    let length = head
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().ok())?
                        })
                        .unwrap();
                    buffer.drain(..end + 4);
                    break (head, length);
                }
            };
            while buffer.len() < content_length {
                let read = socket.read(&mut chunk).await.unwrap();
                buffer.extend_from_slice(&chunk[..read]);
            }
            log.lock()
                .unwrap()
                .push((head, serde_json::from_slice(&buffer).unwrap()));
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n{body}"
            );
            socket.write_all(response.as_bytes()).await.unwrap();
            socket.shutdown().await.ok();
        }
    });
    (base, captured)
}

fn sse(chunks: &[Value]) -> String {
    let mut body = chunks
        .iter()
        .map(|chunk| format!("data: {chunk}\n\n"))
        .collect::<String>();
    body.push_str(": keep-alive comment\n\ndata: [DONE]\n\n");
    body
}

fn request() -> ModelRequest {
    let mut request = ModelRequest::new(
        ModelRequestId::new("request-1").unwrap(),
        ExecutionEpochId::new("epoch-1").unwrap(),
        StableSystemPrefix {
            segments: vec!["Be brief.".into()],
        },
        ModelTurn {
            conversation: vec![ConversationItem::Message {
                role: MessageRole::User,
                content: vec![ContentPart::Text {
                    text: "hello".into(),
                }],
            }],
            context: ContextCapsule::default(),
            output: OutputConstraint::Text,
        },
    );
    request.features.required = BTreeSet::from([ModelFeature::Text]);
    request
}

async fn collect(driver: &ChatCompletionsDriver) -> Vec<ModelEvent> {
    driver
        .stream(request(), CancellationToken::new())
        .map(|event| event.event)
        .collect()
        .await
}

#[tokio::test]
async fn streams_text_with_bearer_key_over_real_http() {
    let (base, captured) = server(vec![sse(&[
        json!({"choices": [{"index": 0, "delta": {"role": "assistant", "content": "Hi"}}]}),
        json!({"choices": [{"index": 0, "delta": {"content": " there"}, "finish_reason": "stop"}]}),
        json!({"choices": [], "usage": {"prompt_tokens": 9, "completion_tokens": 2, "total_tokens": 11}}),
    ])])
    .await;
    let config =
        ChatCompletionsConfig::new(ChatCompletionsEndpoint::new(&base).unwrap(), "grok-test")
            .unwrap()
            .with_usage();
    let driver =
        ChatCompletionsDriver::new(config, Some(OpenAiApiKey::new("test-key-123").unwrap()))
            .unwrap();
    let events = collect(&driver).await;
    assert!(matches!(&events[..], [
        ModelEvent::TextDelta { text: a },
        ModelEvent::TextDelta { text: b },
        ModelEvent::UsageUpdate { .. },
        ModelEvent::Completed { finish_reason: FinishReason::EndTurn, .. },
    ] if a == "Hi" && b == " there"));
    let captured = captured.lock().unwrap();
    let (head, body) = &captured[0];
    assert!(head.starts_with("POST /v1/chat/completions HTTP/1.1"));
    assert!(
        head.to_ascii_lowercase()
            .contains("authorization: bearer test-key-123")
    );
    assert_eq!(body["model"], "grok-test");
    assert_eq!(body["stream_options"], json!({"include_usage": true}));
    assert_eq!(
        body["messages"][0],
        json!({"role": "system", "content": "Be brief."})
    );
    assert_eq!(
        body["messages"][1],
        json!({"role": "user", "content": "hello"})
    );
}

#[tokio::test]
async fn local_servers_need_no_key_and_http_errors_are_bounded_failures() {
    let (base, captured) = server(vec![sse(&[json!({
        "choices": [{"index": 0, "delta": {"content": "local"}, "finish_reason": "stop"}]
    })])])
    .await;
    let config =
        ChatCompletionsConfig::new(ChatCompletionsEndpoint::new(&base).unwrap(), "qwen2.5:7b")
            .unwrap();
    let driver = ChatCompletionsDriver::new(config, None).unwrap();
    let events = collect(&driver).await;
    assert!(matches!(events.last(), Some(ModelEvent::Completed { .. })));
    let head = captured.lock().unwrap()[0].0.to_ascii_lowercase();
    assert!(!head.contains("authorization"));
    assert!(
        captured.lock().unwrap()[0]
            .1
            .get("stream_options")
            .is_none()
    );

    // A closed port becomes one bounded transport failure, never a hang.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    drop(listener);
    let config =
        ChatCompletionsConfig::new(ChatCompletionsEndpoint::new(&base).unwrap(), "m").unwrap();
    let driver = ChatCompletionsDriver::new(config, None).unwrap();
    let events = collect(&driver).await;
    assert!(matches!(&events[..], [ModelEvent::Failed { .. }]));
}
