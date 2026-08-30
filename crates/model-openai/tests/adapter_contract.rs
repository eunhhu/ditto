use std::{
    collections::{BTreeSet, VecDeque},
    future::pending,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use chrono::{Duration as ChronoDuration, Utc};
use ditto_capability::CapabilitySchema;
use ditto_context::{
    ContextCapsule, ContextCapsuleItem, ContextNodeKind, ContextOrigin, ContextScope,
    EpistemicStatus,
};
use ditto_model::{
    CancellationToken, ContentPart, ContinuationState, ConversationItem, ExecutionEpochId,
    FailureKind, FeatureRequest, FinishReason, MessageRole, ModelDriver, ModelEvent, ModelFeature,
    ModelRequest, ModelRequestId, ModelStreamEvent, ModelTurn, OutputConstraint, ParallelToolCalls,
    PromptCacheNamespace, PromptCachePolicy, ProviderCallId, ReasoningRequest, RequestControl,
    StableSystemPrefix, ToolChoice, ToolUsePolicy,
};
use ditto_model_openai::{
    MAX_ACTIVE_OUTPUT_ITEMS, MAX_COMPILED_REQUEST_BYTES, MAX_PROVIDER_CODE_BYTES,
    MAX_PROVIDER_MESSAGE_BYTES, MAX_SEEN_OUTPUT_ITEMS, MAX_SSE_EVENT_BYTES,
    OPENAI_CONTINUATION_FORMAT, OPENAI_PROVIDER, OpenAiHttpRequest, OpenAiHttpResponse,
    OpenAiResponsesDriver, OpenAiRetryPolicy, OpenAiStoragePolicy, OpenAiTransport,
    OpenAiTransportError, OpenAiTransportFuture,
};
use futures_util::{StreamExt, stream};
use serde_json::{Value, json};

const ARTIFACT_READ_PROVIDER_NAME: &str =
    "d_d24e7f8240cb6f0ce32385fb09615c711654420079024d334ad08d82751926";
const ARTIFACT_WRITE_PROVIDER_NAME: &str =
    "d_0321d621d4b1fa0947949dba543320ec17e77b575316b56dae2806a02d4d63";

#[derive(Debug)]
enum Outcome {
    Error(OpenAiTransportError),
    Chunks(Vec<Result<Vec<u8>, OpenAiTransportError>>),
    PendingHandshake,
    PendingBody,
}

#[derive(Debug, Clone)]
struct ScriptedTransport {
    outcomes: Arc<Mutex<VecDeque<Outcome>>>,
    requests: Arc<Mutex<Vec<Vec<u8>>>>,
    calls: Arc<AtomicUsize>,
    completed_handshakes: Arc<AtomicUsize>,
}

impl ScriptedTransport {
    fn new(outcomes: impl IntoIterator<Item = Outcome>) -> Self {
        Self {
            outcomes: Arc::new(Mutex::new(outcomes.into_iter().collect())),
            requests: Arc::new(Mutex::new(Vec::new())),
            calls: Arc::new(AtomicUsize::new(0)),
            completed_handshakes: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    fn requests(&self) -> Vec<Vec<u8>> {
        self.requests.lock().expect("requests lock").clone()
    }

    fn completed_handshakes(&self) -> usize {
        self.completed_handshakes.load(Ordering::SeqCst)
    }
}

impl OpenAiTransport for ScriptedTransport {
    fn send(&self, request: OpenAiHttpRequest) -> OpenAiTransportFuture {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.requests
            .lock()
            .expect("requests lock")
            .push(request.body().to_vec());
        let outcome = self
            .outcomes
            .lock()
            .expect("outcomes lock")
            .pop_front()
            .expect("scripted transport outcome");
        let completed_handshakes = Arc::clone(&self.completed_handshakes);
        Box::pin(async move {
            match outcome {
                Outcome::Error(error) => {
                    completed_handshakes.fetch_add(1, Ordering::SeqCst);
                    Err(error)
                }
                Outcome::Chunks(chunks) => {
                    completed_handshakes.fetch_add(1, Ordering::SeqCst);
                    Ok(OpenAiHttpResponse::new(stream::iter(chunks)))
                }
                Outcome::PendingHandshake => {
                    pending::<Result<OpenAiHttpResponse, OpenAiTransportError>>().await
                }
                Outcome::PendingBody => {
                    completed_handshakes.fetch_add(1, Ordering::SeqCst);
                    Ok(OpenAiHttpResponse::new(stream::pending()))
                }
            }
        })
    }
}

fn base_request(required: impl IntoIterator<Item = ModelFeature>) -> ModelRequest {
    let mut request = ModelRequest::new(
        ModelRequestId::new("openai-request-1").expect("request id"),
        ExecutionEpochId::new("openai-epoch-1").expect("epoch id"),
        StableSystemPrefix {
            segments: vec![
                "Stable instruction one.".into(),
                "Stable instruction two.".into(),
            ],
        },
        ModelTurn {
            conversation: vec![ConversationItem::Message {
                role: MessageRole::User,
                content: vec![ContentPart::Text {
                    text: "Hello".into(),
                }],
            }],
            context: ContextCapsule::default(),
            output: OutputConstraint::Text,
        },
    );
    request.features = FeatureRequest {
        required: required.into_iter().collect(),
        preferred: BTreeSet::new(),
    };
    request
}

fn tool_schema(id: &str) -> CapabilitySchema {
    CapabilitySchema {
        id: id.into(),
        version: "0.1.0".into(),
        summary: format!("Invoke {id}"),
        input_schema: json!({
            "$schema":"https://json-schema.org/draft/2020-12/schema",
            "type":"object",
            "properties":{"value":{"type":"string"}},
            "required":["value"],
            "additionalProperties":false
        }),
        output_schema: json!({
            "$schema":"https://json-schema.org/draft/2020-12/schema",
            "type":"object",
            "properties":{"result":{"type":"string"}},
            "required":["result"],
            "additionalProperties":false
        }),
    }
}

fn provider_tool_name(id: &str) -> &'static str {
    match id {
        "artifact.read" => ARTIFACT_READ_PROVIDER_NAME,
        "artifact.write" => ARTIFACT_WRITE_PROVIDER_NAME,
        other => panic!("missing independent provider-name fixture for {other}"),
    }
}

fn usage() -> Value {
    json!({
        "input_tokens": 21,
        "input_tokens_details":{"cached_tokens":7,"cache_write_tokens":2},
        "output_tokens": 13,
        "output_tokens_details":{"reasoning_tokens":3},
        "total_tokens":34
    })
}

fn wire_event(event_type: &str, sequence: u64, fields: Value) -> Value {
    let mut object = fields.as_object().expect("event fields object").clone();
    object.insert("type".into(), Value::String(event_type.into()));
    object.insert("sequence_number".into(), Value::from(sequence));
    Value::Object(object)
}

fn sse(events: &[Value]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for event in events {
        let event_type = event["type"].as_str().expect("event type");
        bytes.extend_from_slice(format!("event: {event_type}\n").as_bytes());
        bytes.extend_from_slice(b"data: ");
        bytes.extend_from_slice(
            serde_json::to_string(event)
                .expect("serialize source event")
                .as_bytes(),
        );
        bytes.extend_from_slice(b"\n\n");
    }
    bytes
}

fn response_metadata(
    id: &str,
    status: &str,
    store: bool,
    previous_response_id: Option<&str>,
) -> Value {
    json!({
        "id":id,
        "object":"response",
        "model":"gpt-5.6",
        "status":status,
        "store":store,
        "previous_response_id":previous_response_id
    })
}

fn text_success(text: &str) -> Vec<Value> {
    text_success_with_metadata(text, "resp_text_1", false, None)
}

fn text_success_with_metadata(
    text: &str,
    response_id: &str,
    store: bool,
    previous_response_id: Option<&str>,
) -> Vec<Value> {
    let mut created = response_metadata(response_id, "in_progress", store, previous_response_id);
    let mut completed = response_metadata(response_id, "completed", store, previous_response_id);
    completed["usage"] = usage();
    vec![
        wire_event("response.created", 0, json!({"response":created.take()})),
        wire_event(
            "response.output_item.added",
            1,
            json!({
                "output_index":0,
                "item":{"id":"msg_1","type":"message","role":"assistant","status":"in_progress","content":[]}
            }),
        ),
        wire_event(
            "response.content_part.added",
            2,
            json!({
                "item_id":"msg_1","output_index":0,"content_index":0,
                "part":{"type":"output_text","text":"","annotations":[]}
            }),
        ),
        wire_event(
            "response.output_text.delta",
            3,
            json!({"item_id":"msg_1","output_index":0,"content_index":0,"delta":text}),
        ),
        wire_event(
            "response.output_text.done",
            4,
            json!({"item_id":"msg_1","output_index":0,"content_index":0,"text":text}),
        ),
        wire_event(
            "response.content_part.done",
            5,
            json!({
                "item_id":"msg_1","output_index":0,"content_index":0,
                "part":{"type":"output_text","text":text,"annotations":[]}
            }),
        ),
        wire_event(
            "response.output_item.done",
            6,
            json!({
                "output_index":0,
                "item":{
                    "id":"msg_1","type":"message","role":"assistant","status":"completed",
                    "content":[{"type":"output_text","text":text,"annotations":[]}]
                }
            }),
        ),
        wire_event("response.completed", 7, json!({"response":completed})),
    ]
}

fn terminal_only(status: &str, terminal_usage: Option<Value>) -> Vec<Value> {
    let event_type = match status {
        "completed" => "response.completed",
        "incomplete" => "response.incomplete",
        other => panic!("unsupported terminal fixture status {other}"),
    };
    let mut terminal = response_metadata("resp_terminal", status, false, None);
    if let Some(terminal_usage) = terminal_usage {
        terminal["usage"] = terminal_usage;
    }
    if status == "incomplete" {
        terminal["incomplete_details"] = json!({"reason":"max_output_tokens"});
    }
    vec![
        wire_event(
            "response.created",
            0,
            json!({"response":response_metadata("resp_terminal", "in_progress", false, None)}),
        ),
        wire_event(event_type, 1, json!({"response":terminal})),
    ]
}

fn sequential_output_items(count: usize) -> Vec<Value> {
    let mut source = vec![wire_event(
        "response.created",
        0,
        json!({"response":response_metadata("resp_sequential", "in_progress", false, None)}),
    )];
    for index in 0..count {
        let item = json!({"id":format!("future_{index}"),"type":"future_item"});
        source.push(wire_event(
            "response.output_item.added",
            source.len() as u64,
            json!({"output_index":index,"item":item.clone()}),
        ));
        source.push(wire_event(
            "response.output_item.done",
            source.len() as u64,
            json!({"output_index":index,"item":item}),
        ));
    }
    source.push(wire_event(
        "response.completed",
        source.len() as u64,
        json!({"response":response_metadata("resp_sequential", "completed", false, None)}),
    ));
    source
}

fn chunks(body: Vec<u8>, size: usize) -> Outcome {
    Outcome::Chunks(body.chunks(size).map(|chunk| Ok(chunk.to_vec())).collect())
}

async fn collect(
    driver: &OpenAiResponsesDriver,
    request: ModelRequest,
    cancellation: CancellationToken,
) -> Vec<ModelStreamEvent> {
    driver.stream(request, cancellation).collect().await
}

fn assert_valid_success(events: &[ModelStreamEvent]) {
    assert_eq!(
        events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        (0..events.len() as u64).collect::<Vec<_>>()
    );
    assert!(events.iter().all(|event| event.validate().is_ok()));
    assert!(matches!(
        events.last().map(|event| &event.event),
        Some(ModelEvent::Completed { .. })
    ));
}

fn terminal_failure(events: &[ModelStreamEvent]) -> &ditto_model::ModelFailure {
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event.event, ModelEvent::Failed { .. }))
            .count(),
        1
    );
    match &events.last().expect("terminal event").event {
        ModelEvent::Failed { failure } => failure,
        other => panic!("expected terminal failure, got {other:?}"),
    }
}

