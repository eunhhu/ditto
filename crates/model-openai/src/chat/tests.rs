use std::collections::BTreeMap;

use ditto_capability::CapabilitySchema;
use ditto_context::ContextCapsule;
use ditto_model::{
    ContentPart, ConversationItem, ExecutionEpochId, FailureKind, FinishReason, MessageRole,
    ModelEvent, ModelFeature, ModelRequest, ModelRequestId, ModelTurn, OutputConstraint,
    ParallelToolCalls, ProviderCallId, StableSystemPrefix, ToolChoice,
};
use serde_json::{Value, json};

use super::{ChatCompletionsEndpoint, compile::compile_chat_request, mapper::ChatStreamMapper};
use crate::{driver::EventMapper, sse::SseEvent};

fn request(conversation: Vec<ConversationItem>) -> ModelRequest {
    let mut request = ModelRequest::new(
        ModelRequestId::new("request-7").unwrap(),
        ExecutionEpochId::new("epoch-1").unwrap(),
        StableSystemPrefix {
            segments: vec!["You are Ditto.".into()],
        },
        ModelTurn {
            conversation,
            context: ContextCapsule::default(),
            output: OutputConstraint::Text,
        },
    );
    request.tools.push(CapabilitySchema {
        id: "artifact.read".into(),
        version: "0.1.0".into(),
        summary: "Read an artifact".into(),
        input_schema: json!({"type": "object", "properties": {"reference": {"type": "string"}}}),
        output_schema: json!({"type": "object"}),
    });
    request.generation.tool_use.choice = ToolChoice::Auto;
    request.generation.tool_use.parallel_calls = ParallelToolCalls::Forbid;
    request.features.required = [ModelFeature::Text, ModelFeature::ToolCalls].into();
    request
}

fn user(text: &str) -> ConversationItem {
    ConversationItem::Message {
        role: MessageRole::User,
        content: vec![ContentPart::Text { text: text.into() }],
    }
}

fn body(request: &ModelRequest, usage: bool) -> Value {
    let compiled = compile_chat_request(request, "local-model", usage).unwrap();
    serde_json::from_slice(compiled.http.body()).unwrap()
}

#[test]
fn compile_projects_messages_tools_and_controls_deterministically() {
    let call = ProviderCallId::new("call_1").unwrap();
    let request = request(vec![
        user("read it"),
        ConversationItem::Message {
            role: MessageRole::Assistant,
            content: vec![ContentPart::Text {
                text: "Reading.".into(),
            }],
        },
        ConversationItem::ToolCall {
            call_id: call.clone(),
            capability_id: "artifact.read".into(),
            arguments: json!({"reference": "artifact:sha256:00"}),
        },
        ConversationItem::ToolResult {
            call_id: call,
            content: vec![ContentPart::Structured {
                value: json!({"ok": true}),
            }],
            is_error: false,
        },
    ]);
    let body = body(&request, false);
    assert_eq!(body["model"], "local-model");
    assert_eq!(body["stream"], true);
    assert!(body.get("stream_options").is_none());
    assert_eq!(
        body["messages"][0],
        json!({"role": "system", "content": "You are Ditto."})
    );
    assert_eq!(
        body["messages"][1],
        json!({"role": "user", "content": "read it"})
    );
    // Text streamed before the call shares the assistant message.
    assert_eq!(body["messages"][2]["content"], "Reading.");
    assert_eq!(
        body["messages"][2]["tool_calls"][0]["function"],
        json!({"name": "artifact_read", "arguments": "{\"reference\":\"artifact:sha256:00\"}"})
    );
    assert_eq!(body["messages"][3]["role"], "tool");
    assert_eq!(body["messages"][3]["tool_call_id"], "call_1");
    assert!(
        body["messages"][3]["content"]
            .as_str()
            .unwrap()
            .starts_with("DITTO_CONTENT_V1\n")
    );
    assert_eq!(body["tools"][0]["function"]["name"], "artifact_read");
    assert_eq!(body["tool_choice"], "auto");
    assert_eq!(body["parallel_tool_calls"], false);
    assert_eq!(
        compile_chat_request(&request, "local-model", false)
            .unwrap()
            .http
            .body(),
        compile_chat_request(&request, "local-model", false)
            .unwrap()
            .http
            .body()
    );
    assert_eq!(
        self::body(&request, true)["stream_options"],
        json!({"include_usage": true})
    );
}

#[test]
fn compile_keeps_one_system_message_with_context_and_rejects_unsupported_shapes() {
    let mut with_context = request(vec![user("hi")]);
    with_context.turn.context = serde_json::from_value(json!({"nodes": [{
        "id": "memory-1", "kind": "claim", "summary": "I prefer tea", "origin": "user",
        "epistemic": "asserted", "scope": "session", "confidence": 1.0,
        "source_event_ids": ["01K00000000000000000000000"]
    }]}))
    .unwrap();
    let body = body(&with_context, false);
    let system = body["messages"][0]["content"].as_str().unwrap();
    assert!(system.starts_with("You are Ditto.\n\nDITTO_CONTEXT_V1\n"));
    assert!(system.contains("I prefer tea"));
    assert_eq!(body["messages"].as_array().unwrap().len(), 2);

    let mut structured = request(vec![user("hi")]);
    structured.turn.output = OutputConstraint::Structured {
        name: "out".into(),
        schema: json!({"type": "object"}),
        strict: true,
    };
    assert!(compile_chat_request(&structured, "m", false).is_err());
}

