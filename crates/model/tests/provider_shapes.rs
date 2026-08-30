//! Provider-shaped integration fixtures.
//!
//! These translators intentionally live in the integration test crate. They
//! exercise the provider-neutral IR from representative wire-shaped values,
//! without adding a provider SDK/HTTP client or claiming production adapter
//! behavior.

use std::collections::BTreeMap;

use chrono::Utc;
use ditto_capability::CapabilitySchema;
use ditto_context::ContextCapsule;
use ditto_model::{
    CancellationToken, ContentPart, ContinuationState, DriverId, ExecutionEpochId, FixtureDriver,
    FixtureFrame, GenerationControls, MessageRole, ModelDriver, ModelEvent, ModelFeature,
    ModelRequest, ModelRequestId, ModelStreamEvent, ModelTurn, OpaqueReasoningState,
    OutputConstraint, ProviderCallId, ProviderWarning, ReasoningItem, ReasoningItemId,
    ReasoningSegment, ReasoningSegmentKey, ReasoningTextKind, StableSystemPrefix, TokenUsage,
    UsageSemantics, UsageUpdate,
};
use futures_util::StreamExt;
use serde_json::{Map, Value, json};

const TOOL_ID: &str = "artifact.read";

type ShapeResult<T> = Result<T, String>;

fn field<'a>(value: &'a Value, name: &str) -> ShapeResult<&'a Value> {
    value
        .get(name)
        .ok_or_else(|| format!("source event is missing {name:?}"))
}

fn string_field<'a>(value: &'a Value, name: &str) -> ShapeResult<&'a str> {
    field(value, name)?
        .as_str()
        .ok_or_else(|| format!("source field {name:?} is not a string"))
}

fn object_field<'a>(value: &'a Value, name: &str) -> ShapeResult<&'a Map<String, Value>> {
    field(value, name)?
        .as_object()
        .ok_or_else(|| format!("source field {name:?} is not an object"))
}

fn index_field(value: &Value, name: &str) -> ShapeResult<usize> {
    let index = field(value, name)?
        .as_u64()
        .ok_or_else(|| format!("source field {name:?} is not an unsigned integer"))?;
    usize::try_from(index).map_err(|_| format!("source field {name:?} exceeds usize"))
}

fn u32_field(value: &Value, name: &str) -> ShapeResult<u32> {
    let number = field(value, name)?
        .as_u64()
        .ok_or_else(|| format!("source field {name:?} is not an unsigned integer"))?;
    u32::try_from(number).map_err(|_| format!("source field {name:?} exceeds u32"))
}

fn optional_u64(value: &Value, name: &str) -> ShapeResult<Option<u64>> {
    match value.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(number) => number
            .as_u64()
            .map(Some)
            .ok_or_else(|| format!("source field {name:?} is not an unsigned integer")),
    }
}

fn provider_call_id(value: &str) -> ShapeResult<ProviderCallId> {
    ProviderCallId::new(value).map_err(|error| error.to_string())
}

fn reasoning_item_id(value: &str) -> ShapeResult<ReasoningItemId> {
    ReasoningItemId::new(value).map_err(|error| error.to_string())
}

fn reasoning_state(provider: &str, format: &str, data: &str) -> ShapeResult<OpaqueReasoningState> {
    OpaqueReasoningState::new(provider, format, data).map_err(|error| error.to_string())
}

fn reasoning_item(
    id: ReasoningItemId,
    segments: Vec<ReasoningSegment>,
    state: Option<OpaqueReasoningState>,
) -> ReasoningItem {
    ReasoningItem {
        id,
        segments,
        state,
    }
}

fn tool_schema() -> CapabilitySchema {
    CapabilitySchema {
        id: TOOL_ID.into(),
        version: "0.1.0".into(),
        summary: "Read a bounded artifact range.".into(),
        input_schema: json!({
            "$schema":"https://json-schema.org/draft/2020-12/schema",
            "type":"object",
            "properties":{"reference":{"type":"string"}},
            "required":["reference"],
            "additionalProperties":false
        }),
        output_schema: json!({
            "$schema":"https://json-schema.org/draft/2020-12/schema",
            "type":"object",
            "properties":{"content":{"type":"string"}},
            "required":["content"],
            "additionalProperties":false
        }),
    }
}