#[tokio::test]
async fn split_at_every_byte_preserves_text_usage_and_terminal_order() {
    let body = sse(&text_success("Hello from OpenAI"));
    let transport = Arc::new(ScriptedTransport::new([chunks(body, 1)]));
    let driver =
        OpenAiResponsesDriver::gpt_5_6_with_transport(transport, OpenAiStoragePolicy::Ephemeral);
    let events = collect(
        &driver,
        base_request([ModelFeature::Text, ModelFeature::Usage]),
        CancellationToken::new(),
    )
    .await;
    assert_valid_success(&events);
    assert!(matches!(
        &events[0].event,
        ModelEvent::TextDelta { text } if text == "Hello from OpenAI"
    ));
    assert!(matches!(
        &events[1].event,
        ModelEvent::UsageUpdate { update }
            if update.usage.input_tokens == Some(21)
                && update.usage.cached_input_tokens == Some(7)
                && update.usage.output_tokens == Some(13)
                && update.usage.reasoning_tokens == Some(3)
                && update.usage.total_tokens == Some(34)
                && update.usage.details["cache_write_tokens"] == 2
    ));
    assert!(matches!(
        &events[2].event,
        ModelEvent::Completed {
            finish_reason: FinishReason::EndTurn,
            continuation: None
        }
    ));
}

#[tokio::test]
async fn terminal_usage_object_null_and_omission_follow_request_requirement() {
    for status in ["completed", "incomplete"] {
        for usage_required in [false, true] {
            let transport = Arc::new(ScriptedTransport::new([chunks(
                sse(&terminal_only(status, Some(usage()))),
                4096,
            )]));
            let driver = OpenAiResponsesDriver::gpt_5_6_with_transport(
                transport,
                OpenAiStoragePolicy::Ephemeral,
            );
            let required = usage_required.then_some(ModelFeature::Usage);
            let result = collect(&driver, base_request(required), CancellationToken::new()).await;
            assert_valid_success(&result);
            assert_eq!(
                result
                    .iter()
                    .filter(|event| matches!(event.event, ModelEvent::UsageUpdate { .. }))
                    .count(),
                1
            );
        }

        for terminal_usage in [None, Some(Value::Null)] {
            let transport = Arc::new(ScriptedTransport::new([chunks(
                sse(&terminal_only(status, terminal_usage.clone())),
                4096,
            )]));
            let driver = OpenAiResponsesDriver::gpt_5_6_with_transport(
                transport,
                OpenAiStoragePolicy::Ephemeral,
            );
            let optional_result =
                collect(&driver, base_request([]), CancellationToken::new()).await;
            assert_valid_success(&optional_result);
            assert!(
                optional_result
                    .iter()
                    .all(|event| !matches!(event.event, ModelEvent::UsageUpdate { .. }))
            );

            let transport = Arc::new(ScriptedTransport::new([chunks(
                sse(&terminal_only(status, terminal_usage)),
                4096,
            )]));
            let driver = OpenAiResponsesDriver::gpt_5_6_with_transport(
                transport,
                OpenAiStoragePolicy::Ephemeral,
            );
            let required_result = collect(
                &driver,
                base_request([ModelFeature::Usage]),
                CancellationToken::new(),
            )
            .await;
            assert_eq!(
                terminal_failure(&required_result).kind,
                FailureKind::Protocol
            );
        }

        let transport = Arc::new(ScriptedTransport::new([chunks(
            sse(&terminal_only(status, Some(json!("malformed")))),
            4096,
        )]));
        let driver = OpenAiResponsesDriver::gpt_5_6_with_transport(
            transport,
            OpenAiStoragePolicy::Ephemeral,
        );
        let malformed = collect(&driver, base_request([]), CancellationToken::new()).await;
        assert_eq!(terminal_failure(&malformed).kind, FailureKind::Protocol);
    }
}

#[tokio::test]
async fn terminal_status_is_optional_but_present_contradictions_fail_closed() {
    for status in ["completed", "incomplete"] {
        let mut source = terminal_only(status, Some(usage()));
        source[1]["response"]
            .as_object_mut()
            .expect("terminal response")
            .remove("status");
        let transport = Arc::new(ScriptedTransport::new([chunks(sse(&source), 4096)]));
        let driver = OpenAiResponsesDriver::gpt_5_6_with_transport(
            transport,
            OpenAiStoragePolicy::Ephemeral,
        );
        let result = collect(
            &driver,
            base_request([ModelFeature::Usage]),
            CancellationToken::new(),
        )
        .await;
        assert_valid_success(&result);
    }

    let failed_without_status = vec![
        wire_event(
            "response.created",
            0,
            json!({"response":response_metadata("resp_status_failed", "in_progress", false, None)}),
        ),
        wire_event("response.failed", 1, {
            let mut response = response_metadata("resp_status_failed", "failed", false, None);
            response
                .as_object_mut()
                .expect("failed response")
                .remove("status");
            response["error"] = json!({"code":"model_error","message":"failed without status"});
            json!({"response":response})
        }),
    ];
    let transport = Arc::new(ScriptedTransport::new([chunks(
        sse(&failed_without_status),
        4096,
    )]));
    let driver =
        OpenAiResponsesDriver::gpt_5_6_with_transport(transport, OpenAiStoragePolicy::Ephemeral);
    let result = collect(&driver, base_request([]), CancellationToken::new()).await;
    assert_eq!(terminal_failure(&result).kind, FailureKind::Provider);

    for (event_type, semantic_status, contradictory_status) in [
        ("response.completed", "completed", "incomplete"),
        ("response.incomplete", "incomplete", "completed"),
        ("response.failed", "failed", "completed"),
    ] {
        let mut response = response_metadata(
            "resp_status_contradiction",
            contradictory_status,
            false,
            None,
        );
        if semantic_status == "incomplete" {
            response["incomplete_details"] = json!({"reason":"max_output_tokens"});
        }
        if semantic_status == "failed" {
            response["error"] = json!({"code":"model_error","message":"contradiction"});
        }
        let source = vec![
            wire_event(
                "response.created",
                0,
                json!({"response":response_metadata("resp_status_contradiction", "in_progress", false, None)}),
            ),
            wire_event(event_type, 1, json!({"response":response})),
        ];
        let transport = Arc::new(ScriptedTransport::new([chunks(sse(&source), 4096)]));
        let driver = OpenAiResponsesDriver::gpt_5_6_with_transport(
            transport,
            OpenAiStoragePolicy::Ephemeral,
        );
        let result = collect(&driver, base_request([]), CancellationToken::new()).await;
        assert_eq!(terminal_failure(&result).kind, FailureKind::Protocol);
    }
}

