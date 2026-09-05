//! The additional closed tool profile used only by explicitly permitted runs.
use chrono::{DateTime, Utc};
use ditto_artifact_sort::{self as worker, SortArguments};
use ditto_capability::CanonicalResource;
use ditto_model::{ContentPart, ConversationItem, MessageRole, ProviderCallId};
use ditto_protocol::{AgentSortPermission, EventActor, EventRecord, event_kind};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SortGrant {
    pub reference: String,
    pub source_event_id: String,
    pub allow_deduplicate: bool,
}

impl SortGrant {
    pub fn validate(&self) -> Result<(), &'static str> {
        CanonicalResource::artifact(&self.reference).map_err(|_| "invalid sort reference")?;
        let id: ulid::Ulid = self
            .source_event_id
            .parse()
            .map_err(|_| "invalid sort source")?;
        if id.to_string() != self.source_event_id {
            return Err("invalid sort source");
        }
        Ok(())
    }

    pub fn permits(&self, args: &SortArguments) -> bool {
        self.reference == args.reference && (!args.unique || self.allow_deduplicate)
    }

    pub fn matches_root(&self, root: &EventRecord, input: &EventRecord) -> bool {
        root.event_id == self.source_event_id
            && root.seq < input.seq
            && root.kind == event_kind::ARTIFACT_CREATED
            && root.actor == EventActor::System
            && root.session_id == input.session_id
            && root.task_id == input.task_id
            && root.causation_id.is_none()
            && root.correlation_id.is_none()
            && root.payload["reference"] == self.reference
            && root.payload["bytes"]
                .as_u64()
                .is_some_and(|n| n <= worker::MAX_INPUT_BYTES as u64)
    }
}

pub(crate) fn permission_matches(
    grant: Option<&SortGrant>,
    permission: Option<&AgentSortPermission>,
) -> bool {
    match (grant, permission) {
        (None, None) => true,
        (Some(grant), Some(permission)) => {
            grant.reference == worker::input_reference(permission.text.as_bytes())
                && grant.allow_deduplicate == permission.allow_deduplicate
        }
        _ => false,
    }
}

