//! `artifact.sort` inside a turn (ADR 0018): the closed tool profile only an
//! explicitly permitted run offers, on the shared tool lifecycle (ADR 0036).
use ditto_artifact_sort::{self as worker, SortArguments};
use ditto_capability::CanonicalResource;
use ditto_context::{
    ContextLens, ContextNode, ContextNodeKind, ContextOrigin, ContextScope, EpistemicStatus,
};
use ditto_model::{ContentPart, ConversationItem, MessageRole};
use ditto_protocol::{AgentSortPermission, EventActor, EventRecord, event_kind};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// The call id of the sort a turn runs before its first model request.
pub(crate) fn prefetch_call_id(turn_id: &str) -> String {
    format!("prefetch_sort_{turn_id}")
}

/// Id prefix of the node holding a prefetched sort (ADR 0041), after the
/// attachment's own `prefetch_attachment_` so both sort canonically.
pub(crate) const PREFETCH_SORT_NODE_PREFIX: &str = "prefetch_sort_";

/// The context node a verified prefetched sort becomes: the first lines of its
/// order, rendered only from the recorded result so replay compares it with
/// the node the compilation holds.
pub(crate) fn prefetch_node(
    grant: &SortGrant,
    input_seq: i64,
    result: &SortToolResult,
) -> Option<ContextNode> {
    let SortToolResult::Verified {
        reference,
        verifier,
        input_lines,
        output_lines,
        lines,
    } = result
    else {
        return None;
    };
    if lines.is_empty() {
        return None;
    }
    let shown = if lines.len() == *output_lines {
        format!("all {output_lines} lines in order")
    } else {
        format!("the first {} of {output_lines} lines in order", lines.len())
    };
    Some(ContextNode {
        id: format!("{PREFETCH_SORT_NODE_PREFIX}{input_seq}"),
        kind: ContextNodeKind::Resource,
        summary: format!(
            "the attachment, sorted at the start of the turn under the user's permission \
             and checked by {verifier} ({input_lines} lines in, {output_lines} out, \
             sorted artifact {reference}); {shown}:\n{}",
            lines.join("\n")
        ),
        origin: ContextOrigin::System,
        epistemic: EpistemicStatus::Verified,
        scope: ContextScope::Session,
        lens: ContextLens::Task,
        confidence: 1.0,
        source_event_ids: vec![grant.source_event_id.clone()],
        supersedes: Vec::new(),
        valid_from: None,
        valid_until: None,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SortGrant {
    pub reference: String,
    pub source_event_id: String,
    pub allow_deduplicate: bool,
    #[serde(default)]
    pub deduplicate: bool,
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

    /// The one sort the permission itself fixes (ADR 0041). Deduplicating is
    /// something the user's request decides and only the model reads it, so a
    /// permission that allows it fixes nothing and nothing is prefetched.
    /// The sort the permission fixes, or none when the choice is the model's.
    ///
    /// A permission that forbids deduplication fixes `unique: false`; one whose
    /// owner explicitly asked for it fixes `unique: true`. Permission to
    /// deduplicate without that request leaves the choice open, and the harness
    /// does not spend the one-shot lease on a guess.
    pub fn prefetch_arguments(&self) -> Option<SortArguments> {
        if self.allow_deduplicate && !self.deduplicate {
            return None;
        }
        Some(SortArguments {
            reference: self.reference.clone(),
            unique: self.deduplicate,
        })
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
                && grant.deduplicate == permission.deduplicate
        }
        _ => false,
    }
}

pub(super) fn initial_conversation(
    text: String,
    grant: Option<&SortGrant>,
    prefetched: Option<&str>,
) -> Vec<ConversationItem> {
    let mut content = vec![ContentPart::Text { text }];
    if let Some(grant) = grant {
        // When the turn already ran the sort, the ordered lines are in the
        // context and the lease is spent: say so, or the model spends a request
        // asking for a sort it is about to be refused.
        let permission = match prefetched {
            Some(node) => json!({"capability":"artifact.sort", "maximum_calls":1, "calls_used":1,
                "allow_deduplicate":grant.allow_deduplicate, "deduplicate":grant.deduplicate,
                "expires":"end of this bounded turn",
                "already_ran":true, "result_in_context":node,
                "note":"this sort already ran at the start of the turn; its ordered lines are in the                        context under that id, and calling artifact.sort again is refused"}),
            None => json!({"capability":"artifact.sort", "maximum_calls":1,
                "allow_deduplicate":grant.allow_deduplicate, "deduplicate":grant.deduplicate,
                "expires":"end of this bounded turn"}),
        };
        content.push(ContentPart::Structured {
            value: json!({
                "attachment": {"reference":grant.reference},
                "user_permission": permission
            }),
        });
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

/// Lines a sort hands back with its result. Small enough to ride in the tool
/// output, so a model that has already read the input does not spend another
/// request reading back what it just sorted.
pub const MAX_RESULT_LINES: usize = 8;
pub const MAX_RESULT_LINE_CHARS: usize = 120;

/// The first lines of a sort's output, each cut to `MAX_RESULT_LINE_CHARS`.
pub(super) fn result_lines(bytes: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(bytes)
        .lines()
        .take(MAX_RESULT_LINES)
        .map(|line| line.chars().take(MAX_RESULT_LINE_CHARS).collect())
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case", deny_unknown_fields)]
pub enum SortToolResult {
    Verified {
        reference: String,
        verifier: String,
        input_lines: usize,
        output_lines: usize,
        /// The first lines of the sorted output, so a model that already read
        /// the input can report the order without reading the file again. Empty
        /// in journals recorded before turn contract 16.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        lines: Vec<String>,
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
                lines,
            } => {
                let mut value = json!({
                    "reference":reference,"verifier":verifier,"input_lines":input_lines,"output_lines":output_lines
                });
                if !lines.is_empty() {
                    value["lines"] = json!(lines);
                }
                value
            }
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
            lines,
        } => {
            started
                && artifact_event_id.is_some()
                && CanonicalResource::artifact(reference).is_ok()
                && verifier == worker::VERIFIER
                && *input_lines <= worker::MAX_LINES
                && *output_lines <= *input_lines
                && lines.len() <= (*output_lines).min(MAX_RESULT_LINES)
                && lines
                    .iter()
                    .all(|line| line.chars().count() <= MAX_RESULT_LINE_CHARS)
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