#[tokio::test]
async fn request_bytes_are_exact_stable_and_context_keeps_epistemic_labeling() {
    let transport = Arc::new(ScriptedTransport::new([chunks(
        sse(&text_success("ok")),
        4096,
    )]));
    let driver = OpenAiResponsesDriver::gpt_5_6_with_transport(
        transport.clone(),
        OpenAiStoragePolicy::Ephemeral,
    );
    let mut request = base_request([ModelFeature::Text, ModelFeature::Usage]);
    request.turn.context.nodes.push(ContextCapsuleItem {
        id: "ctx-inferred-1".into(),
        kind: ContextNodeKind::Claim,
        summary: "This is inferred context.".into(),
        origin: ContextOrigin::Model,
        epistemic: EpistemicStatus::Inferred,
        scope: ContextScope::Turn,
        confidence: 0.75,
        source_event_ids: vec![],
        valid_from: None,
        valid_until: None,
    });
    request.turn.conversation[0] = ConversationItem::Message {
        role: MessageRole::User,
        content: vec![
            ContentPart::Text {
                text: "Use ".into(),
            },
            ContentPart::Structured {
                value: json!({"b":2,"a":1}),
            },
        ],
    };
    request.generation.prompt_cache = PromptCachePolicy::Automatic {
        namespace: Some(PromptCacheNamespace::new("stable-cache-key").expect("cache key")),
    };

    let _events = collect(&driver, request.clone(), CancellationToken::new()).await;
    let sent = transport.requests();
    assert_eq!(sent.len(), 1);
    let actual: Value = serde_json::from_slice(&sent[0]).expect("compiled request JSON");
    let capsule_json = serde_json::to_string(&request.turn.context).expect("capsule JSON");
    let tagged_content = serde_json::to_string(&json!([
        {"type":"text","text":"Use "},
        {"type":"structured","value":{"a":1,"b":2}}
    ]))
    .expect("tagged content");
    let expected = json!({
        "input":[
            {"role":"developer","content":format!("DITTO_CONTEXT_V1\n{capsule_json}")},
            {"role":"user","content":format!("DITTO_CONTENT_V1\n{tagged_content}")}
        ],
        "instructions":"Stable instruction one.\n\nStable instruction two.",
        "model":"gpt-5.6",
        "prompt_cache_key":"stable-cache-key",
        "prompt_cache_options":{"mode":"implicit"},
        "store":false,
        "stream":true
    });
    assert_eq!(
        sent[0],
        serde_json::to_vec(&expected).expect("expected bytes")
    );
    assert_eq!(actual["input"][0]["role"], "developer");
    assert_eq!(
        actual["input"][0]["content"],
        format!("DITTO_CONTEXT_V1\n{capsule_json}")
    );
    assert_eq!(actual["input"][1]["role"], "user");
    assert!(
        actual["input"][0]["content"]
            .as_str()
            .expect("context text")
            .contains("\"epistemic\":\"inferred\"")
    );
}

#[tokio::test]
async fn interleaved_partial_tool_calls_round_trip_provider_names_and_zero_delta_done() {
    let name_a = provider_tool_name("artifact.read");
    let name_b = provider_tool_name("artifact.write");
    let events = vec![
        wire_event(
            "response.created",
            0,
            json!({"response":response_metadata("resp_tools_1", "in_progress", false, None)}),
        ),
        wire_event(
            "response.output_item.added",
            1,
            json!({"output_index":0,"item":{"id":"fc_1","type":"function_call","call_id":"call_1","name":name_a,"arguments":"","status":"in_progress"}}),
        ),
        wire_event(
            "response.output_item.added",
            2,
            json!({"output_index":1,"item":{"id":"fc_2","type":"function_call","call_id":"call_2","name":name_b,"arguments":"","status":"in_progress"}}),
        ),
        wire_event(
            "response.function_call_arguments.delta",
            3,
            json!({"item_id":"fc_1","output_index":0,"delta":"{\"value\":\"a"}),
        ),
        wire_event(
            "response.function_call_arguments.delta",
            4,
            json!({"item_id":"fc_1","output_index":0,"delta":"1\"}"}),
        ),
        // No raw delta for call_2: arguments.done must supply one semantic delta.
        wire_event(
            "response.function_call_arguments.done",
            5,
            json!({"item_id":"fc_2","output_index":1,"call_id":"call_2","name":name_b,"arguments":"{\"value\":\"b1\"}"}),
        ),
        wire_event(
            "response.function_call_arguments.done",
            6,
            json!({"item_id":"fc_1","output_index":0,"call_id":"call_1","name":name_a,"arguments":"{\"value\":\"a1\"}"}),
        ),
        wire_event(
            "response.output_item.done",
            7,
            json!({"output_index":1,"item":{"id":"fc_2","type":"function_call","call_id":"call_2","name":name_b,"arguments":"{\"value\":\"b1\"}","status":"completed"}}),
        ),
        wire_event(
            "response.output_item.done",
            8,
            json!({"output_index":0,"item":{"id":"fc_1","type":"function_call","call_id":"call_1","name":name_a,"arguments":"{\"value\":\"a1\"}","status":"completed"}}),
        ),
        wire_event("response.completed", 9, {
            let mut response = response_metadata("resp_tools_1", "completed", false, None);
            response["usage"] = usage();
            json!({"response":response})
        }),
    ];
    let transport = Arc::new(ScriptedTransport::new([chunks(sse(&events), 7)]));
    let driver = OpenAiResponsesDriver::gpt_5_6_with_transport(
        transport.clone(),
        OpenAiStoragePolicy::Ephemeral,
    );
    let mut request = base_request([ModelFeature::ToolCalls, ModelFeature::Usage]);
    request.tools = vec![tool_schema("artifact.read"), tool_schema("artifact.write")];
    request.generation.tool_use = ToolUsePolicy {
        choice: ToolChoice::Required,
        parallel_calls: ParallelToolCalls::Allow,
    };
    let result = collect(&driver, request, CancellationToken::new()).await;
    assert_valid_success(&result);
    let semantic = result.iter().map(|event| &event.event).collect::<Vec<_>>();
    assert!(
        matches!(semantic[0], ModelEvent::ToolCallStarted { call_id, capability_id } if call_id.as_str() == "call_1" && capability_id == "artifact.read")
    );
    assert!(
        matches!(semantic[1], ModelEvent::ToolCallStarted { call_id, capability_id } if call_id.as_str() == "call_2" && capability_id == "artifact.write")
    );
    assert!(semantic.iter().any(|event| matches!(event, ModelEvent::ToolCallArgumentDelta { call_id, delta } if call_id.as_str() == "call_2" && delta == "{\"value\":\"b1\"}")));
    assert!(semantic.iter().any(|event| matches!(event, ModelEvent::ToolCallReady { call_id, capability_id, arguments } if call_id.as_str() == "call_1" && capability_id == "artifact.read" && arguments["value"] == "a1")));
    assert!(semantic.iter().any(|event| matches!(event, ModelEvent::ToolCallReady { call_id, capability_id, arguments } if call_id.as_str() == "call_2" && capability_id == "artifact.write" && arguments["value"] == "b1")));
    assert!(matches!(
        semantic.last(),
        Some(ModelEvent::Completed {
            finish_reason: FinishReason::ToolCalls,
            ..
        })
    ));

    let body: Value = serde_json::from_slice(&transport.requests()[0]).expect("request body");
    assert_eq!(
        body["tools"][0]["name"],
        provider_tool_name("artifact.read")
    );
    assert_eq!(
        body["tools"][1]["name"],
        provider_tool_name("artifact.write")
    );
    assert_eq!(body["tool_choice"], "required");
    assert_eq!(body["parallel_tool_calls"], true);
}