#[test]
fn readable_names_map_back_and_overlong_ids_fall_back_to_digest_names() {
    // Valid capability IDs contain no `_`, so `.` to `_` is injective; only IDs
    // beyond the 64-byte function-name limit need a digest.
    let mut request = request(vec![user("hi")]);
    let mut long = request.tools[0].clone();
    long.id = format!("namespace.{}", "x".repeat(70));
    request.tools.push(long.clone());
    let names = compile_chat_request(&request, "m", false)
        .unwrap()
        .reverse_names;
    assert_eq!(names.len(), 2);
    assert_eq!(names["artifact_read"], "artifact.read");
    let (digest, id) = names
        .iter()
        .find(|(name, _)| name.starts_with("d_"))
        .unwrap();
    assert_eq!(id, &long.id);
    assert_eq!(digest.len(), 34);
}

fn feed(mapper: &mut ChatStreamMapper, chunks: &[Value]) -> Vec<ModelEvent> {
    let mut events = Vec::new();
    for chunk in chunks {
        events.extend(
            mapper
                .map(SseEvent {
                    event: None,
                    data: chunk.to_string(),
                })
                .unwrap(),
        );
    }
    events
}

fn done(mapper: &mut ChatStreamMapper) -> Vec<ModelEvent> {
    mapper
        .map(SseEvent {
            event: None,
            data: "[DONE]".into(),
        })
        .unwrap()
}

fn mapper() -> ChatStreamMapper {
    ChatStreamMapper::new(
        BTreeMap::from([("artifact_read".to_owned(), "artifact.read".to_owned())]),
        "request-7",
    )
}

#[test]
fn text_and_usage_stream_then_complete_on_done() {
    let mut mapper = mapper();
    let events = feed(
        &mut mapper,
        &[
            json!({"choices": [{"index": 0, "delta": {"role": "assistant", "content": ""}}]}),
            json!({"choices": [{"index": 0, "delta": {"content": "Hel"}}]}),
            json!({"choices": [{"index": 0, "delta": {"reasoning_content": "hidden"}}]}),
            json!({"choices": [{"index": 0, "delta": {"content": "lo"}, "finish_reason": "stop"}]}),
            json!({"choices": [], "usage": {"prompt_tokens": 12, "completion_tokens": 3, "total_tokens": 15}}),
        ],
    );
    assert!(matches!(&events[..], [
        ModelEvent::TextDelta { text: a },
        ModelEvent::TextDelta { text: b },
        ModelEvent::UsageUpdate { .. },
    ] if a == "Hel" && b == "lo"));
    assert!(matches!(
        &done(&mut mapper)[..],
        [ModelEvent::Completed {
            finish_reason: FinishReason::EndTurn,
            continuation: None
        }]
    ));
}

