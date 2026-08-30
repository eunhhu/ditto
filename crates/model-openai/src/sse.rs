use std::collections::{BTreeMap, BTreeSet};

use ditto_model::{
    ContinuationState, FailureKind, FinishReason, MAX_IDENTIFIER_BYTES, MAX_TOOL_ARGUMENT_BYTES,
    ModelEvent, ModelFailure, ProviderCallId, TokenUsage, UsageSemantics, UsageUpdate,
};
use serde_json::{Map, Value};

use crate::{
    MAX_ACTIVE_OUTPUT_ITEMS, MAX_PROVIDER_CODE_BYTES, MAX_PROVIDER_MESSAGE_BYTES,
    MAX_SEEN_OUTPUT_ITEMS, MAX_SSE_BUFFER_BYTES, MAX_SSE_EVENT_BYTES, OPENAI_CONTINUATION_FORMAT,
    OPENAI_GPT_5_6_MODEL, OPENAI_PROVIDER, OpenAiStoragePolicy,
    compile::OutputMode,
    transport::{bounded_string, sanitize_credential_tokens},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SseEvent {
    pub event: Option<String>,
    pub data: String,
}

#[derive(Debug, Default)]
pub(crate) struct SseDecoder {
    line: Vec<u8>,
    event_name: Option<String>,
    data_lines: Vec<String>,
    event_bytes: usize,
}

impl SseDecoder {
    pub fn push_preserving_prefix(
        &mut self,
        chunk: &[u8],
    ) -> (Vec<SseEvent>, Option<ModelFailure>) {
        let mut events = Vec::new();
        for &byte in chunk {
            if byte == b'\n' {
                let mut line = std::mem::take(&mut self.line);
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                if let Err(failure) = self.accept_line(&line, &mut events) {
                    return (events, Some(failure));
                }
            } else {
                self.line.push(byte);
                if self.line.len() > MAX_SSE_BUFFER_BYTES {
                    return (
                        events,
                        Some(protocol_failure(
                            "OpenAI SSE stream exceeded the unterminated-buffer limit",
                        )),
                    );
                }
            }
        }
        (events, None)
    }

    pub fn finish_preserving_prefix(&mut self) -> (Vec<SseEvent>, Option<ModelFailure>) {
        let mut events = Vec::new();
        if !self.line.is_empty() {
            let mut line = std::mem::take(&mut self.line);
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            if let Err(failure) = self.accept_line(&line, &mut events) {
                return (events, Some(failure));
            }
        }
        self.dispatch(&mut events);
        (events, None)
    }

    fn accept_line(&mut self, line: &[u8], events: &mut Vec<SseEvent>) -> Result<(), ModelFailure> {
        self.event_bytes = self
            .event_bytes
            .saturating_add(line.len().saturating_add(1));
        if self.event_bytes > MAX_SSE_EVENT_BYTES {
            return Err(protocol_failure(
                "OpenAI SSE event exceeded the configured byte limit",
            ));
        }
        let line = std::str::from_utf8(line)
            .map_err(|_| protocol_failure("OpenAI SSE stream contained invalid UTF-8"))?;
        if line.is_empty() {
            self.dispatch(events);
            return Ok(());
        }
        if line.starts_with(':') {
            return Ok(());
        }
        let (field, value) = line.split_once(':').map_or((line, ""), |(field, value)| {
            (field, value.strip_prefix(' ').unwrap_or(value))
        });
        match field {
            "event" => self.event_name = Some(value.to_owned()),
            "data" => self.data_lines.push(value.to_owned()),
            // `id`, `retry`, and future fields do not carry Responses semantics.
            _ => {}
        }
        Ok(())
    }

    fn dispatch(&mut self, events: &mut Vec<SseEvent>) {
        if !self.data_lines.is_empty() {
            events.push(SseEvent {
                event: self.event_name.take(),
                data: self.data_lines.join("\n"),
            });
        } else {
            self.event_name = None;
        }
        self.data_lines.clear();
        self.event_bytes = 0;
    }
}

#[derive(Debug)]
enum OutputItemKind {
    Message,
    Function(FunctionItem),
    Other(String),
}

#[derive(Debug)]
struct OutputItem {
    output_index: u64,
    kind: OutputItemKind,
}

#[derive(Debug)]
struct FunctionItem {
    call_id: ProviderCallId,
    provider_name: String,
    capability_id: String,
    initial_arguments: String,
    arguments: String,
    raw_delta_count: usize,
    ready: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TextPartKind {
    OutputText,
    Refusal,
}

#[derive(Debug)]
struct TextPart {
    output_index: u64,
    kind: TextPartKind,
    text: String,
    text_done: bool,
    content_done: bool,
}

#[derive(Debug)]
pub(crate) struct ResponseMapper {
    reverse_names: BTreeMap<String, String>,
    output_mode: OutputMode,
    storage: OpenAiStoragePolicy,
    usage_required: bool,
    expected_previous_response_id: Option<String>,
    last_sequence: Option<u64>,
    response_id: Option<String>,
    items: BTreeMap<String, OutputItem>,
    seen_item_ids: BTreeSet<String>,
    seen_call_ids: BTreeSet<ProviderCallId>,
    text_parts: BTreeMap<(String, u64), TextPart>,
    saw_tool_call: bool,
    saw_refusal: bool,
    structured_emitted: bool,
    terminal: bool,
    saw_done_marker: bool,
}

impl ResponseMapper {
    pub fn new(
        reverse_names: BTreeMap<String, String>,
        output_mode: OutputMode,
        storage: OpenAiStoragePolicy,
        usage_required: bool,
        expected_previous_response_id: Option<String>,
    ) -> Self {
        Self {
            reverse_names,
            output_mode,
            storage,
            usage_required,
            expected_previous_response_id,
            last_sequence: None,
            response_id: None,
            items: BTreeMap::new(),
            seen_item_ids: BTreeSet::new(),
            seen_call_ids: BTreeSet::new(),
            text_parts: BTreeMap::new(),
            saw_tool_call: false,
            saw_refusal: false,
            structured_emitted: false,
            terminal: false,
            saw_done_marker: false,
        }
    }

    pub fn map(&mut self, event: SseEvent) -> Result<Vec<ModelEvent>, ModelFailure> {
        if event.data == "[DONE]" {
            self.saw_done_marker = true;
            return Ok(Vec::new());
        }
        if self.terminal {
            return Err(protocol_failure(
                "OpenAI stream contained an event after a semantic terminal",
            ));
        }

        let value: Value = serde_json::from_str(&event.data)
            .map_err(|_| protocol_failure("OpenAI SSE data was not valid JSON"))?;
        let object = value
            .as_object()
            .ok_or_else(|| protocol_failure("OpenAI event JSON was not an object"))?;
        let event_type = required_string(object, "type")?;
        if let Some(header) = event.event.as_deref()
            && header != event_type
        {
            return Err(protocol_failure(format!(
                "OpenAI SSE event header {header:?} did not match JSON type {event_type:?}"
            )));
        }
        let sequence = required_u64(object, "sequence_number")?;
        if self
            .last_sequence
            .is_some_and(|previous| sequence <= previous)
        {
            return Err(protocol_failure(format!(
                "OpenAI sequence number {sequence} was not greater than its predecessor"
            )));
        }
        self.last_sequence = Some(sequence);

        match event_type {
            "response.created" => self.response_created(object),
            "response.in_progress" => {
                self.validate_response_identity_if_present(object, Some("in_progress"))?;
                Ok(Vec::new())
            }
            "response.queued" => {
                self.validate_response_identity_if_present(object, Some("queued"))?;
                Ok(Vec::new())
            }
            "response.output_item.added" => self.output_item_added(object),
            "response.output_item.done" => self.output_item_done(object),
            "response.content_part.added" => self.content_part_added(object),
            "response.content_part.done" => self.content_part_done(object),
            "response.output_text.delta" => {
                self.text_delta(object, TextPartKind::OutputText, "delta")
            }
            "response.refusal.delta" => self.text_delta(object, TextPartKind::Refusal, "delta"),
            "response.output_text.done" => self.text_done(object, TextPartKind::OutputText, "text"),
            "response.refusal.done" => self.text_done(object, TextPartKind::Refusal, "refusal"),
            "response.function_call_arguments.delta" => self.function_arguments_delta(object),
            "response.function_call_arguments.done" => self.function_arguments_done(object),
            "response.completed" => self.terminal_response(object, TerminalKind::Completed),
            "response.incomplete" => self.terminal_response(object, TerminalKind::Incomplete),
            "response.failed" => self.terminal_response(object, TerminalKind::Failed),
            "error" => self.standalone_error(object),
            // These lifecycle frames are informational for the v1 semantic IR.
            "response.output_text.annotation.added"
            | "response.reasoning_summary_part.added"
            | "response.reasoning_summary_part.done"
            | "response.reasoning_summary_text.delta"
            | "response.reasoning_summary_text.done" => Ok(Vec::new()),
            // Unknown future events stay non-semantic after envelope validation.
            _ => Ok(Vec::new()),
        }
    }

    pub fn eof_failure(&self) -> ModelFailure {
        if self.saw_done_marker {
            protocol_failure("OpenAI stream ended after [DONE] without a Responses terminal event")
        } else {
            protocol_failure("OpenAI stream ended without a Responses terminal event")
        }
    }

    pub const fn is_terminal(&self) -> bool {
        self.terminal
    }

    fn response_created(
        &mut self,
        object: &Map<String, Value>,
    ) -> Result<Vec<ModelEvent>, ModelFailure> {
        if self.response_id.is_some() {
            return Err(protocol_failure(
                "OpenAI response.created appeared more than once",
            ));
        }
        let response = required_object(object, "response")?;
        self.validate_response_metadata(response)?;
        if let Some(status) = optional_typed_string(response, "status")?
            && !matches!(status, "queued" | "in_progress")
        {
            return Err(protocol_failure(format!(
                "OpenAI response.created carried unexpected status {status:?}"
            )));
        }
        let id = bounded_identifier(required_string(response, "id")?, "response id")?;
        self.response_id = Some(id);
        Ok(Vec::new())
    }

    fn validate_response_identity_if_present(
        &self,
        object: &Map<String, Value>,
        expected_status: Option<&str>,
    ) -> Result<(), ModelFailure> {
        if let Some(response) = object.get("response").and_then(Value::as_object) {
            self.require_response_id(response)?;
            self.validate_response_metadata(response)?;
            if let Some(status) = optional_typed_string(response, "status")?
                && expected_status.is_some_and(|expected| status != expected)
            {
                return Err(protocol_failure(format!(
                    "OpenAI response lifecycle status {status:?} did not match its event"
                )));
            }
        } else if object.contains_key("response") {
            return Err(protocol_failure(
                "OpenAI lifecycle event response field was not an object",
            ));
        }
        Ok(())
    }

    fn validate_response_metadata(
        &self,
        response: &Map<String, Value>,
    ) -> Result<(), ModelFailure> {
        let object_kind = required_string(response, "object")?;
        if object_kind != "response" {
            return Err(protocol_failure(format!(
                "OpenAI response object kind {object_kind:?} was not response"
            )));
        }
        let model = required_string(response, "model")?;
        if model != OPENAI_GPT_5_6_MODEL {
            return Err(protocol_failure(format!(
                "OpenAI response model {model:?} did not match the closed gpt-5.6 profile"
            )));
        }
        if let Some(value) = response.get("store") {
            let store = value
                .as_bool()
                .ok_or_else(|| protocol_failure("OpenAI response store field was not a boolean"))?;
            if store != self.storage.stores_responses() {
                return Err(protocol_failure(
                    "OpenAI response store policy did not match configured storage",
                ));
            }
        }
        if let Some(value) = response.get("previous_response_id") {
            let actual = match value {
                Value::Null => None,
                Value::String(value) => Some(value.as_str()),
                _ => {
                    return Err(protocol_failure(
                        "OpenAI previous_response_id was neither a string nor null",
                    ));
                }
            };
            if actual != self.expected_previous_response_id.as_deref() {
                return Err(protocol_failure(
                    "OpenAI response previous_response_id did not match the request",
                ));
            }
        }
        Ok(())
    }

    fn output_item_added(
        &mut self,
        object: &Map<String, Value>,
    ) -> Result<Vec<ModelEvent>, ModelFailure> {
        self.require_created()?;
        self.check_active_capacity()?;
        let output_index = required_u64(object, "output_index")?;
        let item = required_object(object, "item")?;
        let item_id = bounded_identifier(required_string(item, "id")?, "output item id")?;
        if self.seen_item_ids.contains(&item_id) {
            return Err(protocol_failure(format!(
                "OpenAI output item {item_id} started more than once"
            )));
        }
        if self.seen_item_ids.len() >= MAX_SEEN_OUTPUT_ITEMS {
            return Err(protocol_failure(
                "OpenAI stream exceeded the total output-item history limit",
            ));
        }
        self.seen_item_ids.insert(item_id.clone());
        let item_type = required_string(item, "type")?;
        let mut events = Vec::new();
        let kind = match item_type {
            "message" => {
                if required_string(item, "role")? != "assistant" {
                    return Err(protocol_failure(
                        "OpenAI output message did not have assistant role",
                    ));
                }
                validate_required_status(item, "in_progress", "added message item")?;
                let initial_content =
                    item.get("content")
                        .and_then(Value::as_array)
                        .ok_or_else(|| {
                            protocol_failure("OpenAI added message item omitted content array")
                        })?;
                if !initial_content.is_empty() {
                    return Err(protocol_failure(
                        "OpenAI added message item contained unstreamed content",
                    ));
                }
                OutputItemKind::Message
            }
            "function_call" => {
                let call_id = provider_call_id(required_string(item, "call_id")?)?;
                if self.seen_call_ids.contains(&call_id) {
                    return Err(protocol_failure(format!(
                        "OpenAI provider call {call_id} started more than once"
                    )));
                }
                if self.seen_call_ids.len() >= MAX_SEEN_OUTPUT_ITEMS {
                    return Err(protocol_failure(
                        "OpenAI stream exceeded the total tool-call history limit",
                    ));
                }
                self.seen_call_ids.insert(call_id.clone());
                validate_required_status(item, "in_progress", "added function item")?;
                let provider_name = required_string(item, "name")?.to_owned();
                if !valid_function_name(&provider_name) {
                    return Err(protocol_failure(
                        "OpenAI function name used an invalid provider format",
                    ));
                }
                let capability_id =
                    self.reverse_names
                        .get(&provider_name)
                        .cloned()
                        .ok_or_else(|| {
                            protocol_failure(format!(
                                "OpenAI called unmapped function {provider_name:?}"
                            ))
                        })?;
                let initial_arguments = optional_string(item, "arguments")
                    .unwrap_or_default()
                    .to_owned();
                if initial_arguments.len() > MAX_TOOL_ARGUMENT_BYTES {
                    return Err(tool_failure(
                        FailureKind::Protocol,
                        &call_id,
                        "OpenAI initial function arguments exceeded the byte limit",
                    ));
                }
                events.push(ModelEvent::ToolCallStarted {
                    call_id: call_id.clone(),
                    capability_id: capability_id.clone(),
                });
                OutputItemKind::Function(FunctionItem {
                    call_id,
                    provider_name,
                    capability_id,
                    initial_arguments,
                    arguments: String::new(),
                    raw_delta_count: 0,
                    ready: false,
                })
            }
            other => {
                if other.is_empty()
                    || other.len() > MAX_PROVIDER_CODE_BYTES
                    || other.chars().any(char::is_control)
                {
                    return Err(protocol_failure(
                        "OpenAI output item type was empty, oversized, or contained controls",
                    ));
                }
                OutputItemKind::Other(other.into())
            }
        };
        self.items
            .insert(item_id, OutputItem { output_index, kind });
        Ok(events)
    }

    fn output_item_done(
        &mut self,
        object: &Map<String, Value>,
    ) -> Result<Vec<ModelEvent>, ModelFailure> {
        let output_index = required_u64(object, "output_index")?;
        let item = required_object(object, "item")?;
        let item_id = required_string(item, "id")?;
        let stored = self.items.get(item_id).ok_or_else(|| {
            protocol_failure(format!("OpenAI completed unknown output item {item_id}"))
        })?;
        if stored.output_index != output_index {
            return Err(protocol_failure(format!(
                "OpenAI output item {item_id} changed output_index"
            )));
        }
        let item_type = required_string(item, "type")?;
        match &stored.kind {
            OutputItemKind::Message => {
                if item_type != "message" || required_string(item, "role")? != "assistant" {
                    return Err(protocol_failure(format!(
                        "OpenAI output item {item_id} changed type"
                    )));
                }
                validate_required_status(item, "completed", "completed message item")?;
                let parts = self
                    .text_parts
                    .iter()
                    .filter(|((part_item_id, _), _)| part_item_id == item_id)
                    .collect::<Vec<_>>();
                if parts.iter().any(|(_, part)| !part.content_done) {
                    return Err(protocol_failure(format!(
                        "OpenAI message item {item_id} completed with active content parts"
                    )));
                }
                let content = item
                    .get("content")
                    .and_then(Value::as_array)
                    .ok_or_else(|| {
                        protocol_failure(format!(
                            "OpenAI message item {item_id} final content was not an array"
                        ))
                    })?;
                if content.len() != parts.len() {
                    return Err(protocol_failure(format!(
                        "OpenAI message item {item_id} final content length disagreed with streamed parts"
                    )));
                }
                for (position, (((_, content_index), part), final_part)) in
                    parts.iter().zip(content).enumerate()
                {
                    if *content_index != position as u64 {
                        return Err(protocol_failure(format!(
                            "OpenAI message item {item_id} final content indexes were not contiguous"
                        )));
                    }
                    let final_part = final_part.as_object().ok_or_else(|| {
                        protocol_failure(format!(
                            "OpenAI message item {item_id} final content part was not an object"
                        ))
                    })?;
                    let (expected_type, field) = match part.kind {
                        TextPartKind::OutputText => ("output_text", "text"),
                        TextPartKind::Refusal => ("refusal", "refusal"),
                    };
                    if required_string(final_part, "type")? != expected_type
                        || required_string(final_part, field)? != part.text
                    {
                        return Err(protocol_failure(format!(
                            "OpenAI message item {item_id} final content disagreed with streamed text"
                        )));
                    }
                }
            }
            OutputItemKind::Function(function) => {
                if item_type != "function_call"
                    || required_string(item, "call_id")? != function.call_id.as_str()
                    || required_string(item, "name")? != function.provider_name
                    || required_string(item, "arguments")? != function.arguments
                {
                    return Err(protocol_failure(format!(
                        "OpenAI function item {item_id} changed identity or arguments"
                    )));
                }
                validate_required_status(item, "completed", "completed function item")?;
                if !function.ready {
                    return Err(protocol_failure(format!(
                        "OpenAI function item {item_id} completed before arguments.done"
                    )));
                }
            }
            OutputItemKind::Other(expected) => {
                if item_type != expected {
                    return Err(protocol_failure(format!(
                        "OpenAI output item {item_id} changed type"
                    )));
                }
            }
        }
        self.items.remove(item_id);
        self.text_parts
            .retain(|(part_item_id, _), _| part_item_id != item_id);
        Ok(Vec::new())
    }

    fn content_part_added(
        &mut self,
        object: &Map<String, Value>,
    ) -> Result<Vec<ModelEvent>, ModelFailure> {
        let item_id = required_string(object, "item_id")?;
        let output_index = required_u64(object, "output_index")?;
        let content_index = required_u64(object, "content_index")?;
        self.require_message_item(item_id, output_index)?;
        let part = required_object(object, "part")?;
        let kind = match required_string(part, "type")? {
            "output_text" => {
                if optional_string(part, "text").is_some_and(|text| !text.is_empty()) {
                    return Err(protocol_failure(
                        "OpenAI output_text part started with unstreamed text",
                    ));
                }
                TextPartKind::OutputText
            }
            "refusal" => {
                if optional_string(part, "refusal").is_some_and(|text| !text.is_empty()) {
                    return Err(protocol_failure(
                        "OpenAI refusal part started with unstreamed text",
                    ));
                }
                TextPartKind::Refusal
            }
            _ => return Ok(Vec::new()),
        };
        self.check_active_capacity()?;
        if self
            .text_parts
            .insert(
                (item_id.into(), content_index),
                TextPart {
                    output_index,
                    kind,
                    text: String::new(),
                    text_done: false,
                    content_done: false,
                },
            )
            .is_some()
        {
            return Err(protocol_failure(format!(
                "OpenAI content part {item_id}/{content_index} started more than once"
            )));
        }
        Ok(Vec::new())
    }

    fn content_part_done(
        &mut self,
        object: &Map<String, Value>,
    ) -> Result<Vec<ModelEvent>, ModelFailure> {
        let item_id = required_string(object, "item_id")?;
        let output_index = required_u64(object, "output_index")?;
        let content_index = required_u64(object, "content_index")?;
        let final_part = required_object(object, "part")?;
        if !matches!(
            required_string(final_part, "type")?,
            "output_text" | "refusal"
        ) {
            return Ok(Vec::new());
        }
        let key = (item_id.to_owned(), content_index);
        let part = self.text_parts.get(&key).ok_or_else(|| {
            protocol_failure(format!(
                "OpenAI completed unknown content part {item_id}/{content_index}"
            ))
        })?;
        if part.output_index != output_index || !part.text_done || part.content_done {
            return Err(protocol_failure(format!(
                "OpenAI content part {item_id}/{content_index} changed index or completed early"
            )));
        }
        let (expected_type, text_field) = match part.kind {
            TextPartKind::OutputText => ("output_text", "text"),
            TextPartKind::Refusal => ("refusal", "refusal"),
        };
        if required_string(final_part, "type")? != expected_type
            || required_string(final_part, text_field)? != part.text
        {
            return Err(protocol_failure(format!(
                "OpenAI final content part {item_id}/{content_index} disagreed with deltas"
            )));
        }
        let part = self.text_parts.get_mut(&key).ok_or_else(|| {
            protocol_failure("OpenAI content part disappeared during finalization")
        })?;
        part.content_done = true;
        Ok(Vec::new())
    }

    fn text_delta(
        &mut self,
        object: &Map<String, Value>,
        kind: TextPartKind,
        field: &str,
    ) -> Result<Vec<ModelEvent>, ModelFailure> {
        let item_id = required_string(object, "item_id")?;
        let output_index = required_u64(object, "output_index")?;
        let content_index = required_u64(object, "content_index")?;
        self.require_message_item(item_id, output_index)?;
        let delta = required_string(object, field)?;
        if delta.is_empty() {
            return Err(protocol_failure("OpenAI emitted an empty text delta"));
        }
        let key = (item_id.to_owned(), content_index);
        if !self.text_parts.contains_key(&key) {
            self.check_active_capacity()?;
            self.text_parts.insert(
                key.clone(),
                TextPart {
                    output_index,
                    kind,
                    text: String::new(),
                    text_done: false,
                    content_done: false,
                },
            );
        }
        let Some(part) = self.text_parts.get_mut(&key) else {
            return Err(protocol_failure(
                "OpenAI text part disappeared during correlation",
            ));
        };
        if part.output_index != output_index || part.kind != kind || part.text_done {
            return Err(protocol_failure(format!(
                "OpenAI text part {item_id}/{content_index} changed identity or emitted after done"
            )));
        }
        if part.text.len().saturating_add(delta.len()) > MAX_SSE_EVENT_BYTES {
            return Err(protocol_failure(
                "OpenAI accumulated text exceeded the byte limit",
            ));
        }
        part.text.push_str(delta);
        if kind == TextPartKind::Refusal {
            self.saw_refusal = true;
        }
        Ok(vec![ModelEvent::TextDelta { text: delta.into() }])
    }

    fn text_done(
        &mut self,
        object: &Map<String, Value>,
        kind: TextPartKind,
        field: &str,
    ) -> Result<Vec<ModelEvent>, ModelFailure> {
        let item_id = required_string(object, "item_id")?;
        let output_index = required_u64(object, "output_index")?;
        let content_index = required_u64(object, "content_index")?;
        let final_text = required_string(object, field)?;
        let key = (item_id.to_owned(), content_index);
        let part = self.text_parts.get_mut(&key).ok_or_else(|| {
            protocol_failure(format!(
                "OpenAI completed unknown text part {item_id}/{content_index}"
            ))
        })?;
        if part.output_index != output_index
            || part.kind != kind
            || part.text_done
            || part.text != final_text
        {
            return Err(protocol_failure(format!(
                "OpenAI final text for {item_id}/{content_index} disagreed with deltas"
            )));
        }
        part.text_done = true;
        if kind == TextPartKind::Refusal {
            self.saw_refusal = true;
            return Ok(Vec::new());
        }
        if self.output_mode == OutputMode::Structured {
            if self.structured_emitted {
                return Err(protocol_failure(
                    "OpenAI emitted more than one structured output text part",
                ));
            }
            let value = serde_json::from_str(final_text)
                .map_err(|_| protocol_failure("OpenAI structured output was not valid JSON"))?;
            self.structured_emitted = true;
            return Ok(vec![ModelEvent::StructuredOutput { value }]);
        }
        Ok(Vec::new())
    }

    fn function_arguments_delta(
        &mut self,
        object: &Map<String, Value>,
    ) -> Result<Vec<ModelEvent>, ModelFailure> {
        let item_id = required_string(object, "item_id")?;
        let output_index = required_u64(object, "output_index")?;
        let delta = required_string(object, "delta")?;
        if delta.is_empty() {
            return Err(protocol_failure(
                "OpenAI emitted an empty function argument delta",
            ));
        }
        let function = self.require_function_item_mut(item_id, output_index)?;
        if function.ready {
            return Err(tool_failure(
                FailureKind::Protocol,
                &function.call_id,
                "OpenAI emitted function arguments after arguments.done",
            ));
        }
        if function.raw_delta_count == 0 && !function.initial_arguments.is_empty() {
            return Err(tool_failure(
                FailureKind::Protocol,
                &function.call_id,
                "OpenAI function item carried initial arguments before raw deltas",
            ));
        }
        if function.arguments.len().saturating_add(delta.len()) > MAX_TOOL_ARGUMENT_BYTES {
            return Err(tool_failure(
                FailureKind::Protocol,
                &function.call_id,
                "OpenAI function arguments exceeded the byte limit",
            ));
        }
        function.arguments.push_str(delta);
        function.raw_delta_count += 1;
        Ok(vec![ModelEvent::ToolCallArgumentDelta {
            call_id: function.call_id.clone(),
            delta: delta.into(),
        }])
    }

    fn function_arguments_done(
        &mut self,
        object: &Map<String, Value>,
    ) -> Result<Vec<ModelEvent>, ModelFailure> {
        let item_id = required_string(object, "item_id")?;
        let output_index = required_u64(object, "output_index")?;
        let final_arguments = required_string(object, "arguments")?;
        let final_name = optional_typed_string(object, "name")?;
        let final_call_id = optional_typed_string(object, "call_id")?;
        let function = self.require_function_item_mut(item_id, output_index)?;
        if final_name.is_some_and(|name| name != function.provider_name)
            || final_call_id.is_some_and(|call_id| call_id != function.call_id.as_str())
        {
            return Err(tool_failure(
                FailureKind::Protocol,
                &function.call_id,
                "OpenAI final function argument metadata changed identity",
            ));
        }
        if function.ready {
            return Err(tool_failure(
                FailureKind::Protocol,
                &function.call_id,
                "OpenAI emitted function arguments.done more than once",
            ));
        }
        let mut events = Vec::new();
        if function.raw_delta_count == 0 {
            if !function.initial_arguments.is_empty()
                && function.initial_arguments != final_arguments
            {
                return Err(tool_failure(
                    FailureKind::Protocol,
                    &function.call_id,
                    "OpenAI final function arguments disagreed with the initial item",
                ));
            }
            if final_arguments.is_empty() || final_arguments.len() > MAX_TOOL_ARGUMENT_BYTES {
                return Err(tool_failure(
                    FailureKind::MalformedToolArguments,
                    &function.call_id,
                    "OpenAI final function arguments were empty or oversized",
                ));
            }
            function.arguments.push_str(final_arguments);
            events.push(ModelEvent::ToolCallArgumentDelta {
                call_id: function.call_id.clone(),
                delta: final_arguments.into(),
            });
        } else if function.arguments != final_arguments {
            return Err(tool_failure(
                FailureKind::Protocol,
                &function.call_id,
                "OpenAI final function arguments disagreed with accumulated deltas",
            ));
        }
        let arguments = serde_json::from_str(&function.arguments).map_err(|_| {
            tool_failure(
                FailureKind::MalformedToolArguments,
                &function.call_id,
                "OpenAI function arguments were malformed JSON",
            )
        })?;
        function.ready = true;
        let call_id = function.call_id.clone();
        let capability_id = function.capability_id.clone();
        self.saw_tool_call = true;
        events.push(ModelEvent::ToolCallReady {
            call_id,
            capability_id,
            arguments,
        });
        Ok(events)
    }

    fn terminal_response(
        &mut self,
        object: &Map<String, Value>,
        kind: TerminalKind,
    ) -> Result<Vec<ModelEvent>, ModelFailure> {
        let response = required_object(object, "response")?;
        let response_id = self.require_response_id(response)?.to_owned();
        self.validate_response_metadata(response)?;
        let status = optional_typed_string(response, "status")?;
        if status == Some("cancelled") {
            self.terminal = true;
            return Ok(vec![ModelEvent::Failed {
                failure: provider_failure(
                    FailureKind::Cancelled,
                    "OpenAI reported a cancelled terminal response",
                    Some("cancelled"),
                ),
            }]);
        }

        let expected_status = match kind {
            TerminalKind::Completed => "completed",
            TerminalKind::Incomplete => "incomplete",
            TerminalKind::Failed => "failed",
        };
        if status.is_some_and(|status| status != expected_status) {
            return Err(protocol_failure(format!(
                "OpenAI {kind:?} envelope carried unexpected response status {status:?}"
            )));
        }

        match kind {
            TerminalKind::Failed => {
                self.terminal = true;
                let error = response
                    .get("error")
                    .and_then(Value::as_object)
                    .ok_or_else(|| protocol_failure("OpenAI failed response omitted error"))?;
                let code = optional_string(error, "code");
                let message = required_string(error, "message")?;
                Ok(vec![ModelEvent::Failed {
                    failure: provider_failure(FailureKind::Provider, message, code),
                }])
            }
            TerminalKind::Completed | TerminalKind::Incomplete => {
                if !self.items.is_empty() || !self.text_parts.is_empty() {
                    return Err(protocol_failure(
                        "OpenAI terminal response had unfinished output items",
                    ));
                }
                let usage = parse_usage(response, self.usage_required)?;
                let finish_reason = match kind {
                    TerminalKind::Completed if self.saw_refusal => FinishReason::Refusal,
                    TerminalKind::Completed if self.saw_tool_call => FinishReason::ToolCalls,
                    TerminalKind::Completed => {
                        if self.output_mode == OutputMode::Structured && !self.structured_emitted {
                            return Err(protocol_failure(
                                "OpenAI completed without the required structured output",
                            ));
                        }
                        FinishReason::EndTurn
                    }
                    TerminalKind::Incomplete => {
                        let details = response
                            .get("incomplete_details")
                            .and_then(Value::as_object)
                            .ok_or_else(|| {
                                protocol_failure(
                                    "OpenAI incomplete response omitted incomplete_details",
                                )
                            })?;
                        let reason = required_string(details, "reason")?;
                        match reason {
                            "max_tokens" | "max_output_tokens" => FinishReason::MaxOutputTokens,
                            "content_filter" => FinishReason::ContentFilter,
                            other => FinishReason::Other(bounded_finish_reason(other)?),
                        }
                    }
                    TerminalKind::Failed => unreachable!(),
                };
                let continuation = if kind == TerminalKind::Completed
                    && self.storage == OpenAiStoragePolicy::ProviderManaged
                {
                    Some(
                        ContinuationState::new(
                            OPENAI_PROVIDER,
                            OPENAI_CONTINUATION_FORMAT,
                            serde_json::json!({"response_id":response_id}),
                        )
                        .map_err(|error| {
                            protocol_failure(format!(
                                "OpenAI response ID could not form continuation state: {error}"
                            ))
                        })?,
                    )
                } else {
                    None
                };
                self.terminal = true;
                let mut events = Vec::with_capacity(2);
                if let Some(usage) = usage {
                    events.push(ModelEvent::UsageUpdate {
                        update: UsageUpdate {
                            semantics: UsageSemantics::Cumulative,
                            usage,
                        },
                    });
                }
                events.push(ModelEvent::Completed {
                    finish_reason,
                    continuation,
                });
                Ok(events)
            }
        }
    }

    fn standalone_error(
        &mut self,
        object: &Map<String, Value>,
    ) -> Result<Vec<ModelEvent>, ModelFailure> {
        let code = optional_string(object, "code");
        let message = required_string(object, "message")?;
        self.terminal = true;
        Ok(vec![ModelEvent::Failed {
            failure: provider_failure(FailureKind::Provider, message, code),
        }])
    }

    fn require_created(&self) -> Result<(), ModelFailure> {
        if self.response_id.is_none() {
            return Err(protocol_failure(
                "OpenAI output event preceded response.created",
            ));
        }
        Ok(())
    }

    fn require_response_id<'a>(
        &self,
        response: &'a Map<String, Value>,
    ) -> Result<&'a str, ModelFailure> {
        let id = required_string(response, "id")?;
        if self.response_id.as_deref() != Some(id) {
            return Err(protocol_failure("OpenAI response identity changed"));
        }
        Ok(id)
    }

    fn require_message_item(&self, item_id: &str, output_index: u64) -> Result<(), ModelFailure> {
        let item = self.items.get(item_id).ok_or_else(|| {
            protocol_failure(format!(
                "OpenAI text referenced unknown output item {item_id}"
            ))
        })?;
        if item.output_index != output_index || !matches!(item.kind, OutputItemKind::Message) {
            return Err(protocol_failure(format!(
                "OpenAI text item {item_id} changed output index or type"
            )));
        }
        Ok(())
    }

    fn require_function_item_mut(
        &mut self,
        item_id: &str,
        output_index: u64,
    ) -> Result<&mut FunctionItem, ModelFailure> {
        let item = self.items.get_mut(item_id).ok_or_else(|| {
            protocol_failure(format!(
                "OpenAI arguments referenced unknown output item {item_id}"
            ))
        })?;
        if item.output_index != output_index {
            return Err(protocol_failure(format!(
                "OpenAI function item {item_id} changed output index"
            )));
        }
        match &mut item.kind {
            OutputItemKind::Function(function) => Ok(function),
            OutputItemKind::Message | OutputItemKind::Other(_) => Err(protocol_failure(format!(
                "OpenAI arguments referenced non-function item {item_id}"
            ))),
        }
    }

    fn check_active_capacity(&self) -> Result<(), ModelFailure> {
        if self.items.len().saturating_add(self.text_parts.len()) >= MAX_ACTIVE_OUTPUT_ITEMS {
            return Err(protocol_failure(
                "OpenAI stream exceeded the active output-item limit",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TerminalKind {
    Completed,
    Incomplete,
    Failed,
}

fn parse_usage(
    response: &Map<String, Value>,
    required: bool,
) -> Result<Option<TokenUsage>, ModelFailure> {
    let usage = match response.get("usage") {
        None | Some(Value::Null) if !required => return Ok(None),
        None | Some(Value::Null) => {
            return Err(protocol_failure(
                "OpenAI terminal response omitted required usage",
            ));
        }
        Some(Value::Object(usage)) => usage,
        Some(_) => {
            return Err(protocol_failure(
                "OpenAI terminal response usage was not an object or null",
            ));
        }
    };
    let input_tokens = required_u64(usage, "input_tokens")?;
    let output_tokens = required_u64(usage, "output_tokens")?;
    let total_tokens = required_u64(usage, "total_tokens")?;
    let input_details = optional_object(usage, "input_tokens_details")?;
    let cached_input_tokens = input_details
        .map(|details| optional_u64(details, "cached_tokens"))
        .transpose()?
        .flatten();
    let output_details = optional_object(usage, "output_tokens_details")?;
    let reasoning_tokens = output_details
        .map(|details| optional_u64(details, "reasoning_tokens"))
        .transpose()?
        .flatten();
    let mut details = BTreeMap::new();
    let nested_cache_write = input_details
        .map(|details| optional_u64(details, "cache_write_tokens"))
        .transpose()?
        .flatten();
    let top_level_cache_write = optional_u64(usage, "cache_write_tokens")?;
    if let Some(cache_write_tokens) = nested_cache_write.or(top_level_cache_write) {
        details.insert("cache_write_tokens".into(), cache_write_tokens);
    }
    Ok(Some(TokenUsage {
        input_tokens: Some(input_tokens),
        output_tokens: Some(output_tokens),
        cached_input_tokens,
        reasoning_tokens,
        total_tokens: Some(total_tokens),
        details,
    }))
}

fn required_object<'a>(
    object: &'a Map<String, Value>,
    field: &str,
) -> Result<&'a Map<String, Value>, ModelFailure> {
    object
        .get(field)
        .and_then(Value::as_object)
        .ok_or_else(|| protocol_failure(format!("OpenAI event field {field:?} was not an object")))
}

fn required_string<'a>(
    object: &'a Map<String, Value>,
    field: &str,
) -> Result<&'a str, ModelFailure> {
    object
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| protocol_failure(format!("OpenAI event field {field:?} was not a string")))
}