#[tokio::test]
async fn structured_output_is_parsed_once_before_usage_and_completion() {
    let text = "{\"answer\":42}";
    let transport = Arc::new(ScriptedTransport::new([chunks(
        sse(&text_success(text)),
        31,
    )]));
    let driver = OpenAiResponsesDriver::gpt_5_6_with_transport(
        transport.clone(),
        OpenAiStoragePolicy::Ephemeral,
    );
    let mut request = base_request([
        ModelFeature::Text,
        ModelFeature::StructuredOutput,
        ModelFeature::Usage,
    ]);
    request.turn.output = OutputConstraint::Structured {
        name: "answer.schema".into(),
        schema: json!({
            "type":"object",
            "properties":{"answer":{"type":"integer"}},
            "required":["answer"],
            "additionalProperties":false
        }),
        strict: true,
    };
    let events = collect(&driver, request, CancellationToken::new()).await;
    assert_valid_success(&events);
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event.event, ModelEvent::StructuredOutput { .. }))
            .count(),
        1
    );
    assert!(matches!(
        &events[1].event,
        ModelEvent::StructuredOutput { value } if value == &json!({"answer":42})
    ));
    let body: Value = serde_json::from_slice(&transport.requests()[0]).expect("request body");
    assert_eq!(body["text"]["format"]["type"], "json_schema");
    assert_eq!(body["text"]["format"]["strict"], true);
    assert_ne!(body["text"]["format"]["name"], "answer.schema");
}

#[tokio::test]
async fn correlated_tool_history_specific_choice_and_disabled_cache_compile_exactly() {
    let transport = Arc::new(ScriptedTransport::new([chunks(
        sse(&text_success("ok")),
        4096,
    )]));
    let driver = OpenAiResponsesDriver::gpt_5_6_with_transport(
        transport.clone(),
        OpenAiStoragePolicy::Ephemeral,
    );
    let call_id = ProviderCallId::new("call_history_1").expect("call id");
    let mut request = base_request([ModelFeature::ToolCalls]);
    request.tools = vec![tool_schema("artifact.read")];
    request.turn.conversation.extend([
        ConversationItem::ToolCall {
            call_id: call_id.clone(),
            capability_id: "artifact.read".into(),
            arguments: json!({"z":2,"a":1}),
        },
        ConversationItem::ToolResult {
            call_id,
            content: vec![ContentPart::Structured {
                value: json!({"z":"last","a":"first"}),
            }],
            is_error: false,
        },
        ConversationItem::Message {
            role: MessageRole::Assistant,
            content: vec![ContentPart::Text {
                text: "History preserved.".into(),
            }],
        },
    ]);
    request.generation.prompt_cache = PromptCachePolicy::Disabled;
    request.generation.tool_use = ToolUsePolicy {
        choice: ToolChoice::Specific {
            capability_id: "artifact.read".into(),
        },
        parallel_calls: ParallelToolCalls::Forbid,
    };
    let _ = collect(&driver, request, CancellationToken::new()).await;
    let body: Value = serde_json::from_slice(&transport.requests()[0]).expect("request body");
    let name = provider_tool_name("artifact.read");
    assert_eq!(body["input"][0]["role"], "user");
    assert_eq!(body["input"][1]["type"], "function_call");
    assert_eq!(body["input"][1]["call_id"], "call_history_1");
    assert_eq!(body["input"][1]["name"], name);
    assert_eq!(body["input"][1]["arguments"], "{\"a\":1,\"z\":2}");
    assert_eq!(body["input"][2]["type"], "function_call_output");
    assert_eq!(body["input"][2]["call_id"], "call_history_1");
    assert!(
        body["input"][2]["output"]
            .as_str()
            .expect("tool result output")
            .contains("{\"a\":\"first\",\"z\":\"last\"}")
    );
    assert_eq!(body["input"][3]["role"], "assistant");
    assert_eq!(body["prompt_cache_options"], json!({"mode":"explicit"}));
    assert_eq!(body["tool_choice"], json!({"type":"function","name":name}));
    assert_eq!(body["parallel_tool_calls"], false);
}

#[tokio::test]
async fn provider_managed_storage_emits_and_consumes_exact_response_id_continuation() {
    let transport = Arc::new(ScriptedTransport::new([
        chunks(
            sse(&text_success_with_metadata(
                "first",
                "resp_text_1",
                true,
                None,
            )),
            4096,
        ),
        chunks(
            sse(&text_success_with_metadata(
                "second",
                "resp_text_2",
                true,
                Some("resp_text_1"),
            )),
            4096,
        ),
    ]));
    let driver = OpenAiResponsesDriver::gpt_5_6_with_transport(
        transport.clone(),
        OpenAiStoragePolicy::ProviderManaged,
    );
    let first = collect(
        &driver,
        base_request([
            ModelFeature::Text,
            ModelFeature::Usage,
            ModelFeature::Continuation,
        ]),
        CancellationToken::new(),
    )
    .await;
    let continuation = match &first.last().expect("completion").event {
        ModelEvent::Completed {
            continuation: Some(continuation),
            ..
        } => continuation.clone(),
        other => panic!("expected continuation, got {other:?}"),
    };
    assert_eq!(continuation.provider(), OPENAI_PROVIDER);
    assert_eq!(continuation.format(), OPENAI_CONTINUATION_FORMAT);
    assert_eq!(
        continuation.state().value(),
        &json!({"response_id":"resp_text_1"})
    );

    let mut second = base_request([ModelFeature::Text, ModelFeature::Usage]);
    second.continuation = Some(continuation);
    let result = collect(&driver, second, CancellationToken::new()).await;
    assert_valid_success(&result);
    let requests = transport.requests();
    let body: Value = serde_json::from_slice(&requests[1]).expect("continuation request");
    assert_eq!(body["previous_response_id"], "resp_text_1");
    assert_eq!(body["store"], true);
    assert_eq!(
        body["instructions"],
        "Stable instruction one.\n\nStable instruction two."
    );
}

#[tokio::test]
async fn response_profile_storage_and_previous_id_are_correlated_before_completion() {
    let mut wrong_model = text_success("wrong model");
    wrong_model[7]["response"]["model"] = json!("gpt-5.5");
    let transport = Arc::new(ScriptedTransport::new([chunks(sse(&wrong_model), 4096)]));
    let driver =
        OpenAiResponsesDriver::gpt_5_6_with_transport(transport, OpenAiStoragePolicy::Ephemeral);
    let result = collect(&driver, base_request([]), CancellationToken::new()).await;
    assert_eq!(terminal_failure(&result).kind, FailureKind::Protocol);
    assert!(
        result
            .iter()
            .all(|event| !matches!(event.event, ModelEvent::Completed { .. }))
    );

    let mut wrong_store = text_success_with_metadata("wrong store", "resp_store", true, None);
    wrong_store[7]["response"]["store"] = json!(false);
    let transport = Arc::new(ScriptedTransport::new([chunks(sse(&wrong_store), 4096)]));
    let driver = OpenAiResponsesDriver::gpt_5_6_with_transport(
        transport,
        OpenAiStoragePolicy::ProviderManaged,
    );
    let result = collect(&driver, base_request([]), CancellationToken::new()).await;
    assert_eq!(terminal_failure(&result).kind, FailureKind::Protocol);
    assert!(result.iter().all(|event| {
        !matches!(
            event.event,
            ModelEvent::Completed {
                continuation: Some(_),
                ..
            }
        )
    }));

    let previous = ContinuationState::new(
        OPENAI_PROVIDER,
        OPENAI_CONTINUATION_FORMAT,
        json!({"response_id":"resp_expected_previous"}),
    )
    .expect("continuation");
    let mut wrong_previous = text_success_with_metadata(
        "wrong previous",
        "resp_new",
        true,
        Some("resp_expected_previous"),
    );
    wrong_previous[7]["response"]["previous_response_id"] = json!("resp_different_previous");
    let transport = Arc::new(ScriptedTransport::new([chunks(sse(&wrong_previous), 4096)]));
    let driver = OpenAiResponsesDriver::gpt_5_6_with_transport(
        transport,
        OpenAiStoragePolicy::ProviderManaged,
    );
    let mut request = base_request([]);
    request.continuation = Some(previous);
    let result = collect(&driver, request, CancellationToken::new()).await;
    assert_eq!(terminal_failure(&result).kind, FailureKind::Protocol);
}

