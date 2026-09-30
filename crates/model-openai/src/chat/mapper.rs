//! Map `chat.completion.chunk` SSE events onto the validated model stream.
use std::collections::BTreeMap;

use ditto_model::{
    FailureKind, FinishReason, ModelEvent, ModelFailure, ProviderCallId, TokenUsage,
    UsageSemantics, UsageUpdate,
};
use serde_json::Value;

use crate::{
    MAX_PROVIDER_CODE_BYTES, MAX_PROVIDER_MESSAGE_BYTES,
    driver::EventMapper,
    sse::SseEvent,
    transport::{bounded_string, sanitize_credential_tokens},
};

/// Tool calls accepted in one response; the turn loop executes at most one.
const MAX_TOOL_CALLS: usize = 16;

struct PendingCall {
    call_id: ProviderCallId,
    capability_id: String,
    arguments: String,
}

/// Stateful mapper for one streamed chat completion. Only choice 0 is read.
pub(crate) struct ChatStreamMapper {
    reverse_names: BTreeMap<String, String>,
    request_id: String,
    calls: BTreeMap<u64, PendingCall>,
    latest: Option<u64>,
    finish: Option<FinishReason>,
    terminal: bool,
}

impl ChatStreamMapper {
    pub(crate) fn new(reverse_names: BTreeMap<String, String>, request_id: &str) -> Self {
        Self {
            reverse_names,
            request_id: request_id.to_owned(),
            calls: BTreeMap::new(),
            latest: None,
            finish: None,
            terminal: false,
        }
    }

    fn chunk(&mut self, data: &str) -> Result<Vec<ModelEvent>, ModelFailure> {
        if data == "[DONE]" {
            return self.complete();
        }
        let chunk: Value = serde_json::from_str(data)
            .map_err(|error| protocol(format!("chat stream chunk is not JSON: {error}")))?;
        if let Some(error) = chunk.get("error") {
            return Err(provider_failure(error));
        }
        let mut events = Vec::new();
        for choice in chunk
            .get("choices")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|choice| choice.get("index").and_then(Value::as_u64).unwrap_or(0) == 0)
        {
            if self.finish.is_none()
                && let Some(delta) = choice.get("delta")
            {
                if let Some(text) = delta.get("content").and_then(Value::as_str)
                    && !text.is_empty()
                {
                    events.push(ModelEvent::TextDelta { text: text.into() });
                }
                for call in delta
                    .get("tool_calls")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    self.tool_call(call, &mut events)?;
                }
            }
            if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
                self.finish_choice(reason, &mut events)?;
            }
        }
        if let Some(usage) = chunk.get("usage").filter(|usage| usage.is_object())
            && let Some(update) = usage_update(usage)
        {
            events.push(ModelEvent::UsageUpdate { update });
        }
        Ok(events)
    }

    fn tool_call(
        &mut self,
        call: &Value,
        events: &mut Vec<ModelEvent>,
    ) -> Result<(), ModelFailure> {
        let index = self.call_index(call);
        let function = call.get("function");
        let arguments = match function.and_then(|function| function.get("arguments")) {
            Some(Value::String(text)) => text.clone(),
            // Some servers send a parsed object instead of the JSON string.
            Some(value @ Value::Object(_)) => value.to_string(),
            _ => String::new(),
        };
        if let Some(pending) = self.calls.get_mut(&index) {
            if !arguments.is_empty() {
                pending.arguments.push_str(&arguments);
                events.push(ModelEvent::ToolCallArgumentDelta {
                    call_id: pending.call_id.clone(),
                    delta: arguments,
                });
            }
            return Ok(());
        }
        if self.calls.len() == MAX_TOOL_CALLS {
            return Err(protocol(format!(
                "chat response started more than {MAX_TOOL_CALLS} tool calls"
            )));
        }
        let name = function
            .and_then(|function| function.get("name"))
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty())
            .ok_or_else(|| protocol("chat tool call started without a function name"))?;
        // Unknown names pass through; the turn loop rejects unpaged capabilities.
        let capability_id = self
            .reverse_names
            .get(name)
            .cloned()
            .unwrap_or_else(|| name.to_owned());
        // Servers that omit call IDs get request-scoped ones, unique per epoch.
        let id = call
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .map_or_else(
                || format!("{}-call-{index}", self.request_id),
                str::to_owned,
            );
        let call_id = ProviderCallId::new(id)
            .map_err(|error| protocol(format!("chat tool call ID is invalid: {error}")))?;
        events.push(ModelEvent::ToolCallStarted {
            call_id: call_id.clone(),
            capability_id: capability_id.clone(),
        });
        if !arguments.is_empty() {
            events.push(ModelEvent::ToolCallArgumentDelta {
                call_id: call_id.clone(),
                delta: arguments.clone(),
            });
        }
        self.calls.insert(
            index,
            PendingCall {
                call_id,
                capability_id,
                arguments,
            },
        );
        self.latest = Some(index);
        Ok(())
    }

    /// Servers differ on `index`: use it when present, otherwise match a known
    /// call ID, continue the latest call for a nameless fragment and give
    /// anything else the next free index.
    fn call_index(&self, call: &Value) -> u64 {
        if let Some(index) = call.get("index").and_then(Value::as_u64) {
            return index;
        }
        let id = call.get("id").and_then(Value::as_str);
        if let Some((index, _)) = self
            .calls
            .iter()
            .find(|(_, pending)| id == Some(pending.call_id.as_str()))
        {
            return *index;
        }
        let named = call
            .pointer("/function/name")
            .and_then(Value::as_str)
            .is_some_and(|name| !name.is_empty());
        match self.latest {
            Some(latest) if !named => latest,
            _ => self.calls.keys().next_back().map_or(0, |last| last + 1),
        }
    }

    fn finish_choice(
        &mut self,
        reason: &str,
        events: &mut Vec<ModelEvent>,
    ) -> Result<(), ModelFailure> {
        if self.finish.is_some() {
            return Ok(());
        }
        // Several servers report `stop` even when the response is tool calls.
        let finish = if self.calls.is_empty() {
            match reason {
                "stop" => FinishReason::EndTurn,
                "length" => FinishReason::MaxOutputTokens,
                "tool_calls" | "function_call" => FinishReason::ToolCalls,
                "content_filter" => FinishReason::ContentFilter,
                other => FinishReason::Other(bounded_string(other, MAX_PROVIDER_CODE_BYTES)),
            }
        } else {
            FinishReason::ToolCalls
        };
        for pending in self.calls.values_mut() {
            if pending.arguments.trim().is_empty() {
                pending.arguments = "{}".into();
                events.push(ModelEvent::ToolCallArgumentDelta {
                    call_id: pending.call_id.clone(),
                    delta: "{}".into(),
                });
            }
            let arguments = serde_json::from_str(&pending.arguments).map_err(|error| {
                let mut failure = ModelFailure::new(
                    FailureKind::MalformedToolArguments,
                    format!("tool call arguments are not JSON: {error}"),
                );
                failure.call_id = Some(pending.call_id.clone());
                failure
            })?;
            events.push(ModelEvent::ToolCallReady {
                call_id: pending.call_id.clone(),
                capability_id: pending.capability_id.clone(),
                arguments,
            });
        }
        self.finish = Some(finish);
        Ok(())
    }

    /// `[DONE]` or end of stream: the finish reason becomes the terminal, so a
    /// trailing usage chunk still precedes it.
    fn complete(&mut self) -> Result<Vec<ModelEvent>, ModelFailure> {
        let finish_reason = self
            .finish
            .take()
            .ok_or_else(|| protocol("chat stream ended without a finish reason"))?;
        self.terminal = true;
        Ok(vec![ModelEvent::Completed {
            finish_reason,
            continuation: None,
        }])
    }
}