/// The fixture driver has no provider request compiler or request capabilities.
/// Default generation controls are therefore the valid configuration for this
/// semantic replay; the required set still proves exactly which IR events are
/// expected from each source mapping.
fn valid_request(required: impl IntoIterator<Item = ModelFeature>) -> ModelRequest {
    let mut request = ModelRequest::new(
        ModelRequestId::new("provider-shape-request").expect("valid request id"),
        ExecutionEpochId::new("provider-shape-epoch").expect("valid epoch id"),
        StableSystemPrefix {
            segments: vec![
                "You are Ditto.".into(),
                "Use the supplied artifact tool when needed.".into(),
            ],
        },
        ModelTurn {
            conversation: vec![ditto_model::ConversationItem::Message {
                role: MessageRole::User,
                content: vec![ContentPart::Text {
                    text: "Read the artifact.".into(),
                }],
            }],
            context: ContextCapsule::default(),
            output: OutputConstraint::Text,
        },
    );
    request.tools.push(tool_schema());
    request.features.required.extend(required);
    request.generation = GenerationControls::default();
    request
}

async fn collect(driver: &FixtureDriver, request: ModelRequest) -> Vec<ModelStreamEvent> {
    driver
        .stream(request, CancellationToken::new())
        .collect()
        .await
}

fn assert_contiguous_valid_events(events: &[ModelStreamEvent]) {
    assert_eq!(
        events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        (0..events.len() as u64).collect::<Vec<_>>()
    );
    assert!(events.iter().all(|event| event.validate().is_ok()));
    assert!(
        events
            .iter()
            .all(|event| { !matches!(event.event, ModelEvent::Failed { .. }) })
    );
}

fn openai_responses_shape() -> Vec<Value> {
    vec![
        json!({
            "type":"response.created",
            "response":{"id":"resp_openai_shape_1","status":"in_progress"}
        }),
        json!({
            "type":"response.output_item.added",
            "output_index":0,
            "item":{"id":"rs_openai_shape_1","type":"reasoning","status":"in_progress","summary":[]}
        }),
        json!({
            "type":"response.reasoning_summary_text.delta",
            "item_id":"rs_openai_shape_1",
            "output_index":0,
            "summary_index":0,
            "delta":"Inspect the artifact "
        }),
        json!({
            "type":"response.reasoning_summary_text.delta",
            "item_id":"rs_openai_shape_1",
            "output_index":0,
            "summary_index":0,
            "delta":"before reading it."
        }),
        json!({
            "type":"response.output_item.done",
            "output_index":0,
            "item":{
                "id":"rs_openai_shape_1",
                "type":"reasoning",
                "status":"completed",
                "summary":[{"type":"summary_text","text":"Inspect the artifact before reading it."}],
                "encrypted_content":"enc-openai-reasoning-v1"
            }
        }),
        json!({
            "type":"response.output_item.added",
            "output_index":1,
            "item":{
                "id":"fc_openai_item_1",
                "type":"function_call",
                "status":"in_progress",
                "call_id":"call_openai_shape_1",
                "name":"artifact.read",
                "arguments":""
            }
        }),
        json!({
            "type":"response.function_call_arguments.delta",
            "item_id":"fc_openai_item_1",
            "output_index":1,
            "delta":"{\"reference\":\"sha256:openai-"
        }),
        json!({
            "type":"response.function_call_arguments.delta",
            "item_id":"fc_openai_item_1",
            "output_index":1,
            "delta":"artifact\"}"
        }),
        json!({
            "type":"response.function_call_arguments.done",
            "item_id":"fc_openai_item_1",
            "output_index":1,
            "arguments":"{\"reference\":\"sha256:openai-artifact\"}"
        }),
        json!({
            "type":"response.output_item.done",
            "output_index":1,
            "item":{
                "id":"fc_openai_item_1",
                "type":"function_call",
                "status":"completed",
                "call_id":"call_openai_shape_1",
                "name":"artifact.read",
                "arguments":"{\"reference\":\"sha256:openai-artifact\"}"
            }
        }),
        json!({
            "type":"response.completed",
            "response":{
                "id":"resp_openai_shape_1",
                "status":"completed",
                "usage":{
                    "input_tokens":24,
                    "input_tokens_details":{"cached_tokens":8},
                    "output_tokens":14,
                    "output_tokens_details":{"reasoning_tokens":4},
                    "total_tokens":38,
                    "cache_write_tokens":2
                }
            }
        }),
    ]
}

#[derive(Debug)]
enum OpenAiItem {
    Reasoning {
        output_index: usize,
        summaries: BTreeMap<u32, String>,
    },
    Function {
        output_index: usize,
        call_id: String,
        capability_id: String,
        arguments: String,
        done_arguments: Option<String>,
    },
}