fn optional_string<'a>(object: &'a Map<String, Value>, field: &str) -> Option<&'a str> {
    object.get(field).and_then(Value::as_str)
}

fn optional_typed_string<'a>(
    object: &'a Map<String, Value>,
    field: &str,
) -> Result<Option<&'a str>, ModelFailure> {
    match object.get(field) {
        None => Ok(None),
        Some(Value::String(value)) => Ok(Some(value)),
        Some(_) => Err(protocol_failure(format!(
            "OpenAI event field {field:?} was present but not a string"
        ))),
    }
}

fn optional_object<'a>(
    object: &'a Map<String, Value>,
    field: &str,
) -> Result<Option<&'a Map<String, Value>>, ModelFailure> {
    match object.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Object(value)) => Ok(Some(value)),
        Some(_) => Err(protocol_failure(format!(
            "OpenAI event field {field:?} was present but not an object or null"
        ))),
    }
}

fn optional_u64(object: &Map<String, Value>, field: &str) -> Result<Option<u64>, ModelFailure> {
    match object.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value.as_u64().map(Some).ok_or_else(|| {
            protocol_failure(format!(
                "OpenAI event field {field:?} was present but not an unsigned integer"
            ))
        }),
    }
}

fn validate_required_status(
    object: &Map<String, Value>,
    expected: &str,
    context: &str,
) -> Result<(), ModelFailure> {
    let status = required_string(object, "status")?;
    if status != expected {
        return Err(protocol_failure(format!(
            "OpenAI {context} carried unexpected status {status:?}"
        )));
    }
    Ok(())
}

