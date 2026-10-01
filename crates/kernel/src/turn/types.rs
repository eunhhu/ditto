use chrono::{DateTime, Utc};
use ditto_artifact_read::{ArtifactReadResource, ArtifactReadResult};
use ditto_capability::{CapabilityManifest, CapabilityRevision, ExecutionEpochEvidence};
use ditto_context::CompiledContext;
use ditto_model::{
    ExecutionEpochId, ModelRequest, ModelRequestId, ModelStreamEvent, ProviderCallId,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::KernelError;

/// The turn contract this kernel writes and replays (ADR 0034): until Ditto's
/// first release there is exactly one, and a change replaces it. Turns
/// recorded under an earlier contract stay in the journal, and their answers
/// stay readable in run status and conversation history, but replay rejects
/// them.
pub const TURN_PAYLOAD_VERSION: u16 = 12;
/// Oldest contract whose terminal events run status still reads: their
/// shape has not changed.
pub const MIN_TURN_PAYLOAD_VERSION: u16 = 1;
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
    /// Prior turns replayed as conversation history, oldest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub history_turn_ids: Vec<String>,
    /// The host's UTC offset at acceptance, in minutes, which fixes the local
    /// time the latest message's note states.
    pub utc_offset_minutes: i32,
}

/// The selection in full, as replay rebuilds it from the recorded references.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilitiesSelectedPayload {
    pub event_version: u16,
    pub turn_id: String,
    pub manifest: CapabilityManifest,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sort_manifest: Option<CapabilityManifest>,
    /// Version 5: `web.fetch`, paged when the user's message holds URLs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fetch_manifest: Option<CapabilityManifest>,
    /// Version 8: `memory.search`, offered to every agent run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_manifest: Option<CapabilityManifest>,
    /// Version 10: `memory.remember` and `memory.forget`, only ever rebuilt
    /// from a reference.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remember_manifest: Option<CapabilityManifest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forget_manifest: Option<CapabilityManifest>,
    /// Version 11: `web.search`, only ever rebuilt from a reference.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search_manifest: Option<CapabilityManifest>,
    pub epoch: ExecutionEpochEvidence,
}

/// The durable form of `capabilities.selected` (ADR 0030): the epoch's
/// identity and the exact contracts it bound, each by its digests. Every
/// builtin equals its packaged contract, so replay rebuilds the manifests,
/// cards and schemas from code and checks each digest.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CapabilitiesSelectedRefPayload {
    pub event_version: u16,
    pub turn_id: String,
    pub epoch_id: String,
    pub contracts: Vec<CapabilityRevision>,
}

/// A model request as sent, as replay rebuilds it from its recorded digest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelRequestedPayload {
    pub event_version: u16,
    pub turn_id: String,
    pub request_index: u8,
    pub request: ModelRequest,
}

/// Version-7 durable form of `model.requested`: the request's identity,
/// deadline and SHA-256. Every other part derives from earlier durable
/// events, so replay rebuilds the request and must reproduce the digest.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelRequestDigestPayload {
    pub event_version: u16,
    pub turn_id: String,
    pub request_index: u8,
    pub request_id: ModelRequestId,
    pub deadline: DateTime<Utc>,
    pub request_sha256: String,
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
    /// Version 7: the last provider stream sequence a coalesced text chunk
    /// covers, from `stream_event.sequence`; absent for a single event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub through_sequence: Option<u64>,
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
    /// Typed cause of a validator-derived failure; present only in version-2
    /// turns. The message stays diagnostic text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<TurnFailureReason>,
}

/// Closed version-2 cause for failures whose message comes from a validator
/// outside the turn state machine (context compiler, capability contracts,
/// driver contracts, tool-call lifecycle). Replay checks this value and the
/// stage it belongs to, never the validator's wording, so that wording can
/// change without invalidating recorded turns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnFailureReason {
    SessionContextUnavailable,
    DuplicateContextCandidate,
    EmptyPolicyReason,
    InvalidRequiredContext,
    RequiredContextOverBudget,
    MissingContextProvenance,
    UnresolvedContextProvenance,
    ConversationHistoryUnavailable,
    ArtifactReadUnavailable,
    ArtifactReadPackageUnverified,
    ArtifactReadManifestMismatch,
    ArtifactReadSchemaMismatch,
    ArtifactReadSelectionFailed,
    SortPermissionSourceUnavailable,
    SortContractUnavailable,
    DriverFeaturesUnsupported,
    DriverToolChoiceUnsupported,
    DriverParallelCallsUnsupported,
    RequestInvalidAtDispatch,
    ToolCallLifecycle,
}

impl TurnFailureReason {
    /// The failure code that always accompanies this reason.
    pub const fn code(self) -> TurnFailureCode {
        match self {
            Self::SessionContextUnavailable
            | Self::DuplicateContextCandidate
            | Self::EmptyPolicyReason
            | Self::InvalidRequiredContext
            | Self::RequiredContextOverBudget
            | Self::MissingContextProvenance
            | Self::UnresolvedContextProvenance
            | Self::ConversationHistoryUnavailable => TurnFailureCode::ContextCompilation,
            Self::ArtifactReadUnavailable => TurnFailureCode::CapabilityUnavailable,
            Self::ArtifactReadPackageUnverified
            | Self::ArtifactReadManifestMismatch
            | Self::ArtifactReadSchemaMismatch
            | Self::ArtifactReadSelectionFailed
            | Self::SortPermissionSourceUnavailable
            | Self::SortContractUnavailable => TurnFailureCode::CapabilityContract,
            Self::DriverFeaturesUnsupported
            | Self::DriverToolChoiceUnsupported
            | Self::DriverParallelCallsUnsupported
            | Self::RequestInvalidAtDispatch => TurnFailureCode::DriverContract,
            Self::ToolCallLifecycle => TurnFailureCode::Protocol,
        }
    }
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sort_calls: Vec<super::sort::ReplayedSortCall>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fetch_calls: Vec<super::fetch::ReplayedFetchCall>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recall_calls: Vec<super::recall::ReplayedRecallCall>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub memory_writes: Vec<super::memory_write::ReplayedMemoryWrite>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub search_calls: Vec<super::search::ReplayedSearchCall>,
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