impl EventMapper for ChatStreamMapper {
    fn map(&mut self, event: SseEvent) -> Result<Vec<ModelEvent>, ModelFailure> {
        if self.terminal {
            return Ok(Vec::new());
        }
        self.chunk(event.data.trim())
    }

    fn finish(&mut self) -> Vec<ModelEvent> {
        if self.terminal {
            return Vec::new();
        }
        self.complete()
            .unwrap_or_else(|failure| vec![ModelEvent::Failed { failure }])
    }
}

fn usage_update(usage: &Value) -> Option<UsageUpdate> {
    let count = |value: Option<&Value>| value.and_then(Value::as_u64);
    let usage = TokenUsage {
        input_tokens: count(usage.get("prompt_tokens")),
        output_tokens: count(usage.get("completion_tokens")),
        cached_input_tokens: count(usage.pointer("/prompt_tokens_details/cached_tokens")),
        reasoning_tokens: count(usage.pointer("/completion_tokens_details/reasoning_tokens")),
        total_tokens: count(usage.get("total_tokens")),
        details: BTreeMap::new(),
    };
    usage.validate().ok()?;
    Some(UsageUpdate {
        semantics: UsageSemantics::Cumulative,
        usage,
    })
}

fn provider_failure(error: &Value) -> ModelFailure {
    let message = error
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("chat provider reported an error");
    let mut failure = ModelFailure::new(
        FailureKind::Provider,
        bounded_string(
            &sanitize_credential_tokens(message),
            MAX_PROVIDER_MESSAGE_BYTES,
        ),
    );
    failure.provider_code = error
        .get("code")
        .map(|code| match code {
            Value::String(code) => code.clone(),
            other => other.to_string(),
        })
        .map(|code| bounded_string(&code, MAX_PROVIDER_CODE_BYTES));
    failure
}

fn protocol(message: impl Into<String>) -> ModelFailure {
    ModelFailure::new(FailureKind::Protocol, message)
}
