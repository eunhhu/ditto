//! `artifact.sort` inside a turn (ADR 0018): the closed tool profile only an
//! explicitly permitted run offers, on the shared tool lifecycle (ADR 0036).
use ditto_artifact_sort::{self as worker, SortArguments};
use ditto_capability::CanonicalResource;
use ditto_model::{ContentPart, ConversationItem, MessageRole};
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

pub(crate) fn normalize_call(arguments: &Value) -> Option<SortArguments> {
    ditto_capability::validate_invocation_instance(&worker::schema().input_schema, arguments)
        .ok()?;
    serde_json::from_value(arguments.clone()).ok()
}

/// Replay's check of a recorded result. Status additionally hashes both
/// artifacts and checks the line contract; replay performs no I/O.
pub(crate) fn valid_result(
    result: &SortToolResult,
    request: Option<&SortArguments>,
    started: bool,
    grant: &SortGrant,
    already_claimed: bool,
    after_deadline: bool,
    artifact_event_id: Option<&str>,
) -> bool {
    match result {
        SortToolResult::Error { code } => {
            if artifact_event_id.is_some() {
                return false;
            }
            if started {
                return matches!(
                    code,
                    SortToolError::InputUnavailable
                        | SortToolError::Cancelled
                        | SortToolError::ProcessDeadline
                        | SortToolError::ProcessFailed
                        | SortToolError::VerificationFailed
                );
            }
            match request {
                None => *code == SortToolError::InvalidArguments,
                Some(args) if !grant.permits(args) => *code == SortToolError::PermissionDenied,
                Some(_) => {
                    (already_claimed && *code == SortToolError::LeaseExhausted)
                        || (*code == SortToolError::LeaseExpired && after_deadline)
                }
            }
        }
        SortToolResult::Verified {
            reference,
            verifier,
            input_lines,
            output_lines,
        } => {
            started
                && artifact_event_id.is_some()
                && CanonicalResource::artifact(reference).is_ok()
                && verifier == worker::VERIFIER
                && *input_lines <= worker::MAX_LINES
                && *output_lines <= *input_lines
        }
    }
}

/// The verified artifact a sort's output names: created by the sort's start,
/// before its output.
pub(crate) fn valid_output_root(
    root: &EventRecord,
    reference: &str,
    artifact_event_id: Option<&str>,
    event: &EventRecord,
    started: &EventRecord,
) -> bool {
    Some(root.event_id.as_str()) == artifact_event_id
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