fn required_u64(object: &Map<String, Value>, field: &str) -> Result<u64, ModelFailure> {
    object.get(field).and_then(Value::as_u64).ok_or_else(|| {
        protocol_failure(format!(
            "OpenAI event field {field:?} was not an unsigned integer"
        ))
    })
}

fn bounded_identifier(value: &str, field: &str) -> Result<String, ModelFailure> {
    if value.is_empty()
        || value.len() > MAX_IDENTIFIER_BYTES
        || value.trim() != value
        || value.chars().any(char::is_control)
    {
        return Err(protocol_failure(format!(
            "OpenAI {field} was empty, oversized, or contained controls"
        )));
    }
    Ok(value.into())
}

fn provider_call_id(value: &str) -> Result<ProviderCallId, ModelFailure> {
    ProviderCallId::new(value)
        .map_err(|error| protocol_failure(format!("OpenAI provider call ID was invalid: {error}")))
}

fn valid_function_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn bounded_finish_reason(value: &str) -> Result<String, ModelFailure> {
    let bounded = bounded_string(value, MAX_PROVIDER_CODE_BYTES);
    if bounded.is_empty() || bounded.trim() != bounded || bounded.chars().any(char::is_control) {
        return Err(protocol_failure(
            "OpenAI incomplete reason was empty or contained controls",
        ));
    }
    Ok(bounded)
}