#[tokio::test]
async fn required_response_identity_fields_fail_closed_but_optional_echoes_may_be_omitted() {
    for field in ["object", "model"] {
        let mut source = text_success("missing identity");
        source[0]["response"]
            .as_object_mut()
            .expect("created response")
            .remove(field);
        let transport = Arc::new(ScriptedTransport::new([chunks(sse(&source), 4096)]));
        let driver = OpenAiResponsesDriver::gpt_5_6_with_transport(
            transport,
            OpenAiStoragePolicy::Ephemeral,
        );
        let result = collect(&driver, base_request([]), CancellationToken::new()).await;
        assert_eq!(terminal_failure(&result).kind, FailureKind::Protocol);
    }

    let mut optional_omitted = text_success("optional echoes omitted");
    for index in [0, 7] {
        let response = optional_omitted[index]["response"]
            .as_object_mut()
            .expect("response object");
        response.remove("store");
        response.remove("previous_response_id");
    }
    let transport = Arc::new(ScriptedTransport::new([chunks(
        sse(&optional_omitted),
        4096,
    )]));
    let driver =
        OpenAiResponsesDriver::gpt_5_6_with_transport(transport, OpenAiStoragePolicy::Ephemeral);
    let result = collect(&driver, base_request([]), CancellationToken::new()).await;
    assert_valid_success(&result);
}

#[tokio::test]
async fn wrong_continuation_and_checkpointed_tool_suffix_fail_before_io() {
    let transport = Arc::new(ScriptedTransport::new([]));
    let ephemeral = OpenAiResponsesDriver::gpt_5_6_with_transport(
        transport.clone(),
        OpenAiStoragePolicy::Ephemeral,
    );
    let mut request = base_request([ModelFeature::Text]);
    request.continuation = Some(
        ContinuationState::new(
            OPENAI_PROVIDER,
            OPENAI_CONTINUATION_FORMAT,
            json!({"response_id":"resp_1"}),
        )
        .expect("continuation"),
    );
    let failure = collect(&ephemeral, request, CancellationToken::new()).await;
    assert_eq!(
        terminal_failure(&failure).kind,
        FailureKind::UnsupportedFeature
    );
    assert_eq!(transport.calls(), 0);

    let managed = OpenAiResponsesDriver::gpt_5_6_with_transport(
        transport.clone(),
        OpenAiStoragePolicy::ProviderManaged,
    );
    let mut wrong_format = base_request([ModelFeature::Text]);
    wrong_format.continuation = Some(
        ContinuationState::new(
            OPENAI_PROVIDER,
            "responses-other-v2",
            json!({"response_id":"resp_1"}),
        )
        .expect("continuation"),
    );
    let failure = collect(&managed, wrong_format, CancellationToken::new()).await;
    assert_eq!(
        terminal_failure(&failure).kind,
        FailureKind::UnsupportedFeature
    );

    let mut wrong_shape = base_request([ModelFeature::Text]);
    wrong_shape.continuation = Some(
        ContinuationState::new(
            OPENAI_PROVIDER,
            OPENAI_CONTINUATION_FORMAT,
            json!({"response_id":"resp_1","extra":true}),
        )
        .expect("continuation"),
    );
    let failure = collect(&managed, wrong_shape, CancellationToken::new()).await;
    assert_eq!(
        terminal_failure(&failure).kind,
        FailureKind::UnsupportedFeature
    );

    let mut suffix = base_request([ModelFeature::Text, ModelFeature::ToolCalls]);
    suffix.tools = vec![tool_schema("artifact.read")];
    suffix.turn.conversation = vec![
        ConversationItem::ToolCall {
            call_id: ProviderCallId::new("call_prior").expect("call id"),
            capability_id: "artifact.read".into(),
            arguments: json!({"value":"x"}),
        },
        ConversationItem::ToolResult {
            call_id: ProviderCallId::new("call_prior").expect("call id"),
            content: vec![ContentPart::Text {
                text: "done".into(),
            }],
            is_error: false,
        },
    ];
    suffix.continuation = Some(
        ContinuationState::new(
            OPENAI_PROVIDER,
            OPENAI_CONTINUATION_FORMAT,
            json!({"response_id":"resp_1"}),
        )
        .expect("continuation"),
    );
    let failure = collect(&managed, suffix, CancellationToken::new()).await;
    assert_eq!(
        terminal_failure(&failure).kind,
        FailureKind::UnsupportedFeature
    );
    assert_eq!(transport.calls(), 0);
}

#[tokio::test]
async fn incomplete_finish_reasons_accept_both_token_spellings_and_content_filter() {
    for (reason, expected) in [
        ("max_tokens", FinishReason::MaxOutputTokens),
        ("max_output_tokens", FinishReason::MaxOutputTokens),
        ("content_filter", FinishReason::ContentFilter),
        (
            "provider_limit",
            FinishReason::Other("provider_limit".into()),
        ),
    ] {
        let source = vec![
            wire_event(
                "response.created",
                0,
                json!({"response":response_metadata("resp_incomplete", "in_progress", false, None)}),
            ),
            wire_event("response.incomplete", 1, {
                let mut response = response_metadata("resp_incomplete", "incomplete", false, None);
                response["usage"] = usage();
                response["incomplete_details"] = json!({"reason":reason});
                json!({"response":response})
            }),
        ];
        let transport = Arc::new(ScriptedTransport::new([chunks(sse(&source), 4096)]));
        let driver = OpenAiResponsesDriver::gpt_5_6_with_transport(
            transport,
            OpenAiStoragePolicy::Ephemeral,
        );
        let result = collect(
            &driver,
            base_request([ModelFeature::Usage]),
            CancellationToken::new(),
        )
        .await;
        assert!(matches!(
            result.last().map(|event| &event.event),
            Some(ModelEvent::Completed { finish_reason, continuation: None }) if finish_reason == &expected
        ));
    }
}

#[tokio::test]
async fn refusal_delta_is_text_but_terminal_reason_remains_refusal() {
    let source = vec![
        wire_event(
            "response.created",
            0,
            json!({"response":response_metadata("resp_refusal", "in_progress", false, None)}),
        ),
        wire_event(
            "response.output_item.added",
            1,
            json!({"output_index":0,"item":{"id":"msg_refusal","type":"message","role":"assistant","status":"in_progress","content":[]}}),
        ),
        wire_event(
            "response.content_part.added",
            2,
            json!({"item_id":"msg_refusal","output_index":0,"content_index":0,"part":{"type":"refusal","refusal":""}}),
        ),
        wire_event(
            "response.refusal.delta",
            3,
            json!({"item_id":"msg_refusal","output_index":0,"content_index":0,"delta":"I cannot comply."}),
        ),
        wire_event(
            "response.refusal.done",
            4,
            json!({"item_id":"msg_refusal","output_index":0,"content_index":0,"refusal":"I cannot comply."}),
        ),
        wire_event(
            "response.content_part.done",
            5,
            json!({"item_id":"msg_refusal","output_index":0,"content_index":0,"part":{"type":"refusal","refusal":"I cannot comply."}}),
        ),
        wire_event(
            "response.output_item.done",
            6,
            json!({"output_index":0,"item":{"id":"msg_refusal","type":"message","role":"assistant","status":"completed","content":[{"type":"refusal","refusal":"I cannot comply."}]}}),
        ),
        wire_event("response.completed", 7, {
            let mut response = response_metadata("resp_refusal", "completed", false, None);
            response["usage"] = usage();
            json!({"response":response})
        }),
    ];
    let transport = Arc::new(ScriptedTransport::new([chunks(sse(&source), 17)]));
    let driver =
        OpenAiResponsesDriver::gpt_5_6_with_transport(transport, OpenAiStoragePolicy::Ephemeral);
    let result = collect(
        &driver,
        base_request([ModelFeature::Text, ModelFeature::Usage]),
        CancellationToken::new(),
    )
    .await;
    assert!(
        matches!(&result[0].event, ModelEvent::TextDelta { text } if text == "I cannot comply.")
    );
    assert!(matches!(
        result.last().map(|event| &event.event),
        Some(ModelEvent::Completed {
            finish_reason: FinishReason::Refusal,
            ..
        })
    ));
}