#[test]
fn split_tool_call_arguments_become_one_ready_call() {
    let mut mapper = mapper();
    let events = feed(
        &mut mapper,
        &[
            json!({"choices": [{"index": 0, "delta": {"tool_calls": [{"index": 0, "id": "call_a", "type": "function", "function": {"name": "artifact_read", "arguments": ""}}]}}]}),
            json!({"choices": [{"index": 0, "delta": {"tool_calls": [{"index": 0, "function": {"arguments": "{\"reference\":"}}]}}]}),
            json!({"choices": [{"index": 0, "delta": {"tool_calls": [{"index": 0, "function": {"arguments": "\"x\"}"}}]}}]}),
            json!({"choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}]}),
        ],
    );
    let call = ProviderCallId::new("call_a").unwrap();
    assert_eq!(
        events,
        [
            ModelEvent::ToolCallStarted {
                call_id: call.clone(),
                capability_id: "artifact.read".into()
            },
            ModelEvent::ToolCallArgumentDelta {
                call_id: call.clone(),
                delta: "{\"reference\":".into()
            },
            ModelEvent::ToolCallArgumentDelta {
                call_id: call.clone(),
                delta: "\"x\"}".into()
            },
            ModelEvent::ToolCallReady {
                call_id: call,
                capability_id: "artifact.read".into(),
                arguments: json!({"reference": "x"})
            },
        ]
    );
    assert!(matches!(
        &done(&mut mapper)[..],
        [ModelEvent::Completed {
            finish_reason: FinishReason::ToolCalls,
            ..
        }]
    ));
}

#[test]
fn whole_calls_without_ids_or_with_stop_still_map_to_tool_calls() {
    let mut mapper = mapper();
    let events = feed(
        &mut mapper,
        &[
            json!({"choices": [{"index": 0, "delta": {"tool_calls": [{"function": {"name": "artifact_read", "arguments": {"reference": "y"}}}]}, "finish_reason": "stop"}]}),
        ],
    );
    let call = ProviderCallId::new("request-7-call-0").unwrap();
    assert!(matches!(&events[0], ModelEvent::ToolCallStarted { call_id, .. } if *call_id == call));
    assert!(
        matches!(events.last(), Some(ModelEvent::ToolCallReady { arguments, .. }) if *arguments == json!({"reference": "y"}))
    );
    // End of stream without [DONE] still completes after a finish reason.
    assert!(matches!(
        &mapper.finish()[..],
        [ModelEvent::Completed {
            finish_reason: FinishReason::ToolCalls,
            ..
        }]
    ));
}

#[test]
fn fragments_without_index_join_the_matching_or_latest_call() {
    let mut mapper = mapper();
    let events = feed(
        &mut mapper,
        &[
            json!({"choices": [{"index": 0, "delta": {"tool_calls": [{"id": "call_b", "type": "function", "function": {"name": "artifact_read", "arguments": "{\"reference\":"}}]}}]}),
            // The ID repeats without an index.
            json!({"choices": [{"index": 0, "delta": {"tool_calls": [{"id": "call_b", "function": {"arguments": "\"z\""}}]}}]}),
            // Neither index nor ID: the latest call continues.
            json!({"choices": [{"index": 0, "delta": {"tool_calls": [{"function": {"arguments": "}"}}]}}]}),
            json!({"choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}]}),
        ],
    );
    let started = events
        .iter()
        .filter(|event| matches!(event, ModelEvent::ToolCallStarted { .. }))
        .count();
    assert_eq!(started, 1);
    assert!(matches!(
        events.last(),
        Some(ModelEvent::ToolCallReady { call_id, arguments, .. })
            if call_id.as_str() == "call_b" && *arguments == json!({"reference": "z"})
    ));
}

#[test]
fn errors_malformed_arguments_and_missing_finish_fail_closed() {
    let mut mapper = mapper();
    let failure = mapper
        .map(SseEvent {
            event: None,
            data: json!({"error": {"message": "rate limited sk-proj-DITTO_CHAT_SENTINEL_31", "code": 429}}).to_string(),
        })
        .unwrap_err();
    assert_eq!(failure.kind, FailureKind::Provider);
    assert_eq!(failure.provider_code.as_deref(), Some("429"));
    assert!(!failure.message.contains("DITTO_CHAT_SENTINEL"));

    let mut mapper = self::mapper();
    assert_eq!(
        mapper
            .map(SseEvent {
                event: None,
                data: "{not json".into()
            })
            .unwrap_err()
            .kind,
        FailureKind::Protocol
    );

    let mut mapper = self::mapper();
    feed(
        &mut mapper,
        &[
            json!({"choices": [{"index": 0, "delta": {"tool_calls": [{"index": 0, "id": "c", "function": {"name": "artifact_read", "arguments": "{\"broken\""}}]}}]}),
        ],
    );
    let failure = mapper
        .map(SseEvent {
            event: None,
            data: json!({"choices": [{"index": 0, "finish_reason": "tool_calls"}]}).to_string(),
        })
        .unwrap_err();
    assert_eq!(failure.kind, FailureKind::MalformedToolArguments);

    let mut mapper = self::mapper();
    feed(
        &mut mapper,
        &[json!({"choices": [{"index": 0, "delta": {"content": "partial"}}]})],
    );
    assert!(matches!(
        &mapper.finish()[..],
        [ModelEvent::Failed { failure }] if failure.kind == FailureKind::Protocol
    ));
    // Other choices are ignored rather than merged.
    let mut mapper = self::mapper();
    assert!(
        feed(
            &mut mapper,
            &[json!({"choices": [{"index": 1, "delta": {"content": "other"}}]})]
        )
        .is_empty()
    );
}

#[test]
fn endpoints_require_https_except_loopback() {
    for accepted in [
        "https://api.x.ai/v1",
        "https://openrouter.ai/api/v1/",
        "http://127.0.0.1:11434/v1",
        "http://localhost:8080",
        "http://[::1]:9000/v1",
    ] {
        let endpoint = ChatCompletionsEndpoint::new(accepted).unwrap();
        assert!(
            endpoint.url().path().ends_with("/chat/completions"),
            "{accepted}"
        );
    }
    assert_eq!(
        ChatCompletionsEndpoint::new("https://openrouter.ai/api/v1/")
            .unwrap()
            .url()
            .as_str(),
        "https://openrouter.ai/api/v1/chat/completions"
    );
    for rejected in [
        "http://example.com/v1",
        "http://192.168.1.10:11434/v1",
        "ftp://example.com",
        "https://user:secret@example.com/v1",
        "https://example.com/v1?key=secret",
        "not a url",
    ] {
        assert!(
            ChatCompletionsEndpoint::new(rejected).is_err(),
            "{rejected}"
        );
    }
}