pub(super) fn initial_conversation(
    text: String,
    grant: Option<&SortGrant>,
) -> Vec<ConversationItem> {
    let mut content = vec![ContentPart::Text { text }];
    if let Some(grant) = grant {
        content.push(ContentPart::Structured { value: json!({
            "attachment": {"reference":grant.reference},
            "user_permission": {"capability":"artifact.sort", "maximum_calls":1,
                "allow_deduplicate":grant.allow_deduplicate, "expires":"end of this bounded turn"}
        }) });
    }
    vec![ConversationItem::Message {
        role: MessageRole::User,
        content,
    }]
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SortToolRequested {
    pub event_version: u16,
    pub turn_id: String,
    pub request_index: u8,
    pub call_id: ProviderCallId,
    pub arguments: Value,
    pub normalized: Option<SortArguments>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SortToolStarted {
    pub event_version: u16,
    pub turn_id: String,
    pub request_index: u8,
    pub call_id: ProviderCallId,
    pub epoch_id: String,
    pub invocation_digest: String,
    pub claim_id: String,
    pub permit_id: String,
    pub claimed_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub normalized: SortArguments,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SortToolError {
    InvalidArguments,
    PermissionDenied,
    LeaseExhausted,
    LeaseExpired,
    InputUnavailable,
    Cancelled,
    ProcessDeadline,
    ProcessFailed,
    VerificationFailed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case", deny_unknown_fields)]
pub enum SortToolResult {
    Verified {
        reference: String,
        verifier: String,
        input_lines: usize,
        output_lines: usize,
    },
    Error {
        code: SortToolError,
    },
}

impl SortToolResult {
    pub(super) fn model_value(&self) -> Value {
        match self {
            Self::Verified {
                reference,
                verifier,
                input_lines,
                output_lines,
            } => json!({
                "reference":reference,"verifier":verifier,"input_lines":input_lines,"output_lines":output_lines
            }),
            Self::Error { code } => json!({"error":code}),
        }
    }
    pub(super) fn is_error(&self) -> bool {
        matches!(self, Self::Error { .. })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SortToolOutput {
    pub event_version: u16,
    pub turn_id: String,
    pub request_index: u8,
    pub call_id: ProviderCallId,
    /// Claim consumption, not a claim that the OS definitely spawned a process.
    pub claimed: bool,
    pub result: SortToolResult,
    pub artifact_event_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplayedSortCall {
    pub requested: SortToolRequested,
    pub started: Option<SortToolStarted>,
    pub output: Option<SortToolOutput>,
}

pub(crate) fn normalize_call(arguments: &Value) -> Option<SortArguments> {
    ditto_capability::validate_invocation_instance(&worker::schema().input_schema, arguments)
        .ok()?;
    serde_json::from_value(arguments.clone()).ok()
}

pub(crate) fn valid_started(
    start: &SortToolStarted,
    event: &EventRecord,
    input: &EventRecord,
    grant: &SortGrant,
) -> bool {
    let digest = &start.invocation_digest;
    start.event_version == 1
        && start.turn_id == input.correlation_id.as_deref().unwrap_or_default()
        && event.span_id.as_deref() == Some(start.call_id.as_str())
        && event.actor == EventActor::Capability
        && event.kind == event_kind::AGENT_SORT_STARTED
        && event.session_id == input.session_id
        && event.task_id == input.task_id
        && event.correlation_id == input.correlation_id
        && event.seq > input.seq
        && grant.permits(&start.normalized)
        && start.request_index < 7
        && !start.epoch_id.is_empty()
        && start.epoch_id.len() <= 256
        && CanonicalResource::artifact(format!("artifact:sha256:{digest}")).is_ok()
        && start.claim_id == format!("claim_{digest}")
        && start.permit_id == format!("permit_{digest}")
        && start.claimed_at.timestamp_millis() <= event.recorded_at.timestamp_millis()
        && start.claimed_at >= input.recorded_at
        && start.expires_at > start.claimed_at
        && start.expires_at <= input.recorded_at + chrono::Duration::minutes(5)
}

pub(crate) fn valid_requested(
    request: &SortToolRequested,
    event: &EventRecord,
    input: &EventRecord,
) -> bool {
    request.event_version == 1
        && request.turn_id == input.correlation_id.as_deref().unwrap_or_default()
        && request.request_index < 7
        && request.normalized == normalize_call(&request.arguments)
        && event.kind == event_kind::AGENT_SORT_REQUESTED
        && event.actor == EventActor::Model
        && event.span_id.as_deref() == Some(request.call_id.as_str())
        && event.session_id == input.session_id
        && event.task_id == input.task_id
        && event.correlation_id == input.correlation_id
        && event.seq > input.seq
}

pub(crate) fn start_matches_request(
    start: &SortToolStarted,
    event: &EventRecord,
    request: &SortToolRequested,
    requested: &EventRecord,
) -> bool {
    start.request_index == request.request_index
        && start.call_id == request.call_id
        && request.normalized.as_ref() == Some(&start.normalized)
        && event.causation_id.as_deref() == Some(&requested.event_id)
        && event.seq > requested.seq
}

/// Structural evidence only. Status additionally hashes both artifacts and
/// independently checks the line contract; replay deliberately performs no I/O.
#[allow(clippy::too_many_arguments)]
pub(crate) fn valid_output(
    output: &SortToolOutput,
    event: &EventRecord,
    input: &EventRecord,
    grant: &SortGrant,
    request: &SortToolRequested,
    requested: &EventRecord,
    started: Option<&EventRecord>,
    already_claimed: bool,
    deadline: DateTime<Utc>,
) -> bool {
    if output.event_version != 1
        || output.turn_id != request.turn_id
        || output.call_id != request.call_id
        || output.request_index != request.request_index
        || output.claimed != started.is_some()
        || event.actor != EventActor::Capability
        || event.kind != event_kind::AGENT_SORT_OUTPUT
        || event.span_id.as_deref() != Some(output.call_id.as_str())
        || event.session_id != input.session_id
        || event.task_id != input.task_id
        || event.correlation_id != input.correlation_id
        || event.causation_id.as_deref() != Some(&started.unwrap_or(requested).event_id)
        || event.seq <= started.unwrap_or(requested).seq
    {
        return false;
    }
    if let SortToolResult::Error { code } = &output.result {
        if output.artifact_event_id.is_some() {
            return false;
        }
        return if started.is_some() {
            matches!(
                code,
                SortToolError::InputUnavailable
                    | SortToolError::Cancelled
                    | SortToolError::ProcessDeadline
                    | SortToolError::ProcessFailed
                    | SortToolError::VerificationFailed
            )
        } else {
            match request.normalized.as_ref() {
                None => *code == SortToolError::InvalidArguments,
                Some(args) if !grant.permits(args) => *code == SortToolError::PermissionDenied,
                Some(_) => {
                    (already_claimed && *code == SortToolError::LeaseExhausted)
                        || (*code == SortToolError::LeaseExpired && event.recorded_at >= deadline)
                }
            }
        };
    }
    let SortToolResult::Verified {
        reference,
        verifier,
        input_lines,
        output_lines,
    } = &output.result
    else {
        return false;
    };
    started.is_some()
        && output.artifact_event_id.is_some()
        && CanonicalResource::artifact(reference).is_ok()
        && verifier == worker::VERIFIER
        && *input_lines <= worker::MAX_LINES
        && *output_lines <= *input_lines
}

pub(crate) fn valid_output_root(
    root: &EventRecord,
    output: &SortToolOutput,
    event: &EventRecord,
    started: &EventRecord,
) -> bool {
    let SortToolResult::Verified { reference, .. } = &output.result else {
        return false;
    };
    Some(&root.event_id) == output.artifact_event_id.as_ref()
        && root.kind == event_kind::ARTIFACT_CREATED
        && root.actor == EventActor::System
        && root.session_id == event.session_id
        && root.task_id == event.task_id
        && root.correlation_id.is_none()
        && root.causation_id.as_deref() == Some(&started.event_id)
        && root.seq > started.seq
        && root.seq < event.seq
        && root.payload["reference"] == *reference
        && root.payload["bytes"]
            .as_u64()
            .is_some_and(|n| n <= worker::MAX_OUTPUT_BYTES as u64)
}
