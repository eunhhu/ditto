//! Deterministic projection of the provider-neutral request onto the
//! OpenAI-compatible `/chat/completions` wire shape.
use std::collections::BTreeMap;

use ditto_model::{
    ContentPart, ConversationItem, MessageRole, ModelRequest, OutputConstraint, ParallelToolCalls,
    PromptCachePolicy, ToolChoice,
};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::{MAX_COMPILED_REQUEST_BYTES, OpenAiHttpRequest, compile::CompileError};

const CONTEXT_HEADING: &str = "DITTO_CONTEXT_V1";
const CONTENT_PREFIX: &str = "DITTO_CONTENT_V1\n";
const MAX_FUNCTION_NAME_BYTES: usize = 64;

#[derive(Debug)]
pub(crate) struct CompiledChatRequest {
    pub http: OpenAiHttpRequest,
    /// Wire function name to capability ID.
    pub reverse_names: BTreeMap<String, String>,
}

/// Compile one request. Every ordering is deterministic so identical requests
/// produce identical bytes.
pub(crate) fn compile_chat_request(
    request: &ModelRequest,
    model: &str,
    include_usage: bool,
) -> Result<CompiledChatRequest, CompileError> {
    request
        .validate()
        .map_err(|error| CompileError::InvalidRequest(error.to_string()))?;
    if request.continuation.is_some() {
        return Err(CompileError::Unsupported(
            "chat completions have no provider-managed continuation".into(),
        ));
    }
    if !matches!(request.turn.output, OutputConstraint::Text) {
        return Err(CompileError::Unsupported(
            "structured output is not implemented for chat completions".into(),
        ));
    }
    if matches!(
        request.generation.prompt_cache,
        PromptCachePolicy::StablePrefix { .. }
    ) {
        return Err(CompileError::Unsupported(
            "explicit stable-prefix cache breakpoints are not implemented".into(),
        ));
    }

    let (tools, names) = compile_tools(request)?;
    let reverse_names = names
        .iter()
        .map(|(capability, name)| (name.clone(), capability.clone()))
        .collect::<BTreeMap<_, _>>();

    let mut messages = Vec::new();
    let system = system_message(request)?;
    if !system.is_empty() {
        messages.push(json!({"role": "system", "content": system}));
    }
    for item in &request.turn.conversation {
        match item {
            ConversationItem::Message { role, content } => messages.push(json!({
                "role": match role {
                    MessageRole::User => "user",
                    MessageRole::Assistant => "assistant",
                },
                "content": render_content(content)?,
            })),
            ConversationItem::ToolCall {
                call_id,
                capability_id,
                arguments,
            } => {
                let name = names.get(capability_id).ok_or_else(|| {
                    CompileError::Unsupported(format!(
                        "conversation calls unpaged capability {capability_id}"
                    ))
                })?;
                let call = json!({
                    "id": call_id.as_str(),
                    "type": "function",
                    "function": {
                        "name": name,
                        "arguments": serde_json::to_string(&canonical(arguments))
                            .map_err(|error| CompileError::Serialization(error.to_string()))?,
                    },
                });
                // Text streamed before the call, or an earlier call of the same
                // response, shares one assistant message. The current turn
                // always starts with a user message, so history never merges.
                match messages.last_mut() {
                    Some(last) if last["role"] == "assistant" => match last.get_mut("tool_calls") {
                        Some(Value::Array(calls)) => calls.push(call),
                        _ => last["tool_calls"] = json!([call]),
                    },
                    _ => messages.push(json!({
                        "role": "assistant",
                        "content": Value::Null,
                        "tool_calls": [call],
                    })),
                }
            }
            ConversationItem::ToolResult {
                call_id, content, ..
            } => messages.push(json!({
                "role": "tool",
                "tool_call_id": call_id.as_str(),
                "content": render_content(content)?,
            })),
            ConversationItem::Reasoning { .. } => {
                return Err(CompileError::Unsupported(
                    "reasoning-item replay is not implemented for chat completions".into(),
                ));
            }
        }
    }

    let mut body = Map::new();
    body.insert("model".into(), Value::String(model.into()));
    body.insert("stream".into(), Value::Bool(true));
    if include_usage {
        body.insert("stream_options".into(), json!({"include_usage": true}));
    }
    body.insert("messages".into(), Value::Array(messages));
    if !tools.is_empty() {
        body.insert("tools".into(), Value::Array(tools));
        match &request.generation.tool_use.choice {
            ToolChoice::ProviderDefault => {}
            ToolChoice::Auto => {
                body.insert("tool_choice".into(), json!("auto"));
            }
            ToolChoice::None => {
                body.insert("tool_choice".into(), json!("none"));
            }
            ToolChoice::Required => {
                body.insert("tool_choice".into(), json!("required"));
            }
            ToolChoice::Specific { capability_id } => {
                let name = names.get(capability_id).ok_or_else(|| {
                    CompileError::Unsupported(format!(
                        "specific tool choice references unpaged capability {capability_id}"
                    ))
                })?;
                body.insert(
                    "tool_choice".into(),
                    json!({"type": "function", "function": {"name": name}}),
                );
            }
        }
        match request.generation.tool_use.parallel_calls {
            ParallelToolCalls::ProviderDefault => {}
            ParallelToolCalls::Allow => {
                body.insert("parallel_tool_calls".into(), Value::Bool(true));
            }
            ParallelToolCalls::Forbid => {
                body.insert("parallel_tool_calls".into(), Value::Bool(false));
            }
        }
    }

    let bytes = serde_json::to_vec(&Value::Object(body))
        .map_err(|error| CompileError::Serialization(error.to_string()))?;
    if bytes.len() > MAX_COMPILED_REQUEST_BYTES {
        return Err(CompileError::RequestTooLarge {
            actual: bytes.len(),
            maximum: MAX_COMPILED_REQUEST_BYTES,
        });
    }
    Ok(CompiledChatRequest {
        http: OpenAiHttpRequest::new(bytes),
        reverse_names,
    })
}