fn map_openai_responses(events: &[Value]) -> ShapeResult<Vec<FixtureFrame>> {
    let mut frames = Vec::new();
    let mut items = BTreeMap::<String, OpenAiItem>::new();
    let mut response_id = None;
    let mut completed = false;

    for (position, event) in events.iter().enumerate() {
        if completed {
            return Err(format!(
                "OpenAI event {position} follows response.completed"
            ));
        }
        let event_type = string_field(event, "type")?;
        match event_type {
            "response.created" => {
                let response = object_field(event, "response")?;
                response_id = Some(
                    response
                        .get("id")
                        .and_then(Value::as_str)
                        .ok_or_else(|| "source response.id is not a string".to_owned())?
                        .to_owned(),
                );
            }
            "response.output_item.added" => {
                let output_index = index_field(event, "output_index")?;
                let item = object_field(event, "item")?;
                let id = string_field(&Value::Object(item.clone()), "id")?.to_owned();
                if items.contains_key(&id) {
                    return Err(format!("OpenAI output item {id} started twice"));
                }
                match item.get("type").and_then(Value::as_str) {
                    Some("reasoning") => {
                        items.insert(
                            id.clone(),
                            OpenAiItem::Reasoning {
                                output_index,
                                summaries: BTreeMap::new(),
                            },
                        );
                        frames.push(FixtureFrame::ReasoningItemStarted {
                            item_id: reasoning_item_id(&id)?,
                        });
                    }
                    Some("function_call") => {
                        let call_id =
                            string_field(&Value::Object(item.clone()), "call_id")?.to_owned();
                        let capability_id =
                            string_field(&Value::Object(item.clone()), "name")?.to_owned();
                        items.insert(
                            id,
                            OpenAiItem::Function {
                                output_index,
                                call_id: call_id.clone(),
                                capability_id: capability_id.clone(),
                                arguments: String::new(),
                                done_arguments: None,
                            },
                        );
                        frames.push(FixtureFrame::ToolCallStarted {
                            call_id: provider_call_id(&call_id)?,
                            capability_id,
                        });
                    }
                    Some(other) => return Err(format!("unsupported OpenAI output item {other}")),
                    None => return Err("OpenAI output item has no type".into()),
                }
            }
            "response.reasoning_summary_text.delta" => {
                let item_id = string_field(event, "item_id")?;
                let output_index = index_field(event, "output_index")?;
                let summary_index = u32_field(event, "summary_index")?;
                let delta = string_field(event, "delta")?;
                let Some(OpenAiItem::Reasoning {
                    output_index: expected_index,
                    summaries,
                }) = items.get_mut(item_id)
                else {
                    return Err(format!(
                        "OpenAI reasoning delta references unknown item {item_id}"
                    ));
                };
                if *expected_index != output_index {
                    return Err(format!(
                        "OpenAI reasoning item {item_id} changed output index"
                    ));
                }
                summaries.entry(summary_index).or_default().push_str(delta);
                frames.push(FixtureFrame::ReasoningDelta {
                    item_id: reasoning_item_id(item_id)?,
                    segment: ReasoningSegmentKey {
                        kind: ReasoningTextKind::Summary,
                        index: summary_index,
                    },
                    delta: delta.into(),
                });
            }
            "response.function_call_arguments.delta" => {
                let item_id = string_field(event, "item_id")?;
                let output_index = index_field(event, "output_index")?;
                let delta = string_field(event, "delta")?;
                let Some(OpenAiItem::Function {
                    output_index: expected_index,
                    call_id,
                    arguments,
                    ..
                }) = items.get_mut(item_id)
                else {
                    return Err(format!(
                        "OpenAI function arguments reference unknown output item {item_id}"
                    ));
                };
                if *expected_index != output_index {
                    return Err(format!(
                        "OpenAI function item {item_id} changed output index"
                    ));
                }
                arguments.push_str(delta);
                frames.push(FixtureFrame::ToolCallArgumentDelta {
                    call_id: provider_call_id(call_id)?,
                    delta: delta.into(),
                });
            }
            "response.function_call_arguments.done" => {
                let item_id = string_field(event, "item_id")?;
                let output_index = index_field(event, "output_index")?;
                let done_arguments = string_field(event, "arguments")?.to_owned();
                let Some(OpenAiItem::Function {
                    output_index: expected_index,
                    arguments,
                    done_arguments: stored_done,
                    ..
                }) = items.get_mut(item_id)
                else {
                    return Err(format!(
                        "OpenAI completed arguments reference unknown output item {item_id}"
                    ));
                };
                if *expected_index != output_index || *arguments != done_arguments {
                    return Err(format!(
                        "OpenAI completed arguments do not match item {item_id}"
                    ));
                }
                *stored_done = Some(done_arguments);
            }
            "response.output_item.done" => {
                let output_index = index_field(event, "output_index")?;
                let item = object_field(event, "item")?;
                let id = item
                    .get("id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "OpenAI completed item has no id".to_owned())?;
                let item_type = item
                    .get("type")
                    .and_then(Value::as_str)
                    .ok_or_else(|| format!("OpenAI completed item {id} has no type"))?;
                let Some(source_item) = items.remove(id) else {
                    return Err(format!("OpenAI completed unknown output item {id}"));
                };
                match (source_item, item_type) {
                    (
                        OpenAiItem::Reasoning {
                            output_index: expected_index,
                            summaries,
                        },
                        "reasoning",
                    ) => {
                        if expected_index != output_index {
                            return Err(format!("OpenAI reasoning item {id} changed output index"));
                        }
                        let final_summary = item
                            .get("summary")
                            .and_then(Value::as_array)
                            .ok_or_else(|| format!("OpenAI reasoning item {id} has no summary"))?;
                        for (summary_index, summary) in final_summary.iter().enumerate() {
                            let text = summary
                                .get("text")
                                .and_then(Value::as_str)
                                .ok_or_else(|| "OpenAI summary entry has no text".to_owned())?;
                            if summaries.get(&(summary_index as u32)).map(String::as_str)
                                != Some(text)
                            {
                                return Err(format!(
                                    "OpenAI final summary does not match deltas for item {id}"
                                ));
                            }
                        }
                        let encrypted = item
                            .get("encrypted_content")
                            .and_then(Value::as_str)
                            .ok_or_else(|| {
                                format!("OpenAI reasoning item {id} has no encrypted content")
                            })?;
                        let segments = summaries
                            .into_iter()
                            .map(|(index, text)| ReasoningSegment {
                                key: ReasoningSegmentKey {
                                    kind: ReasoningTextKind::Summary,
                                    index,
                                },
                                text,
                            })
                            .collect();
                        frames.push(FixtureFrame::ReasoningItemReady {
                            item: reasoning_item(
                                reasoning_item_id(id)?,
                                segments,
                                Some(reasoning_state(
                                    "openai",
                                    "responses-reasoning-encrypted-v1",
                                    encrypted,
                                )?),
                            ),
                        });
                    }
                    (
                        OpenAiItem::Function {
                            output_index: expected_index,
                            call_id,
                            capability_id,
                            arguments,
                            done_arguments,
                        },
                        "function_call",
                    ) => {
                        if expected_index != output_index {
                            return Err(format!("OpenAI function item {id} changed output index"));
                        }
                        let final_call_id = item
                            .get("call_id")
                            .and_then(Value::as_str)
                            .ok_or_else(|| format!("OpenAI function item {id} has no call_id"))?;
                        let final_name = item
                            .get("name")
                            .and_then(Value::as_str)
                            .ok_or_else(|| format!("OpenAI function item {id} has no name"))?;
                        let final_arguments = item
                            .get("arguments")
                            .and_then(Value::as_str)
                            .ok_or_else(|| format!("OpenAI function item {id} has no arguments"))?;
                        if final_call_id != call_id
                            || final_name != capability_id
                            || final_arguments != arguments
                            || done_arguments.as_deref() != Some(arguments.as_str())
                        {
                            return Err(format!(
                                "OpenAI final function item does not match deltas for item {id}"
                            ));
                        }
                        frames.push(FixtureFrame::ToolCallReady {
                            call_id: provider_call_id(&call_id)?,
                        });
                    }
                    (_, other) => {
                        return Err(format!("OpenAI output item {id} changed type to {other}"));
                    }
                }
            }
            "response.completed" => {
                let response = object_field(event, "response")?;
                let completed_id = response
                    .get("id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "OpenAI response.completed has no response id".to_owned())?;
                if response_id.as_deref() != Some(completed_id) {
                    return Err("OpenAI response id changed before completion".into());
                }
                if !items.is_empty() {
                    return Err("OpenAI response completed with unfinished output items".into());
                }
                let usage = response
                    .get("usage")
                    .ok_or_else(|| "OpenAI response.completed has no usage".to_owned())?;
                let input_tokens = usage
                    .get("input_tokens")
                    .and_then(Value::as_u64)
                    .ok_or_else(|| "OpenAI usage has no input_tokens".to_owned())?;
                let output_tokens = usage
                    .get("output_tokens")
                    .and_then(Value::as_u64)
                    .ok_or_else(|| "OpenAI usage has no output_tokens".to_owned())?;
                let total_tokens = usage
                    .get("total_tokens")
                    .and_then(Value::as_u64)
                    .ok_or_else(|| "OpenAI usage has no total_tokens".to_owned())?;
                let cached_input_tokens = object_field(usage, "input_tokens_details")?
                    .get("cached_tokens")
                    .and_then(Value::as_u64);
                let reasoning_tokens = object_field(usage, "output_tokens_details")?
                    .get("reasoning_tokens")
                    .and_then(Value::as_u64);
                let mut details = BTreeMap::new();
                if let Some(cache_write_tokens) =
                    usage.get("cache_write_tokens").and_then(Value::as_u64)
                {
                    details.insert("cache_write_tokens".into(), cache_write_tokens);
                }
                frames.push(FixtureFrame::UsageUpdate {
                    update: UsageUpdate {
                        semantics: UsageSemantics::Cumulative,
                        usage: TokenUsage {
                            input_tokens: Some(input_tokens),
                            output_tokens: Some(output_tokens),
                            cached_input_tokens,
                            reasoning_tokens,
                            total_tokens: Some(total_tokens),
                            details,
                        },
                    },
                });
                frames.push(FixtureFrame::Completed {
                    finish_reason: ditto_model::FinishReason::ToolCalls,
                    continuation: Some(
                        ContinuationState::new(
                            "openai",
                            "responses-v1",
                            json!({
                                "response_id":completed_id,
                                "reasoning_item_id":"rs_openai_shape_1"
                            }),
                        )
                        .map_err(|error| error.to_string())?,
                    ),
                });
                completed = true;
            }
            other => return Err(format!("unsupported OpenAI event type {other}")),
        }
    }
    if !completed {
        return Err("OpenAI fixture did not reach response.completed".into());
    }
    Ok(frames)
}

fn anthropic_messages_shape() -> Vec<Value> {
    vec![
        json!({
            "type":"message_start",
            "message":{
                "id":"msg_anthropic_shape_1",
                "type":"message",
                "role":"assistant",
                "content":[],
                "model":"claude-shape-fixture",
                "stop_reason":null,
                "stop_sequence":null,
                "usage":{
                    "input_tokens":27,
                    "cache_read_input_tokens":4,
                    "cache_creation_input_tokens":2,
                    "output_tokens":1
                }
            }
        }),
        json!({
            "type":"content_block_start",
            "index":0,
            "content_block":{"type":"thinking","thinking":""}
        }),
        json!({
            "type":"content_block_delta",
            "index":0,
            "delta":{"type":"thinking_delta","thinking":"Check "}
        }),
        json!({
            "type":"content_block_delta",
            "index":0,
            "delta":{"type":"thinking_delta","thinking":"the artifact."}
        }),
        json!({
            "type":"content_block_delta",
            "index":0,
            "delta":{"type":"signature_delta","signature":"sig-anthropic-thinking-v1"}
        }),
        json!({"type":"content_block_stop","index":0}),
        json!({
            "type":"content_block_start",
            "index":1,
            "content_block":{
                "type":"tool_use",
                "id":"toolu_anthropic_shape_1",
                "name":"artifact.read",
                "input":{}
            }
        }),
        json!({
            "type":"content_block_delta",
            "index":1,
            "delta":{"type":"input_json_delta","partial_json":"{\"reference\":\"sha256:anth"}
        }),
        json!({
            "type":"content_block_delta",
            "index":1,
            "delta":{"type":"input_json_delta","partial_json":"ropic-artifact\"}"}
        }),
        json!({"type":"content_block_stop","index":1}),
        json!({"type":"ping"}),
        json!({
            "type":"message_delta",
            "delta":{"stop_reason":"tool_use","stop_sequence":null},
            "usage":{"output_tokens":11}
        }),
        json!({"type":"message_stop"}),
    ]
}

#[derive(Debug)]
enum AnthropicBlock {
    Thinking {
        item_id: ReasoningItemId,
        summary: String,
        signature: Option<String>,
    },
    Tool {
        call_id: ProviderCallId,
        capability_id: String,
    },
}

fn map_anthropic_messages(events: &[Value]) -> ShapeResult<Vec<FixtureFrame>> {
    let mut frames = Vec::new();
    let mut blocks = BTreeMap::<usize, AnthropicBlock>::new();
    let mut message_id = None;
    let mut input_tokens = None;
    let mut cached_input_tokens = None;
    let mut cache_creation_input_tokens = None;
    let mut stop_reason = None;
    let mut saw_message_delta = false;
    let mut completed = false;

    for (position, event) in events.iter().enumerate() {
        if completed {
            return Err(format!("Anthropic event {position} follows message_stop"));
        }
        let event_type = string_field(event, "type")?;
        match event_type {
            "message_start" => {
                if message_id.is_some() {
                    return Err("Anthropic message_start appeared twice".into());
                }
                let message = object_field(event, "message")?;
                let message_value = Value::Object(message.clone());
                message_id = Some(string_field(&message_value, "id")?.to_owned());
                let usage = object_field(&message_value, "usage")?;
                input_tokens = optional_u64(&Value::Object(usage.clone()), "input_tokens")?;
                cached_input_tokens =
                    optional_u64(&Value::Object(usage.clone()), "cache_read_input_tokens")?;
                cache_creation_input_tokens =
                    optional_u64(&Value::Object(usage.clone()), "cache_creation_input_tokens")?;
            }
            "content_block_start" => {
                let index = index_field(event, "index")?;
                if blocks.contains_key(&index) {
                    return Err(format!("Anthropic content block {index} started twice"));
                }
                let content_block = object_field(event, "content_block")?;
                let block_value = Value::Object(content_block.clone());
                let block_type = string_field(&block_value, "type")?;
                let block = match block_type {
                    "thinking" => AnthropicBlock::Thinking {
                        item_id: reasoning_item_id(&format!("anthropic-thinking-{index}"))?,
                        summary: String::new(),
                        signature: None,
                    },
                    "tool_use" => AnthropicBlock::Tool {
                        call_id: provider_call_id(string_field(&block_value, "id")?)?,
                        capability_id: string_field(&block_value, "name")?.to_owned(),
                    },
                    other => return Err(format!("unsupported Anthropic block type {other}")),
                };
                match &block {
                    AnthropicBlock::Thinking { item_id, .. } => {
                        frames.push(FixtureFrame::ReasoningItemStarted {
                            item_id: item_id.clone(),
                        });
                    }
                    AnthropicBlock::Tool {
                        call_id,
                        capability_id,
                    } => frames.push(FixtureFrame::ToolCallStarted {
                        call_id: call_id.clone(),
                        capability_id: capability_id.clone(),
                    }),
                }
                blocks.insert(index, block);
            }
            "content_block_delta" => {
                let index = index_field(event, "index")?;
                let delta = object_field(event, "delta")?;
                let delta_value = Value::Object(delta.clone());
                let delta_type = string_field(&delta_value, "type")?;
                let Some(block) = blocks.get_mut(&index) else {
                    return Err(format!("Anthropic delta references unknown block {index}"));
                };
                match (block, delta_type) {
                    (
                        AnthropicBlock::Thinking {
                            item_id, summary, ..
                        },
                        "thinking_delta",
                    ) => {
                        let text = string_field(&delta_value, "thinking")?;
                        summary.push_str(text);
                        frames.push(FixtureFrame::ReasoningDelta {
                            item_id: item_id.clone(),
                            segment: ReasoningSegmentKey {
                                kind: ReasoningTextKind::ProviderReasoning,
                                index: 0,
                            },
                            delta: text.into(),
                        });
                    }
                    (AnthropicBlock::Thinking { signature, .. }, "signature_delta") => {
                        if signature.is_some() {
                            return Err(format!("Anthropic block {index} has duplicate signature"));
                        }
                        *signature = Some(string_field(&delta_value, "signature")?.to_owned());
                    }
                    (AnthropicBlock::Tool { call_id, .. }, "input_json_delta") => {
                        frames.push(FixtureFrame::ToolCallArgumentDelta {
                            call_id: call_id.clone(),
                            delta: string_field(&delta_value, "partial_json")?.into(),
                        });
                    }
                    (_, other) => {
                        return Err(format!(
                            "Anthropic delta type {other} does not match block {index}"
                        ));
                    }
                }
            }
            "content_block_stop" => {
                let index = index_field(event, "index")?;
                let Some(block) = blocks.remove(&index) else {
                    return Err(format!("Anthropic stopped unknown block {index}"));
                };
                match block {
                    AnthropicBlock::Thinking {
                        item_id,
                        summary,
                        signature,
                    } => {
                        let signature = signature.ok_or_else(|| {
                            format!("Anthropic thinking block {index} has no signature")
                        })?;
                        let mut segments = Vec::new();
                        if !summary.is_empty() {
                            segments.push(ReasoningSegment {
                                key: ReasoningSegmentKey {
                                    kind: ReasoningTextKind::ProviderReasoning,
                                    index: 0,
                                },
                                text: summary,
                            });
                        }
                        frames.push(FixtureFrame::ReasoningItemReady {
                            item: reasoning_item(
                                item_id,
                                segments,
                                Some(reasoning_state(
                                    "anthropic",
                                    "messages-thinking-signature-v1",
                                    &signature,
                                )?),
                            ),
                        });
                    }
                    AnthropicBlock::Tool { call_id, .. } => {
                        frames.push(FixtureFrame::ToolCallReady { call_id });
                    }
                }
            }
            "message_delta" => {
                if message_id.is_none() {
                    return Err("Anthropic message_delta preceded message_start".into());
                }
                let delta = object_field(event, "delta")?;
                let delta_value = Value::Object(delta.clone());
                stop_reason = Some(string_field(&delta_value, "stop_reason")?.to_owned());
                let usage = object_field(event, "usage")?;
                let output_tokens =
                    optional_u64(&Value::Object(usage.clone()), "output_tokens")?
                        .ok_or_else(|| "Anthropic message_delta has no output_tokens".to_owned())?;
                let mut details = BTreeMap::new();
                if let Some(cache_creation) = cache_creation_input_tokens {
                    details.insert("cache_creation_input_tokens".into(), cache_creation);
                }
                frames.push(FixtureFrame::UsageUpdate {
                    update: UsageUpdate {
                        semantics: UsageSemantics::Cumulative,
                        usage: TokenUsage {
                            input_tokens,
                            output_tokens: Some(output_tokens),
                            cached_input_tokens,
                            total_tokens: input_tokens.map(|input| input + output_tokens),
                            details,
                            ..TokenUsage::default()
                        },
                    },
                });
                saw_message_delta = true;
            }
            "message_stop" => {
                if message_id.is_none() || !saw_message_delta {
                    return Err("Anthropic message_stop lacked message_start/message_delta".into());
                }
                if !blocks.is_empty() {
                    return Err("Anthropic message_stop had unfinished content blocks".into());
                }
                let finish_reason = match stop_reason.as_deref() {
                    Some("tool_use") => ditto_model::FinishReason::ToolCalls,
                    Some("end_turn") | None => ditto_model::FinishReason::EndTurn,
                    Some(other) => ditto_model::FinishReason::Other(other.to_owned()),
                };
                frames.push(FixtureFrame::Completed {
                    finish_reason,
                    continuation: None,
                });
                completed = true;
            }
            other => frames.push(FixtureFrame::ProviderWarning {
                warning: ProviderWarning {
                    code: Some("unknown_event".into()),
                    message: format!("Anthropic emitted unknown event type {other}"),
                },
            }),
        }
    }
    if !completed {
        return Err("Anthropic fixture did not reach message_stop".into());
    }
    Ok(frames)
}

#[test]
fn provider_shape_mappers_reject_orphaned_openai_argument_deltas() {
    let mut events = openai_responses_shape();
    events[6]["item_id"] = json!("fc_openai_missing_item");

    let error = map_openai_responses(&events).expect_err("orphaned argument delta must fail");
    assert!(error.contains("unknown output item fc_openai_missing_item"));
}

#[test]
fn provider_shape_mappers_reject_unstarted_and_truncated_anthropic_thinking() {
    let mut missing_start = anthropic_messages_shape();
    missing_start.remove(1);
    let error =
        map_anthropic_messages(&missing_start).expect_err("thinking delta needs a block start");
    assert!(error.contains("unknown block 0"));

    let mut truncated = anthropic_messages_shape();
    truncated.remove(5);
    let error = map_anthropic_messages(&truncated).expect_err("thinking block must be stopped");
    assert!(error.contains("unfinished content blocks"));
}

#[tokio::test]
async fn openai_responses_source_shape_preserves_item_call_reasoning_usage_and_continuation() {
    let frames = map_openai_responses(&openai_responses_shape()).expect("map OpenAI shape");
    let driver = FixtureDriver::new(
        DriverId::new("openai-responses-shape").expect("driver id"),
        frames,
    )
    .expect("valid OpenAI fixture");
    let request = valid_request([
        ModelFeature::ToolCalls,
        ModelFeature::Usage,
        ModelFeature::Continuation,
        ModelFeature::ReasoningSummary,
        ModelFeature::ReasoningState,
    ]);
    request
        .validate_at(Utc::now())
        .expect("valid request/config");
    assert_eq!(driver.descriptor().request_capabilities, Default::default());
    assert_eq!(
        driver.descriptor().emitted_features,
        [
            ModelFeature::ToolCalls,
            ModelFeature::Usage,
            ModelFeature::Continuation,
            ModelFeature::ReasoningSummary,
            ModelFeature::ReasoningState,
        ]
        .into_iter()
        .collect()
    );

    let events = collect(&driver, request).await;
    assert_contiguous_valid_events(&events);
    let ready_reasoning = events.iter().find_map(|event| match &event.event {
        ModelEvent::ReasoningItemReady { item } => Some(item),
        _ => None,
    });
    let ready_reasoning = ready_reasoning.expect("OpenAI reasoning item ready");
    assert_eq!(ready_reasoning.id.as_str(), "rs_openai_shape_1");
    assert_eq!(ready_reasoning.segments.len(), 1);
    assert_eq!(
        ready_reasoning.segments[0].key.kind,
        ReasoningTextKind::Summary
    );
    assert_eq!(
        ready_reasoning.segments[0].text,
        "Inspect the artifact before reading it."
    );
    let state = ready_reasoning.state.as_ref().expect("encrypted state");
    assert_eq!(state.provider(), "openai");
    assert_eq!(state.format(), "responses-reasoning-encrypted-v1");
    assert_eq!(state.data(), "enc-openai-reasoning-v1");

    assert!(events.iter().any(|event| {
        matches!(
            &event.event,
            ModelEvent::ToolCallReady {
                call_id,
                capability_id,
                arguments,
            } if call_id.as_str() == "call_openai_shape_1"
                && call_id.as_str() != "fc_openai_item_1"
                && capability_id == TOOL_ID
                && arguments["reference"] == "sha256:openai-artifact"
        )
    }));
    assert!(events.iter().any(|event| {
        matches!(
            &event.event,
            ModelEvent::UsageUpdate { update }
                if update.semantics == UsageSemantics::Cumulative
                    && update.usage.input_tokens == Some(24)
                    && update.usage.output_tokens == Some(14)
                    && update.usage.cached_input_tokens == Some(8)
                    && update.usage.reasoning_tokens == Some(4)
                    && update.usage.details["cache_write_tokens"] == 2
        )
    }));
    assert!(matches!(
        events.last().map(|event| &event.event),
        Some(ModelEvent::Completed {
            finish_reason: ditto_model::FinishReason::ToolCalls,
            continuation: Some(state),
        }) if state.provider() == "openai"
            && state.format() == "responses-v1"
            && state.state().value()["response_id"] == "resp_openai_shape_1"
    ));
}

#[tokio::test]
async fn anthropic_messages_source_shape_preserves_indexed_blocks_thinking_signature_partial_json_usage_warning_and_stop()
 {
    let frames = map_anthropic_messages(&anthropic_messages_shape()).expect("map Anthropic shape");
    let driver = FixtureDriver::new(
        DriverId::new("anthropic-messages-shape").expect("driver id"),
        frames,
    )
    .expect("valid Anthropic fixture");
    let request = valid_request([
        ModelFeature::ToolCalls,
        ModelFeature::Usage,
        ModelFeature::ProviderWarnings,
        ModelFeature::ReasoningContent,
        ModelFeature::ReasoningState,
    ]);
    request
        .validate_at(Utc::now())
        .expect("valid request/config");
    assert_eq!(
        driver.descriptor().emitted_features,
        [
            ModelFeature::ToolCalls,
            ModelFeature::Usage,
            ModelFeature::ProviderWarnings,
            ModelFeature::ReasoningContent,
            ModelFeature::ReasoningState,
        ]
        .into_iter()
        .collect()
    );

    let events = collect(&driver, request).await;
    assert_contiguous_valid_events(&events);
    let ready_reasoning = events.iter().find_map(|event| match &event.event {
        ModelEvent::ReasoningItemReady { item } => Some(item),
        _ => None,
    });
    let ready_reasoning = ready_reasoning.expect("Anthropic thinking item ready");
    assert_eq!(ready_reasoning.id.as_str(), "anthropic-thinking-0");
    assert_eq!(
        ready_reasoning.segments[0].key.kind,
        ReasoningTextKind::ProviderReasoning
    );
    assert_eq!(ready_reasoning.segments[0].key.index, 0);
    assert_eq!(ready_reasoning.segments[0].text, "Check the artifact.");
    let state = ready_reasoning.state.as_ref().expect("thinking signature");
    assert_eq!(state.provider(), "anthropic");
    assert_eq!(state.format(), "messages-thinking-signature-v1");
    assert_eq!(state.data(), "sig-anthropic-thinking-v1");

    assert!(events.iter().any(|event| {
        matches!(
            &event.event,
            ModelEvent::ToolCallReady {
                call_id,
                arguments,
                ..
            } if call_id.as_str() == "toolu_anthropic_shape_1"
                && arguments["reference"] == "sha256:anthropic-artifact"
        )
    }));
    assert!(events.iter().any(|event| {
        matches!(
            &event.event,
            ModelEvent::ProviderWarning { warning }
                if warning.code.as_deref() == Some("unknown_event")
                    && warning.message.contains("ping")
        )
    }));
    assert!(events.iter().any(|event| {
        matches!(
            &event.event,
            ModelEvent::UsageUpdate { update }
                if update.semantics == UsageSemantics::Cumulative
                    && update.usage.input_tokens == Some(27)
                    && update.usage.output_tokens == Some(11)
                    && update.usage.cached_input_tokens == Some(4)
                    && update.usage.details["cache_creation_input_tokens"] == 2
        )
    }));
    assert!(matches!(
        events.last().map(|event| &event.event),
        Some(ModelEvent::Completed {
            finish_reason: ditto_model::FinishReason::ToolCalls,
            continuation: None,
        })
    ));
}
