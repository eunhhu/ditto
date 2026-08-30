use std::collections::BTreeMap;

use ditto_model::{
    ContentPart, ConversationItem, MAX_IDENTIFIER_BYTES, MessageRole, ModelRequest,
    OutputConstraint, ParallelToolCalls, PromptCachePolicy, ToolChoice,
};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    MAX_COMPILED_REQUEST_BYTES, OPENAI_GPT_5_6_MODEL, OpenAiHttpRequest, OpenAiStoragePolicy,
};

const MAX_PROVIDER_NAME_BYTES: usize = 64;
const CONTEXT_PREFIX: &str = "DITTO_CONTEXT_V1\n";
const CONTENT_PREFIX: &str = "DITTO_CONTENT_V1\n";

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub(crate) enum CompileError {
    #[error("model request is invalid: {0}")]
    InvalidRequest(String),
    #[error("request cannot be projected to OpenAI Responses: {0}")]
    Unsupported(String),
    #[error("compiled OpenAI request is {actual} bytes, exceeding the {maximum}-byte limit")]
    RequestTooLarge { actual: usize, maximum: usize },
    #[error("failed to serialize compiled OpenAI request: {0}")]
    Serialization(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OutputMode {
    Text,
    Structured,
}

#[derive(Debug)]
pub(crate) struct CompiledRequest {
    pub http: OpenAiHttpRequest,
    pub reverse_names: BTreeMap<String, String>,
    pub output_mode: OutputMode,
    pub previous_response_id: Option<String>,
}

pub(crate) fn compile_request(
    request: &ModelRequest,
    storage: OpenAiStoragePolicy,
) -> Result<CompiledRequest, CompileError> {
    request
        .validate()
        .map_err(|error| CompileError::InvalidRequest(error.to_string()))?;

    if request.continuation.is_some()
        && request.turn.conversation.iter().any(|item| {
            matches!(
                item,
                ConversationItem::ToolCall { .. }
                    | ConversationItem::ToolResult { .. }
                    | ConversationItem::Reasoning { .. }
            )
        })
    {
        return Err(CompileError::Unsupported(
            "response-ID continuation suffixes may contain only conversation messages".into(),
        ));
    }

    if request
        .turn
        .conversation
        .iter()
        .any(|item| matches!(item, ConversationItem::Reasoning { .. }))
    {
        return Err(CompileError::Unsupported(
            "reasoning-item replay is not implemented by this profile".into(),
        ));
    }

    let mut body = Map::new();
    body.insert("model".into(), Value::String(OPENAI_GPT_5_6_MODEL.into()));
    body.insert("store".into(), Value::Bool(storage.stores_responses()));
    body.insert("stream".into(), Value::Bool(true));

    if !request.stable_system_prefix.segments.is_empty() {
        body.insert(
            "instructions".into(),
            Value::String(request.stable_system_prefix.segments.join("\n\n")),
        );
    }

    let mut input = Vec::new();
    if !request.turn.context.nodes.is_empty() {
        let context = serde_json::to_string(&request.turn.context)
            .map_err(|error| CompileError::Serialization(error.to_string()))?;
        input.push(json!({
            "role": "developer",
            "content": format!("{CONTEXT_PREFIX}{context}")
        }));
    }
    for item in &request.turn.conversation {
        input.push(compile_conversation_item(item)?);
    }
    if !input.is_empty() {
        body.insert("input".into(), Value::Array(input));
    }

    let (tools, reverse_names) = compile_tools(request)?;
    if !tools.is_empty() {
        body.insert("tools".into(), Value::Array(tools));
    }

    match &request.generation.prompt_cache {
        PromptCachePolicy::ProviderDefault => {}
        PromptCachePolicy::Disabled => {
            body.insert("prompt_cache_options".into(), json!({"mode":"explicit"}));
        }
        PromptCachePolicy::Automatic { namespace } => {
            body.insert("prompt_cache_options".into(), json!({"mode":"implicit"}));
            if let Some(namespace) = namespace {
                body.insert(
                    "prompt_cache_key".into(),
                    Value::String(namespace.as_str().into()),
                );
            }
        }
        PromptCachePolicy::StablePrefix { .. } => {
            return Err(CompileError::Unsupported(
                "explicit stable-prefix cache breakpoints are not implemented".into(),
            ));
        }
    }

    match &request.generation.tool_use.choice {
        ToolChoice::ProviderDefault => {}
        ToolChoice::None => {
            body.insert("tool_choice".into(), Value::String("none".into()));
        }
        ToolChoice::Auto => {
            body.insert("tool_choice".into(), Value::String("auto".into()));
        }
        ToolChoice::Required => {
            body.insert("tool_choice".into(), Value::String("required".into()));
        }
        ToolChoice::Specific { capability_id } => {
            let provider_name = reverse_names
                .iter()
                .find_map(|(provider_name, original)| {
                    (original == capability_id).then_some(provider_name)
                })
                .ok_or_else(|| {
                    CompileError::Unsupported(format!(
                        "specific tool choice references unmapped capability {capability_id}"
                    ))
                })?;
            body.insert(
                "tool_choice".into(),
                json!({"type":"function","name":provider_name}),
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

    let output_mode = match &request.turn.output {
        OutputConstraint::Text => OutputMode::Text,
        OutputConstraint::Structured {
            name,
            schema,
            strict,
        } => {
            if !schema.is_object() {
                return Err(CompileError::Unsupported(
                    "OpenAI structured output schemas must be JSON Schema objects".into(),
                ));
            }
            body.insert(
                "text".into(),
                json!({
                    "format": {
                        "type": "json_schema",
                        "name": provider_name(name),
                        "schema": canonical_json_value(schema),
                        "strict": strict
                    }
                }),
            );
            OutputMode::Structured
        }
    };

    let mut previous_response_id = None;
    if let Some(continuation) = &request.continuation {
        if storage != OpenAiStoragePolicy::ProviderManaged {
            return Err(CompileError::Unsupported(
                "ephemeral mode cannot consume provider-managed continuation state".into(),
            ));
        }
        let state = continuation
            .state()
            .value()
            .as_object()
            .filter(|state| state.len() == 1)
            .ok_or_else(|| {
                CompileError::Unsupported(
                    "OpenAI continuation state must be exactly {response_id}".into(),
                )
            })?;
        let response_id = state
            .get("response_id")
            .and_then(Value::as_str)
            .filter(|value| valid_provider_identifier(value))
            .ok_or_else(|| {
                CompileError::Unsupported(
                    "OpenAI continuation response_id is absent or invalid".into(),
                )
            })?;
        body.insert(
            "previous_response_id".into(),
            Value::String(response_id.into()),
        );
        previous_response_id = Some(response_id.into());
    }

    let bytes = serde_json::to_vec(&Value::Object(body))
        .map_err(|error| CompileError::Serialization(error.to_string()))?;
    if bytes.len() > MAX_COMPILED_REQUEST_BYTES {
        return Err(CompileError::RequestTooLarge {
            actual: bytes.len(),
            maximum: MAX_COMPILED_REQUEST_BYTES,
        });
    }

    Ok(CompiledRequest {
        http: OpenAiHttpRequest::new(bytes),
        reverse_names,
        output_mode,
        previous_response_id,
    })
}

fn compile_conversation_item(item: &ConversationItem) -> Result<Value, CompileError> {
    match item {
        ConversationItem::Message { role, content } => Ok(json!({
            "role": match role {
                MessageRole::User => "user",
                MessageRole::Assistant => "assistant",
            },
            "content": render_content(content)?
        })),
        ConversationItem::ToolCall {
            call_id,
            capability_id,
            arguments,
        } => Ok(json!({
            "type": "function_call",
            "call_id": call_id.as_str(),
            "name": provider_name(capability_id),
            "arguments": serde_json::to_string(&canonical_json_value(arguments))
                .map_err(|error| CompileError::Serialization(error.to_string()))?
        })),
        ConversationItem::ToolResult {
            call_id,
            content,
            is_error,
        } => Ok(json!({
            "type": "function_call_output",
            "call_id": call_id.as_str(),
            "output": render_content(content)?,
            "status": if *is_error { "incomplete" } else { "completed" }
        })),
        ConversationItem::Reasoning { .. } => Err(CompileError::Unsupported(
            "reasoning-item replay is not implemented by this profile".into(),
        )),
    }
}

fn render_content(content: &[ContentPart]) -> Result<String, CompileError> {
    if let [ContentPart::Text { text }] = content {
        return Ok(text.clone());
    }
    let tagged = content
        .iter()
        .map(|part| match part {
            ContentPart::Text { text } => json!({"type":"text","text":text}),
            ContentPart::Structured { value } => {
                json!({"type":"structured","value":canonical_json_value(value)})
            }
        })
        .collect::<Vec<_>>();
    let encoded = serde_json::to_string(&tagged)
        .map_err(|error| CompileError::Serialization(error.to_string()))?;
    Ok(format!("{CONTENT_PREFIX}{encoded}"))
}

fn compile_tools(
    request: &ModelRequest,
) -> Result<(Vec<Value>, BTreeMap<String, String>), CompileError> {
    let mut tools = Vec::with_capacity(request.tools.len());
    let mut reverse = BTreeMap::new();
    for schema in &request.tools {
        if !schema.input_schema.is_object() || !schema.output_schema.is_object() {
            return Err(CompileError::Unsupported(format!(
                "capability {} uses a boolean schema unsupported by OpenAI function tools",
                schema.id
            )));
        }
        let name = provider_name(&schema.id);
        if let Some(existing) = reverse.insert(name.clone(), schema.id.clone()) {
            return Err(CompileError::Unsupported(format!(
                "capabilities {existing} and {} collide as OpenAI name {name}",
                schema.id
            )));
        }
        tools.push(json!({
            "type": "function",
            "name": name,
            "description": schema.summary,
            "parameters": canonical_json_value(&schema.input_schema),
            "output_schema": canonical_json_value(&schema.output_schema),
            "strict": false
        }));
    }
    Ok((tools, reverse))
}

pub(crate) fn provider_name(value: &str) -> String {
    if valid_openai_name(value) {
        return value.to_owned();
    }
    let digest = Sha256::digest(value.as_bytes());
    let mut name = String::with_capacity(MAX_PROVIDER_NAME_BYTES);
    name.push_str("d_");
    for byte in digest {
        if name.len() == MAX_PROVIDER_NAME_BYTES {
            break;
        }
        const HEX: &[u8; 16] = b"0123456789abcdef";
        name.push(char::from(HEX[usize::from(byte >> 4)]));
        if name.len() == MAX_PROVIDER_NAME_BYTES {
            break;
        }
        name.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    name
}

fn valid_openai_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_PROVIDER_NAME_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn valid_provider_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_IDENTIFIER_BYTES
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

fn canonical_json_value(value: &Value) -> Value {
    match value {
        Value::Array(values) => Value::Array(values.iter().map(canonical_json_value).collect()),
        Value::Object(values) => {
            let mut keys = values.keys().collect::<Vec<_>>();
            keys.sort_unstable();
            let mut canonical = Map::new();
            for key in keys {
                canonical.insert(key.clone(), canonical_json_value(&values[key]));
            }
            Value::Object(canonical)
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => value.clone(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use ditto_capability::CapabilitySchema;
    use ditto_context::ContextCapsule;
    use ditto_model::{
        ExecutionEpochId, FeatureRequest, GenerationControls, ModelFeature, ModelRequestId,
        ModelTurn, StableSystemPrefix,
    };

    use super::*;

    fn request() -> ModelRequest {
        let mut request = ModelRequest::new(
            ModelRequestId::new("request-1").expect("request id"),
            ExecutionEpochId::new("epoch-1").expect("epoch id"),
            StableSystemPrefix {
                segments: vec!["stable one".into(), "stable two".into()],
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
        request.tools.push(CapabilitySchema {
            id: "artifact.read".into(),
            version: "0.1.0".into(),
            summary: "Read an artifact".into(),
            input_schema: json!({"type":"object"}),
            output_schema: json!({"type":"object"}),
        });
        request.features = FeatureRequest {
            required: BTreeSet::from([ModelFeature::ToolCalls]),
            preferred: BTreeSet::new(),
        };
        request.generation = GenerationControls::default();
        request
    }

    #[test]
    fn deterministic_request_preserves_stable_order_and_full_tool_schemas() {
        let compiled =
            compile_request(&request(), OpenAiStoragePolicy::Ephemeral).expect("compile request");
        let body: Value = serde_json::from_slice(compiled.http.body()).expect("request JSON");
        assert_eq!(body["model"], OPENAI_GPT_5_6_MODEL);
        assert_eq!(body["store"], false);
        assert_eq!(body["stream"], true);
        assert_eq!(body["instructions"], "stable one\n\nstable two");
        assert_eq!(body["input"][0]["role"], "user");
        assert_eq!(body["tools"][0]["parameters"], json!({"type":"object"}));
        assert_eq!(body["tools"][0]["output_schema"], json!({"type":"object"}));
        let mapped = body["tools"][0]["name"].as_str().expect("tool name");
        assert_eq!(compiled.reverse_names[mapped], "artifact.read");

        let again = compile_request(&request(), OpenAiStoragePolicy::Ephemeral)
            .expect("compile identical request");
        assert_eq!(compiled.http.body(), again.http.body());
    }

    #[test]
    fn provider_names_preserve_valid_values_and_hash_other_ids_deterministically() {
        assert_eq!(provider_name("valid_Name-1"), "valid_Name-1");
        let mapped = provider_name("artifact.read");
        assert_eq!(
            mapped,
            "d_d24e7f8240cb6f0ce32385fb09615c711654420079024d334ad08d82751926"
        );
        assert_eq!(mapped.len(), MAX_PROVIDER_NAME_BYTES);
        assert!(valid_openai_name(&mapped));
        assert_ne!(mapped, provider_name("artifact.write"));
    }
}