#[tokio::test]
async fn provider_failures_and_known_nested_cancel_are_typed_and_bounded() {
    let cases = [
        vec![wire_event(
            "error",
            0,
            json!({"code":"server_error","message":"provider exploded"}),
        )],
        vec![
            wire_event(
                "response.created",
                0,
                json!({"response":response_metadata("resp_failed", "in_progress", false, None)}),
            ),
            wire_event("response.failed", 1, {
                let mut response = response_metadata("resp_failed", "failed", false, None);
                response["error"] = json!({"code":"model_error","message":"generation failed"});
                json!({"response":response})
            }),
        ],
        vec![
            wire_event(
                "response.created",
                0,
                json!({"response":response_metadata("resp_cancelled", "in_progress", false, None)}),
            ),
            wire_event("response.failed", 1, {
                let mut response = response_metadata("resp_cancelled", "cancelled", false, None);
                response["error"] = Value::Null;
                json!({"response":response})
            }),
        ],
    ];
    for (index, source) in cases.into_iter().enumerate() {
        let transport = Arc::new(ScriptedTransport::new([chunks(sse(&source), 4096)]));
        let driver = OpenAiResponsesDriver::gpt_5_6_with_transport(
            transport,
            OpenAiStoragePolicy::Ephemeral,
        );
        let result = collect(&driver, base_request([]), CancellationToken::new()).await;
        let failure = terminal_failure(&result);
        if index == 2 {
            assert_eq!(failure.kind, FailureKind::Cancelled);
        } else {
            assert_eq!(failure.kind, FailureKind::Provider);
            assert!(failure.provider_code.is_some());
        }
    }
}

#[tokio::test]
async fn in_band_provider_failures_scrub_exact_and_masked_credential_tokens() {
    const EXACT_CANARY: &str = "sk-proj-DITTO_SSE_SECRET_7f19";
    const MASKED_CANARY: &str = "sk-proj-...T_7f19";
    let standalone = vec![wire_event(
        "error",
        0,
        json!({
            "code":format!("invalid_{EXACT_CANARY}"),
            "message":format!("rejected {EXACT_CANARY}; masked {MASKED_CANARY}")
        }),
    )];
    let nested = vec![
        wire_event(
            "response.created",
            0,
            json!({"response":response_metadata("resp_secret_failure", "in_progress", false, None)}),
        ),
        wire_event("response.failed", 1, {
            let mut response = response_metadata("resp_secret_failure", "failed", false, None);
            response["error"] = json!({
                "code":format!("invalid_{MASKED_CANARY}"),
                "message":format!("provider echoed {EXACT_CANARY} and {MASKED_CANARY}")
            });
            json!({"response":response})
        }),
    ];

    for source in [standalone, nested] {
        let transport = Arc::new(ScriptedTransport::new([chunks(sse(&source), 4096)]));
        let driver = OpenAiResponsesDriver::gpt_5_6_with_transport(
            transport,
            OpenAiStoragePolicy::Ephemeral,
        );
        let result = collect(&driver, base_request([]), CancellationToken::new()).await;
        let failure = terminal_failure(&result);
        assert_eq!(failure.kind, FailureKind::Provider);
        assert!(failure.message.len() <= MAX_PROVIDER_MESSAGE_BYTES);
        assert!(
            failure
                .provider_code
                .as_deref()
                .expect("provider code")
                .len()
                <= MAX_PROVIDER_CODE_BYTES
        );
        for exposed in [
            format!("{result:?}"),
            format!("{failure:?}"),
            failure.message.clone(),
            failure.provider_code.clone().expect("provider code"),
        ] {
            assert!(!exposed.contains(EXACT_CANARY));
            assert!(!exposed.contains(MASKED_CANARY));
        }
        assert!(failure.message.contains("<redacted>"));
        assert!(
            failure
                .provider_code
                .as_deref()
                .expect("provider code")
                .contains("<redacted>")
        );
    }
}

#[tokio::test]
async fn undocumented_cancel_event_and_done_marker_are_not_success() {
    for source in [
        vec![wire_event(
            "response.cancelled",
            0,
            json!({"response":{"id":"resp_unknown","status":"cancelled"}}),
        )],
        Vec::new(),
    ] {
        let mut body = sse(&source);
        body.extend_from_slice(b"data: [DONE]\n\n");
        let transport = Arc::new(ScriptedTransport::new([chunks(body, 4096)]));
        let driver = OpenAiResponsesDriver::gpt_5_6_with_transport(
            transport,
            OpenAiStoragePolicy::Ephemeral,
        );
        let result = collect(&driver, base_request([]), CancellationToken::new()).await;
        assert_eq!(terminal_failure(&result).kind, FailureKind::Protocol);
    }
}

#[tokio::test]
async fn malformed_sse_json_envelope_sequence_identity_and_finals_fail_closed() {
    let mut cases = Vec::new();
    cases.push(b"event: response.created\ndata: {not-json}\n\n".to_vec());
    cases.push({
        let value = wire_event(
            "response.created",
            0,
            json!({"response":response_metadata("resp_1", "in_progress", false, None)}),
        );
        format!(
            "event: response.completed\ndata: {}\n\n",
            serde_json::to_string(&value).expect("event JSON")
        )
        .into_bytes()
    });
    cases.push(sse(&[
        wire_event(
            "response.created",
            1,
            json!({"response":response_metadata("resp_1", "in_progress", false, None)}),
        ),
        wire_event(
            "response.in_progress",
            1,
            json!({"response":response_metadata("resp_1", "in_progress", false, None)}),
        ),
    ]));
    let mut mismatched_text = text_success("hello");
    mismatched_text[4]["text"] = json!("different");
    cases.push(sse(&mismatched_text));
    let mut changed_index = text_success("hello");
    changed_index[3]["output_index"] = json!(9);
    cases.push(sse(&changed_index));

    for body in cases {
        let transport = Arc::new(ScriptedTransport::new([chunks(body, 3)]));
        let driver = OpenAiResponsesDriver::gpt_5_6_with_transport(
            transport,
            OpenAiStoragePolicy::Ephemeral,
        );
        let result = collect(&driver, base_request([]), CancellationToken::new()).await;
        assert_eq!(terminal_failure(&result).kind, FailureKind::Protocol);
    }
}

#[tokio::test]
async fn final_message_and_function_metadata_must_match_streamed_state() {
    let mut wrong_status = text_success("status");
    wrong_status[6]["item"]["status"] = json!("incomplete");
    let mut wrong_content = text_success("content");
    wrong_content[6]["item"]["content"][0]["text"] = json!("changed");
    let mut missing_content = text_success("missing");
    missing_content[6]["item"]
        .as_object_mut()
        .expect("final message item")
        .remove("content");

    for source in [wrong_status, wrong_content, missing_content] {
        let transport = Arc::new(ScriptedTransport::new([chunks(sse(&source), 4096)]));
        let driver = OpenAiResponsesDriver::gpt_5_6_with_transport(
            transport,
            OpenAiStoragePolicy::Ephemeral,
        );
        let result = collect(&driver, base_request([]), CancellationToken::new()).await;
        assert_eq!(terminal_failure(&result).kind, FailureKind::Protocol);
    }

    let source = vec![
        wire_event(
            "response.created",
            0,
            json!({"response":response_metadata("resp_function_metadata", "in_progress", false, None)}),
        ),
        wire_event(
            "response.output_item.added",
            1,
            json!({
                "output_index":0,
                "item":{
                    "id":"fc_metadata","type":"function_call","call_id":"call_metadata",
                    "name":ARTIFACT_READ_PROVIDER_NAME,"arguments":"","status":"in_progress"
                }
            }),
        ),
        wire_event(
            "response.function_call_arguments.done",
            2,
            json!({
                "item_id":"fc_metadata","output_index":0,"call_id":"call_metadata",
                "name":ARTIFACT_WRITE_PROVIDER_NAME,"arguments":"{}"
            }),
        ),
    ];
    let transport = Arc::new(ScriptedTransport::new([chunks(sse(&source), 4096)]));
    let driver =
        OpenAiResponsesDriver::gpt_5_6_with_transport(transport, OpenAiStoragePolicy::Ephemeral);
    let mut request = base_request([ModelFeature::ToolCalls]);
    request.tools = vec![tool_schema("artifact.read")];
    let result = collect(&driver, request, CancellationToken::new()).await;
    let failure = terminal_failure(&result);
    assert_eq!(failure.kind, FailureKind::Protocol);
    assert_eq!(
        failure.call_id.as_ref().map(ProviderCallId::as_str),
        Some("call_metadata")
    );
}