/// One system message: many chat templates reject a second system message.
fn system_message(request: &ModelRequest) -> Result<String, CompileError> {
    let mut parts = request.stable_system_prefix.segments.clone();
    if !request.turn.context.nodes.is_empty() {
        let context = serde_json::to_string(&request.turn.context)
            .map_err(|error| CompileError::Serialization(error.to_string()))?;
        parts.push(format!("{CONTEXT_HEADING}\n{context}"));
    }
    Ok(parts.join("\n\n"))
}

fn render_content(content: &[ContentPart]) -> Result<String, CompileError> {
    if let [ContentPart::Text { text }] = content {
        return Ok(text.clone());
    }
    let tagged = content
        .iter()
        .map(|part| match part {
            ContentPart::Text { text } => json!({"type": "text", "text": text}),
            ContentPart::Structured { value } => {
                json!({"type": "structured", "value": canonical(value)})
            }
        })
        .collect::<Vec<_>>();
    serde_json::to_string(&tagged)
        .map(|encoded| format!("{CONTENT_PREFIX}{encoded}"))
        .map_err(|error| CompileError::Serialization(error.to_string()))
}

/// Capability ID to readable function name: invalid characters become `_`
/// (`artifact.read` → `artifact_read`); a collision or an overlong ID falls
/// back to a digest name so the mapping stays a bijection.
fn compile_tools(
    request: &ModelRequest,
) -> Result<(Vec<Value>, BTreeMap<String, String>), CompileError> {
    let mut names = BTreeMap::new();
    let mut taken = BTreeMap::new();
    let mut tools = Vec::with_capacity(request.tools.len());
    for schema in &request.tools {
        if !schema.input_schema.is_object() {
            return Err(CompileError::Unsupported(format!(
                "capability {} uses a boolean input schema",
                schema.id
            )));
        }
        let mut name = readable_name(&schema.id);
        if taken.contains_key(&name) {
            name = digest_name(&schema.id);
        }
        if let Some(existing) = taken.insert(name.clone(), schema.id.clone()) {
            return Err(CompileError::Unsupported(format!(
                "capabilities {existing} and {} collide as function {name}",
                schema.id
            )));
        }
        names.insert(schema.id.clone(), name.clone());
        tools.push(json!({
            "type": "function",
            "function": {
                "name": name,
                "description": schema.summary,
                "parameters": canonical(&schema.input_schema),
            },
        }));
    }
    Ok((tools, names))
}

fn readable_name(id: &str) -> String {
    let name = id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect::<String>();
    if name.is_empty() || name.len() > MAX_FUNCTION_NAME_BYTES {
        digest_name(id)
    } else {
        name
    }
}

fn digest_name(id: &str) -> String {
    let digest = Sha256::digest(id.as_bytes());
    let mut name = String::from("d_");
    for byte in digest.iter().take(16) {
        name.push_str(&format!("{byte:02x}"));
    }
    name
}

fn canonical(value: &Value) -> Value {
    match value {
        Value::Array(values) => Value::Array(values.iter().map(canonical).collect()),
        Value::Object(values) => {
            let mut keys = values.keys().collect::<Vec<_>>();
            keys.sort_unstable();
            Value::Object(
                keys.into_iter()
                    .map(|key| (key.clone(), canonical(&values[key])))
                    .collect(),
            )
        }
        _ => value.clone(),
    }
}
