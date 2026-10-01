//! The lifecycle every agent tool call shares (ADR 0036): `tool.requested`
//! records the model's call and its normalized arguments, `tool.started` the
//! claim under which a leased call ran, and `tool.output` its result. Run and
//! replay share these envelope rules; each tool adds only its own.
use chrono::{DateTime, Utc};
use ditto_model::ProviderCallId;
use ditto_protocol::{EventActor, EventRecord, event_kind};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The turn failure of a call stopped before authorization.
pub(crate) const STOPPED: &str = "tool call stopped before authorization";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolRequested {
    pub event_version: u16,
    pub turn_id: String,
    pub request_index: u8,
    pub call_id: ProviderCallId,
    pub capability_id: String,
    pub arguments: Value,
    /// The arguments as the tool normalizes them; `None` when invalid.
    pub normalized: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolStarted {
    pub event_version: u16,
    pub turn_id: String,
    pub request_index: u8,
    pub call_id: ProviderCallId,
    pub capability_id: String,
    pub epoch_id: String,
    pub invocation_digest: String,
    pub claim_id: String,
    pub permit_id: String,
    pub claimed_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    /// What a search became at the operator's endpoint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolOutput {
    pub event_version: u16,
    pub turn_id: String,
    pub request_index: u8,
    pub call_id: ProviderCallId,
    pub capability_id: String,
    /// The claim was consumed: the call started, whatever it then did.
    pub claimed: bool,
    pub result: Value,
    /// The verified artifact a sort produced.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_event_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplayedToolCall {
    pub requested: ToolRequested,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started: Option<ToolStarted>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<ToolOutput>,
}

impl ReplayedToolCall {
    /// The recorded result as `T`, once the call has one.
    pub fn result<T: serde::de::DeserializeOwned>(&self) -> Option<T> {
        serde_json::from_value(self.output.as_ref()?.result.clone()).ok()
    }
}

/// Why a leased call did not start: its lease had no call left, or the turn
/// ran out of time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LeaseRefusal {
    Exhausted,
    Expired,
}

fn same_turn(event: &EventRecord, input: &EventRecord) -> bool {
    event.session_id == input.session_id
        && event.task_id == input.task_id
        && event.correlation_id == input.correlation_id
        && event.seq > input.seq
}

/// A `tool.requested` event for `call`, normalized by replay's own rule.
pub(crate) fn valid_requested(
    request: &ToolRequested,
    event: &EventRecord,
    input: &EventRecord,
    normalized: Option<&Value>,
) -> bool {
    request.event_version == 1
        && request.turn_id == input.correlation_id.as_deref().unwrap_or_default()
        && request.request_index < 7
        && request.normalized.as_ref() == normalized
        && event.kind == event_kind::TOOL_REQUESTED
        && event.actor == EventActor::Model
        && event.span_id.as_deref() == Some(request.call_id.as_str())
        && same_turn(event, input)
}

/// A `tool.started` event right after `requested`: well-formed claim
/// evidence of that call. Replay also binds it to the turn's epoch and
/// deadline.
pub(crate) fn valid_started(
    start: &ToolStarted,
    event: &EventRecord,
    request: &ToolRequested,
    requested: &EventRecord,
    input: &EventRecord,
) -> bool {
    let digest = &start.invocation_digest;
    start.event_version == 1
        && start.turn_id == request.turn_id
        && start.request_index == request.request_index
        && start.call_id == request.call_id
        && start.capability_id == request.capability_id
        && request.normalized.is_some()
        && event.kind == event_kind::TOOL_STARTED
        && event.actor == EventActor::Capability
        && event.span_id.as_deref() == Some(start.call_id.as_str())
        && event.causation_id.as_deref() == Some(&requested.event_id)
        && event.seq > requested.seq
        && same_turn(event, input)
        && !start.epoch_id.is_empty()
        && start.epoch_id.len() <= 256
        && digest.len() == 64
        && digest
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        && start.claim_id == format!("claim_{digest}")
        && start.permit_id == format!("permit_{digest}")
        && start.claimed_at.timestamp_millis() <= event.recorded_at.timestamp_millis()
        && start.claimed_at >= input.recorded_at
        && start.expires_at > start.claimed_at
        && start.expires_at
            <= input.recorded_at
                + chrono::Duration::seconds(super::types::MAX_TURN_DURATION.as_secs() as i64)
}

/// A `tool.output` event closing `request`: caused by its start, or by the
/// request when the call never started.
pub(crate) fn valid_output(
    output: &ToolOutput,
    event: &EventRecord,
    request: &ToolRequested,
    requested: &EventRecord,
    started: Option<&EventRecord>,
    input: &EventRecord,
) -> bool {
    let cause = started.unwrap_or(requested);
    output.event_version == 1
        && output.turn_id == request.turn_id
        && output.request_index == request.request_index
        && output.call_id == request.call_id
        && output.capability_id == request.capability_id
        && output.claimed == started.is_some()
        && event.kind == event_kind::TOOL_OUTPUT
        && event.actor == EventActor::Capability
        && event.span_id.as_deref() == Some(output.call_id.as_str())
        && event.causation_id.as_deref() == Some(&cause.event_id)
        && event.seq > cause.seq
        && same_turn(event, input)
}