#[tokio::test]
async fn post_terminal_failure_preserves_chunk_independent_prefix_and_allows_done() {
    let mut source = text_success("stable prefix");
    let terminal = source.pop().expect("terminal event");
    let trailing = wire_event("response.future.after_terminal", 8, json!({"opaque":true}));
    let prefix_bytes = sse(&source);
    let terminal_and_invalid = sse(&[terminal, trailing]);
    let mut coalesced_bytes = prefix_bytes.clone();
    coalesced_bytes.extend_from_slice(&terminal_and_invalid);
    let coalesced_len = coalesced_bytes.len();

    let coalesced_transport = Arc::new(ScriptedTransport::new([chunks(
        coalesced_bytes,
        coalesced_len,
    )]));
    let coalesced_driver = OpenAiResponsesDriver::gpt_5_6_with_transport(
        coalesced_transport,
        OpenAiStoragePolicy::Ephemeral,
    );
    let coalesced = collect(
        &coalesced_driver,
        base_request([ModelFeature::Text, ModelFeature::Usage]),
        CancellationToken::new(),
    )
    .await;

    let fragmented_transport = Arc::new(ScriptedTransport::new([Outcome::Chunks(vec![
        Ok(prefix_bytes),
        Ok(terminal_and_invalid),
    ])]));
    let fragmented_driver = OpenAiResponsesDriver::gpt_5_6_with_transport(
        fragmented_transport,
        OpenAiStoragePolicy::Ephemeral,
    );
    let fragmented = collect(
        &fragmented_driver,
        base_request([ModelFeature::Text, ModelFeature::Usage]),
        CancellationToken::new(),
    )
    .await;

    assert_eq!(coalesced, fragmented);
    assert!(matches!(
        &coalesced[0].event,
        ModelEvent::TextDelta { text } if text == "stable prefix"
    ));
    assert_eq!(terminal_failure(&coalesced).kind, FailureKind::Protocol);
    assert!(coalesced.iter().all(|event| !matches!(
        event.event,
        ModelEvent::UsageUpdate { .. } | ModelEvent::Completed { .. }
    )));

    let mut done_body = sse(&text_success("done sentinel"));
    done_body.extend_from_slice(b"data: [DONE]\n\n");
    let done_len = done_body.len();
    let transport = Arc::new(ScriptedTransport::new([chunks(done_body, done_len)]));
    let driver =
        OpenAiResponsesDriver::gpt_5_6_with_transport(transport, OpenAiStoragePolicy::Ephemeral);
    let result = collect(
        &driver,
        base_request([ModelFeature::Text, ModelFeature::Usage]),
        CancellationToken::new(),
    )
    .await;
    assert_valid_success(&result);
    assert_eq!(
        result
            .iter()
            .filter(|event| event.event.is_terminal())
            .count(),
        1
    );
}

#[tokio::test]
async fn malformed_and_mismatched_tool_arguments_are_typed_failures() {
    for (deltas, done, expected) in [
        (vec!["{bad"], "{bad", FailureKind::MalformedToolArguments),
        (
            vec!["{\"value\":\"a\"}"],
            "{\"value\":\"b\"}",
            FailureKind::Protocol,
        ),
    ] {
        let name = provider_tool_name("artifact.read");
        let mut source = vec![
            wire_event(
                "response.created",
                0,
                json!({"response":response_metadata("resp_tool_bad", "in_progress", false, None)}),
            ),
            wire_event(
                "response.output_item.added",
                1,
                json!({"output_index":0,"item":{"id":"fc_bad","type":"function_call","call_id":"call_bad","name":name,"arguments":"","status":"in_progress"}}),
            ),
        ];
        for (offset, delta) in deltas.into_iter().enumerate() {
            source.push(wire_event(
                "response.function_call_arguments.delta",
                2 + offset as u64,
                json!({"item_id":"fc_bad","output_index":0,"delta":delta}),
            ));
        }
        source.push(wire_event(
            "response.function_call_arguments.done",
            source.len() as u64,
            json!({"item_id":"fc_bad","output_index":0,"arguments":done}),
        ));
        let transport = Arc::new(ScriptedTransport::new([chunks(sse(&source), 4096)]));
        let driver = OpenAiResponsesDriver::gpt_5_6_with_transport(
            transport,
            OpenAiStoragePolicy::Ephemeral,
        );
        let mut request = base_request([ModelFeature::ToolCalls]);
        request.tools = vec![tool_schema("artifact.read")];
        let result = collect(&driver, request, CancellationToken::new()).await;
        let failure = terminal_failure(&result);
        assert_eq!(failure.kind, expected);
        assert_eq!(
            failure.call_id.as_ref().map(ProviderCallId::as_str),
            Some("call_bad")
        );
    }
}

#[tokio::test]
async fn unknown_future_events_are_ignored_only_after_valid_envelope_and_sequence() {
    let mut source = text_success("known text");
    source.insert(
        1,
        wire_event(
            "response.future.telemetry",
            1,
            json!({"opaque":{"anything":true}}),
        ),
    );
    for (index, event) in source.iter_mut().enumerate().skip(2) {
        event["sequence_number"] = json!(index as u64);
    }
    let transport = Arc::new(ScriptedTransport::new([chunks(sse(&source), 5)]));
    let driver =
        OpenAiResponsesDriver::gpt_5_6_with_transport(transport, OpenAiStoragePolicy::Ephemeral);
    let result = collect(
        &driver,
        base_request([ModelFeature::Text, ModelFeature::Usage]),
        CancellationToken::new(),
    )
    .await;
    assert_valid_success(&result);
    assert_eq!(
        result
            .iter()
            .filter(|event| matches!(event.event, ModelEvent::TextDelta { .. }))
            .count(),
        1
    );
}

#[tokio::test]
async fn eligible_pre_response_failure_retries_but_body_failure_never_retries() {
    let retry = OpenAiRetryPolicy::new(2, Duration::ZERO, Duration::ZERO, Duration::ZERO)
        .expect("retry policy");
    let transport = Arc::new(ScriptedTransport::new([
        Outcome::Error(OpenAiTransportError::connection("connect failed")),
        chunks(sse(&text_success("retried")), 4096),
    ]));
    let driver = OpenAiResponsesDriver::gpt_5_6_with_transport_and_retry(
        transport.clone(),
        OpenAiStoragePolicy::Ephemeral,
        retry,
    );
    let result = collect(
        &driver,
        base_request([ModelFeature::Text, ModelFeature::Usage]),
        CancellationToken::new(),
    )
    .await;
    assert_valid_success(&result);
    assert_eq!(transport.calls(), 2);
    assert_eq!(transport.requests()[0], transport.requests()[1]);

    let created = sse(&[wire_event(
        "response.created",
        0,
        json!({"response":response_metadata("resp_created_then_broke", "in_progress", false, None)}),
    )]);
    let transport = Arc::new(ScriptedTransport::new([
        Outcome::Chunks(vec![
            Ok(created),
            Err(OpenAiTransportError::body("body broke")),
        ]),
        chunks(sse(&text_success("must not happen")), 4096),
    ]));
    let driver = OpenAiResponsesDriver::gpt_5_6_with_transport_and_retry(
        transport.clone(),
        OpenAiStoragePolicy::Ephemeral,
        retry,
    );
    let result = collect(&driver, base_request([]), CancellationToken::new()).await;
    assert_eq!(terminal_failure(&result).kind, FailureKind::Transport);
    assert_eq!(transport.calls(), 1);
}

#[tokio::test]
async fn quota_failure_is_not_retried() {
    let quota = serde_json::to_vec(&json!({
        "error":{"code":"insufficient_quota","type":"insufficient_quota","message":"quota"}
    }))
    .expect("quota body");
    let transport = Arc::new(ScriptedTransport::new([
        Outcome::Error(OpenAiTransportError::http_status(
            429,
            &quota,
            Some(Duration::ZERO),
        )),
        chunks(sse(&text_success("must not retry")), 4096),
    ]));
    let driver = OpenAiResponsesDriver::gpt_5_6_with_transport(
        transport.clone(),
        OpenAiStoragePolicy::Ephemeral,
    );
    let result = collect(&driver, base_request([]), CancellationToken::new()).await;
    assert_eq!(terminal_failure(&result).kind, FailureKind::Transport);
    assert_eq!(transport.calls(), 1);
}