fn protocol_failure(message: impl Into<String>) -> ModelFailure {
    let message = sanitize_credential_tokens(&message.into());
    ModelFailure::new(
        FailureKind::Protocol,
        bounded_string(&message, MAX_PROVIDER_MESSAGE_BYTES),
    )
}

fn tool_failure(
    kind: FailureKind,
    call_id: &ProviderCallId,
    message: impl Into<String>,
) -> ModelFailure {
    let message = sanitize_credential_tokens(&message.into());
    let mut failure = ModelFailure::new(kind, bounded_string(&message, MAX_PROVIDER_MESSAGE_BYTES));
    failure.call_id = Some(call_id.clone());
    failure
}

fn provider_failure(kind: FailureKind, message: &str, code: Option<&str>) -> ModelFailure {
    let message = sanitize_credential_tokens(message);
    let mut failure = ModelFailure::new(kind, bounded_string(&message, MAX_PROVIDER_MESSAGE_BYTES));
    failure.provider_code =
        code.map(|code| bounded_string(&sanitize_credential_tokens(code), MAX_PROVIDER_CODE_BYTES));
    failure
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decoder_accepts_crlf_comments_multiline_data_and_byte_splits() {
        let source = b": keepalive\r\nevent: example\r\ndata: {\r\ndata: \"a\":1}\r\n\r\n";
        for split in 1..=source.len() {
            let mut decoder = SseDecoder::default();
            let mut events = Vec::new();
            for chunk in source.chunks(split) {
                let (decoded, failure) = decoder.push_preserving_prefix(chunk);
                assert!(failure.is_none(), "decode chunk: {failure:?}");
                events.extend(decoded);
            }
            let (decoded, failure) = decoder.finish_preserving_prefix();
            assert!(failure.is_none(), "finish decoder: {failure:?}");
            events.extend(decoded);
            assert_eq!(
                events,
                vec![SseEvent {
                    event: Some("example".into()),
                    data: "{\n\"a\":1}".into(),
                }]
            );
        }
    }

    #[test]
    fn decoder_rejects_invalid_utf8_and_unterminated_oversize_input() {
        let mut decoder = SseDecoder::default();
        assert!(decoder.push_preserving_prefix(&[0xff, b'\n']).1.is_some());

        let mut decoder = SseDecoder::default();
        assert!(
            decoder
                .push_preserving_prefix(&vec![b'x'; MAX_SSE_BUFFER_BYTES + 1])
                .1
                .is_some()
        );
    }

    #[test]
    fn decoder_preserves_dispatched_prefix_before_a_later_byte_failure() {
        let mut source = b"event: example\ndata: {\"ok\":true}\n\n".to_vec();
        source.extend_from_slice(&[0xff, b'\n']);
        let mut decoder = SseDecoder::default();
        let (events, failure) = decoder.push_preserving_prefix(&source);
        assert_eq!(
            events,
            vec![SseEvent {
                event: Some("example".into()),
                data: "{\"ok\":true}".into(),
            }]
        );
        assert_eq!(failure.expect("invalid UTF-8").kind, FailureKind::Protocol);
    }
}
