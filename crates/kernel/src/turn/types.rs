use chrono::{DateTime, Utc};
use ditto_artifact_read::{ArtifactReadResource, ArtifactReadResult};
use ditto_capability::{CapabilityManifest, CapabilitySchema, ExecutionEpoch};
use ditto_context::{CompiledContext, ContextCapsule};
use ditto_model::{
    ExecutionEpochId, ModelRequest, ModelRequestId, ModelStreamEvent, ProviderCallId,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::KernelError;

pub const TURN_PAYLOAD_VERSION: u16 = 1;
pub const MAX_MODEL_REQUESTS: usize = 8;
pub const MAX_MODEL_EVENTS_PER_REQUEST: usize = 4_096;
pub const MAX_ASSISTANT_TEXT_BYTES: usize = 256 * 1_024;
pub const MAX_MODEL_OUTPUT_EVENT_BYTES: usize = 320 * 1_024;
pub const MAX_MODEL_OUTPUT_BYTES_PER_REQUEST: usize = 4 * 1_024 * 1_024;
pub const MAX_TURN_FAILURE_MESSAGE_BYTES: usize = 4 * 1_024;
pub const MAX_TURN_DURATION: std::time::Duration = std::time::Duration::from_secs(5 * 60);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextCompiledPayload {
    pub event_version: u16,
    pub turn_id: String,
    pub provenance_through_seq: i64,
    pub compiled: CompiledContext,
    pub capsule: ContextCapsule,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilitiesSelectedPayload {
    pub event_version: u16,
    pub turn_id: String,
    pub manifest: CapabilityManifest,
    pub epoch: ExecutionEpoch,
    pub schemas: Vec<CapabilitySchema>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelRequestedPayload {
    pub event_version: u16,
    pub turn_id: String,
    pub request_index: u8,
    pub request: ModelRequest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelOutputPayload {
    pub event_version: u16,
    pub turn_id: String,
    pub request_index: u8,
    pub request_id: ModelRequestId,
    #[serde(with = "chrono::serde::ts_milliseconds")]
    pub admitted_at: DateTime<Utc>,
    pub stream_event: ModelStreamEvent,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CapabilityRequestedPayload {
    pub event_version: u16,
    pub turn_id: String,
    pub request_index: u8,
    pub execution_epoch_id: ExecutionEpochId,
    pub call_id: ProviderCallId,
    pub capability_id: String,
    pub capability_version: String,
    pub arguments: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub normalized: Option<ArtifactReadResource>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecutionStartedPayload {
    pub event_version: u16,
    pub turn_id: String,
    pub request_index: u8,
    pub call_id: ProviderCallId,
    pub capability_id: String,
    pub capability_version: String,
    pub authorization_through_seq: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource: Option<ArtifactReadResource>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecutionOutputPayload {
    pub event_version: u16,
    pub turn_id: String,
    pub request_index: u8,
    pub call_id: ProviderCallId,
    pub capability_id: String,
    pub capability_version: String,
    pub result: ArtifactReadResult,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactReadTurnStatus {
    Unverified,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactReadTurnOutcome {
    pub turn_id: String,
    pub session_id: String,
    pub task_id: String,
    pub execution_epoch_id: ExecutionEpochId,
    pub response: String,
    pub status: ArtifactReadTurnStatus,
    pub request_count: u8,
    pub tool_call_count: u8,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnFinishedPayload {
    pub event_version: u16,
    pub turn_id: String,
    pub outcome: ArtifactReadTurnOutcome,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnFailureCode {
    InvalidInput,
    ContextCompilation,
    CapabilityUnavailable,
    CapabilityContract,
    DriverContract,
    ModelFailure,
    Protocol,
    Cancelled,
    DeadlineExceeded,
    BoundExceeded,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnFailure {
    pub turn_id: String,
    pub session_id: String,
    pub task_id: String,
    pub code: TurnFailureCode,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_index: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_id: Option<ProviderCallId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<TurnFailureEvidence>,
}

/// Closed, typed evidence for terminal failures whose validity cannot be
/// reconstructed from the preceding accepted turn events alone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum TurnFailureEvidence {
    Deadline {
        #[serde(with = "chrono::serde::ts_milliseconds")]
        deadline: DateTime<Utc>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnFailedPayload {
    pub event_version: u16,
    pub turn_id: String,
    pub failure: TurnFailure,
    pub status: ArtifactReadTurnStatus,
    pub request_count: u8,
    pub tool_call_count: u8,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ArtifactReadTurnReplay {
    Finished { outcome: ArtifactReadTurnOutcome },
    Failed { failure: TurnFailure },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnSequenceSpan {
    pub first_seq: i64,
    pub last_seq: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplayedArtifactReadCall {
    pub requested: CapabilityRequestedPayload,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started: Option<ExecutionStartedPayload>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<ExecutionOutputPayload>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplayedReadOnlyTurn {
    pub turn_id: String,
    pub session_id: String,
    pub task_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<ContextCompiledPayload>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<CapabilitiesSelectedPayload>,
    pub requests: Vec<ModelRequestedPayload>,
    pub outputs: Vec<ModelOutputPayload>,
    pub calls: Vec<ReplayedArtifactReadCall>,
    pub terminal: ArtifactReadTurnReplay,
    pub sequence_span: TurnSequenceSpan,
}

#[derive(Debug, Error)]
pub enum TurnRunError {
    #[error(transparent)]
    Kernel(#[from] KernelError),
    #[error("turn failed: {0:?}")]
    Failed(Box<TurnFailure>),
    #[error("turn payload serialization failed: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("turn runtime invariant failed: {0}")]
    Internal(&'static str),
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ReplayError {
    #[error("turn replay is invalid: {0}")]
    Invalid(String),
}
/// Trusted kernel-only controls for a read-only turn. This type deliberately
/// has no serde implementation, so an untrusted command cannot smuggle in
/// harness timing authority.
#[derive(Debug, Clone, Copy, Default)]
pub struct ReadOnlyTurnControl {
    pub deadline: Option<DateTime<Utc>>,
}