#[tokio::test]
async fn unsupported_controls_boolean_tool_schema_and_oversize_request_fail_before_io() {
    let transport = Arc::new(ScriptedTransport::new([]));
    let driver = OpenAiResponsesDriver::gpt_5_6_with_transport(
        transport.clone(),
        OpenAiStoragePolicy::Ephemeral,
    );

    let mut reasoning = base_request([]);
    reasoning.generation.reasoning = Some(ReasoningRequest::default());
    let result = collect(&driver, reasoning, CancellationToken::new()).await;
    assert_eq!(
        terminal_failure(&result).kind,
        FailureKind::UnsupportedFeature
    );

    let mut stable_breakpoint = base_request([]);
    stable_breakpoint.generation.prompt_cache = PromptCachePolicy::StablePrefix {
        namespace: None,
        ttl_seconds: None,
    };
    let result = collect(&driver, stable_breakpoint, CancellationToken::new()).await;
    assert_eq!(
        terminal_failure(&result).kind,
        FailureKind::UnsupportedFeature
    );

    let mut boolean_schema = base_request([ModelFeature::ToolCalls]);
    let mut schema = tool_schema("artifact.read");
    schema.input_schema = Value::Bool(true);
    boolean_schema.tools = vec![schema];
    let result = collect(&driver, boolean_schema, CancellationToken::new()).await;
    assert_eq!(
        terminal_failure(&result).kind,
        FailureKind::UnsupportedFeature
    );

    let mut oversized = base_request([]);
    oversized.stable_system_prefix.segments = vec!["x".repeat(MAX_COMPILED_REQUEST_BYTES + 1)];
    let result = collect(&driver, oversized, CancellationToken::new()).await;
    assert_eq!(terminal_failure(&result).kind, FailureKind::Protocol);
    assert_eq!(transport.calls(), 0);
}

#[tokio::test]
async fn sse_event_and_active_output_state_limits_fail_closed() {
    let oversized = {
        let mut body = b"data: ".to_vec();
        body.extend(std::iter::repeat_n(b'x', MAX_SSE_EVENT_BYTES + 1));
        body.extend_from_slice(b"\n\n");
        body
    };
    let transport = Arc::new(ScriptedTransport::new([chunks(oversized, 65_536)]));
    let driver =
        OpenAiResponsesDriver::gpt_5_6_with_transport(transport, OpenAiStoragePolicy::Ephemeral);
    let result = collect(&driver, base_request([]), CancellationToken::new()).await;
    assert_eq!(terminal_failure(&result).kind, FailureKind::Protocol);

    let mut source = vec![wire_event(
        "response.created",
        0,
        json!({"response":response_metadata("resp_many", "in_progress", false, None)}),
    )];
    for index in 0..=MAX_ACTIVE_OUTPUT_ITEMS {
        source.push(wire_event(
            "response.output_item.added",
            index as u64 + 1,
            json!({
                "output_index":index,
                "item":{"id":format!("future_{index}"),"type":"future_item"}
            }),
        ));
    }
    let transport = Arc::new(ScriptedTransport::new([chunks(sse(&source), 4096)]));
    let driver =
        OpenAiResponsesDriver::gpt_5_6_with_transport(transport, OpenAiStoragePolicy::Ephemeral);
    let result = collect(&driver, base_request([]), CancellationToken::new()).await;
    assert_eq!(terminal_failure(&result).kind, FailureKind::Protocol);
}

#[tokio::test]
async fn total_sequential_output_history_accepts_n_and_rejects_n_plus_one() {
    let at_limit = sequential_output_items(MAX_SEEN_OUTPUT_ITEMS);
    let transport = Arc::new(ScriptedTransport::new([chunks(sse(&at_limit), 4096)]));
    let driver =
        OpenAiResponsesDriver::gpt_5_6_with_transport(transport, OpenAiStoragePolicy::Ephemeral);
    let result = collect(&driver, base_request([]), CancellationToken::new()).await;
    assert_valid_success(&result);

    let over_limit = sequential_output_items(MAX_SEEN_OUTPUT_ITEMS + 1);
    let transport = Arc::new(ScriptedTransport::new([chunks(sse(&over_limit), 4096)]));
    let driver =
        OpenAiResponsesDriver::gpt_5_6_with_transport(transport, OpenAiStoragePolicy::Ephemeral);
    let result = collect(&driver, base_request([]), CancellationToken::new()).await;
    let failure = terminal_failure(&result);
    assert_eq!(failure.kind, FailureKind::Protocol);
    assert!(failure.message.contains("total output-item history limit"));
}

#[tokio::test]
async fn bounded_retry_after_is_honored_until_the_request_deadline() {
    let rate_limit = serde_json::to_vec(&json!({
        "error":{"code":"rate_limit_exceeded","type":"rate_limit_error","message":"slow down"}
    }))
    .expect("rate-limit body");
    let transport = Arc::new(ScriptedTransport::new([
        Outcome::Error(OpenAiTransportError::http_status(
            429,
            &rate_limit,
            Some(Duration::from_millis(500)),
        )),
        chunks(sse(&text_success("too early")), 4096),
    ]));
    let retry = OpenAiRetryPolicy::new(2, Duration::ZERO, Duration::ZERO, Duration::from_secs(1))
        .expect("retry policy");
    let driver = OpenAiResponsesDriver::gpt_5_6_with_transport_and_retry(
        transport.clone(),
        OpenAiStoragePolicy::Ephemeral,
        retry,
    );
    let mut request = base_request([]);
    request.control.deadline = Some(Utc::now() + ChronoDuration::milliseconds(200));
    let result = collect(&driver, request, CancellationToken::new()).await;
    assert_eq!(
        terminal_failure(&result).kind,
        FailureKind::DeadlineExceeded
    );
    assert_eq!(transport.calls(), 1);
}

async fn wait_for_call(transport: &ScriptedTransport) {
    for _ in 0..100 {
        if transport.calls() > 0 {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("transport was not called");
}

async fn wait_for_completed_handshake(transport: &ScriptedTransport) {
    for _ in 0..100 {
        if transport.completed_handshakes() > 0 {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("transport handshake did not complete");
}

#[derive(Clone, Copy)]
enum CancellationPhase {
    Handshake,
    Body,
    Backoff,
}

#[tokio::test(flavor = "current_thread")]
async fn cancellation_propagates_during_handshake_body_and_backoff() {
    for (phase, outcome, retry, expected_message) in [
        (
            CancellationPhase::Handshake,
            Outcome::PendingHandshake,
            OpenAiRetryPolicy::default(),
            "transport handshake",
        ),
        (
            CancellationPhase::Body,
            Outcome::PendingBody,
            OpenAiRetryPolicy::default(),
            "while streaming",
        ),
        (
            CancellationPhase::Backoff,
            Outcome::Error(OpenAiTransportError::connection("retry me")),
            OpenAiRetryPolicy::new(
                2,
                Duration::from_secs(30),
                Duration::from_secs(30),
                Duration::from_secs(30),
            )
            .expect("retry policy"),
            "retry backoff",
        ),
    ] {
        let transport = Arc::new(ScriptedTransport::new([outcome]));
        let driver = Arc::new(OpenAiResponsesDriver::gpt_5_6_with_transport_and_retry(
            transport.clone(),
            OpenAiStoragePolicy::Ephemeral,
            retry,
        ));
        let cancellation = CancellationToken::new();
        let task_cancellation = cancellation.clone();
        let task =
            tokio::spawn(
                async move { collect(&driver, base_request([]), task_cancellation).await },
            );
        match phase {
            CancellationPhase::Handshake => wait_for_call(&transport).await,
            CancellationPhase::Body | CancellationPhase::Backoff => {
                wait_for_completed_handshake(&transport).await;
            }
        }
        cancellation.cancel();
        let result = tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .expect("cancellation must unblock")
            .expect("collection task");
        let failure = terminal_failure(&result);
        assert_eq!(failure.kind, FailureKind::Cancelled);
        assert!(failure.message.contains(expected_message));
        assert_eq!(transport.calls(), 1);
    }
}

#[tokio::test]
async fn deadline_propagates_during_handshake_body_and_backoff() {
    for (outcome, retry) in [
        (Outcome::PendingHandshake, OpenAiRetryPolicy::default()),
        (Outcome::PendingBody, OpenAiRetryPolicy::default()),
        (
            Outcome::Error(OpenAiTransportError::connection("retry me")),
            OpenAiRetryPolicy::new(
                2,
                Duration::from_secs(30),
                Duration::from_secs(30),
                Duration::from_secs(30),
            )
            .expect("retry policy"),
        ),
    ] {
        let transport = Arc::new(ScriptedTransport::new([outcome]));
        let driver = OpenAiResponsesDriver::gpt_5_6_with_transport_and_retry(
            transport.clone(),
            OpenAiStoragePolicy::Ephemeral,
            retry,
        );
        let mut request = base_request([]);
        request.control = RequestControl {
            cancellation_id: None,
            deadline: Some(Utc::now() + ChronoDuration::milliseconds(200)),
        };
        let result = tokio::time::timeout(
            Duration::from_secs(1),
            collect(&driver, request, CancellationToken::new()),
        )
        .await
        .expect("deadline must unblock");
        assert_eq!(
            terminal_failure(&result).kind,
            FailureKind::DeadlineExceeded
        );
        assert_eq!(transport.calls(), 1);
    }
}
