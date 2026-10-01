//! `memory.search` inside a turn (ADR 0029): lexical recall over the memories
//! the turn's context compilation saw, including those the context budget left
//! out. It is local and read-only, and replay recomputes every result.
use std::collections::BTreeSet;

use ditto_capability::{
    CanonicalResource, CapabilityDeriver, CapabilityManifest, CapabilitySchema, DerivationBudget,
    DeriverError, DeriverRevision, EffectProfile, canonical_manifest_digest,
};
use ditto_context::{
    ContextExclusionReason, ContextNode, ContextOrigin, EpistemicStatus, lexical_recall,
};
use ditto_model::ProviderCallId;
use ditto_protocol::{EventActor, EventRecord, event_kind};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const ID: &str = "memory.search";
pub(crate) const VERSION: &str = "0.1.0";
/// Memories one search returns at most.
pub const MAX_RESULTS: usize = 8;
/// Memory text one search returns at most, across its results.
pub const MAX_RESULT_TEXT_BYTES: usize = 8 * 1_024;
const MAX_QUERY_CHARS: usize = 200;

pub(crate) fn manifest() -> CapabilityManifest {
    toml::from_str(include_str!(
        "../../../../capabilities/core/memory-search/capability.toml"
    ))
    .expect("the bundled memory.search manifest parses")
}

/// The installed package must be the bundled one.
pub(crate) fn validate_manifest(installed: &CapabilityManifest) -> bool {
    canonical_manifest_digest(installed) == canonical_manifest_digest(&manifest())
}

pub(crate) fn schema() -> CapabilitySchema {
    CapabilitySchema {
        id: ID.into(),
        version: VERSION.into(),
        summary: manifest().summary,
        input_schema: json!({
            "type": "object", "additionalProperties": false, "required": ["query"],
            "properties": {
                "query": {"type": "string", "minLength": 1, "maxLength": MAX_QUERY_CHARS}
            }
        }),
        output_schema: json!({
            "type": "object", "additionalProperties": false,
            "required": ["memories", "searched"],
            "properties": {
                "memories": {
                    "type": "array", "maxItems": MAX_RESULTS,
                    "items": {
                        "type": "object", "additionalProperties": false,
                        "required": ["id", "text"],
                        "properties": {"id": {"type": "string"}, "text": {"type": "string"}}
                    }
                },
                "searched": {"type": "integer", "minimum": 0}
            }
        }),
    }
}

pub(crate) struct RecallDeriver(DeriverRevision);

impl Default for RecallDeriver {
    fn default() -> Self {
        Self(DeriverRevision::new("memory-search-v1").expect("static deriver revision is valid"))
    }
}

impl CapabilityDeriver for RecallDeriver {
    fn capability_id(&self) -> &str {
        ID
    }

    fn revision(&self) -> &DeriverRevision {
        &self.0
    }

    fn normalize(
        &self,
        arguments: &Value,
        budget: &mut DerivationBudget,
    ) -> Result<Value, DeriverError> {
        budget.charge(1)?;
        normalized_query(arguments)
            .map(|query| json!({"query": query}))
            .ok_or_else(|| DeriverError::new("memory.search query is empty"))
    }

    fn derive_effect(
        &self,
        _normalized_arguments: &Value,
        budget: &mut DerivationBudget,
    ) -> Result<EffectProfile, DeriverError> {
        budget.charge(1)?;
        Ok(EffectProfile::read_content())
    }

    fn derive_resources(
        &self,
        _normalized_arguments: &Value,
        budget: &mut DerivationBudget,
    ) -> Result<BTreeSet<CanonicalResource>, DeriverError> {
        budget.charge(1)?;
        Ok(BTreeSet::new())
    }
}

/// The query of schema-valid arguments, trimmed; `None` when blank.
fn normalized_query(arguments: &Value) -> Option<String> {
    let query = arguments.get("query")?.as_str()?.trim();
    (!query.is_empty()).then(|| query.to_owned())
}

/// The replay-side normalization: the raw schema, then the trimmed query.
pub(crate) fn normalize_call(arguments: &Value) -> Option<String> {
    ditto_capability::validate_invocation_instance(&schema().input_schema, arguments).ok()?;
    normalized_query(arguments)
}

/// What a search may read: the nodes the turn compiled, and those it left out
/// only as irrelevant or over budget. Invalid, disputed and expired nodes stay
/// out.
pub(crate) fn searchable(reason: &ContextExclusionReason) -> bool {
    matches!(
        reason,
        ContextExclusionReason::Irrelevant | ContextExclusionReason::TokenBudget
    )
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecalledMemory {
    pub id: String,
    pub text: String,
    /// Version 10: Ditto inferred it from the conversation (ADR 0031).
    #[serde(default, skip_serializing_if = "is_false")]
    pub inferred: bool,
}

fn is_false(value: &bool) -> bool {
    !value
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case", deny_unknown_fields)]
pub enum RecallToolResult {
    Found {
        memories: Vec<RecalledMemory>,
        searched: u32,
    },
    InvalidArguments,
}

impl RecallToolResult {
    /// The best matches for `query` among the memories in `space`, bounded in
    /// count and text: the user's own assertions and what Ditto inferred
    /// (ADR 0031), marked as such. Nothing derived by another origin may
    /// appear, since the model reads the result as the user's memories.
    pub(crate) fn search(query: &str, space: &[ContextNode]) -> Self {
        let asserted = space.iter().filter(|node| {
            matches!(
                (node.origin, node.epistemic),
                (ContextOrigin::User, EpistemicStatus::Asserted)
                    | (ContextOrigin::Model, EpistemicStatus::Inferred)
            )
        });
        let searched = asserted.clone().count();
        let mut memories = Vec::new();
        let mut bytes = 0;
        for node in lexical_recall(query, asserted)
            .into_iter()
            .take(MAX_RESULTS)
        {
            if bytes + node.summary.len() > MAX_RESULT_TEXT_BYTES {
                break;
            }
            bytes += node.summary.len();
            memories.push(RecalledMemory {
                id: node.id.clone(),
                text: node.summary.clone(),
                inferred: node.origin == ContextOrigin::Model,
            });
        }
        Self::Found {
            memories,
            searched: u32::try_from(searched).unwrap_or(u32::MAX),
        }
    }

    /// What the model reads.
    pub(crate) fn model_value(&self) -> Value {
        match self {
            Self::Found { memories, searched } => json!({
                "memories": memories,
                "searched": searched,
                "content_origin": "memories Ditto keeps for the user: what they asked Ditto to remember, and what Ditto inferred (marked inferred)",
            }),
            Self::InvalidArguments => json!({"error": "invalid_arguments"}),
        }
    }

    pub(crate) fn is_error(&self) -> bool {
        matches!(self, Self::InvalidArguments)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecallToolRequested {
    pub event_version: u16,
    pub turn_id: String,
    pub request_index: u8,
    pub call_id: ProviderCallId,
    pub arguments: Value,
    pub query: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecallToolOutput {
    pub event_version: u16,
    pub turn_id: String,
    pub request_index: u8,
    pub call_id: ProviderCallId,
    pub result: RecallToolResult,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplayedRecallCall {
    pub requested: RecallToolRequested,
    pub output: RecallToolOutput,
}

fn same_scope(event: &EventRecord, input: &EventRecord) -> bool {
    event.session_id == input.session_id
        && event.task_id == input.task_id
        && event.correlation_id == input.correlation_id
        && event.seq > input.seq
}

pub(crate) fn valid_requested(
    request: &RecallToolRequested,
    event: &EventRecord,
    input: &EventRecord,
) -> bool {
    request.event_version == 1
        && request.turn_id == input.correlation_id.as_deref().unwrap_or_default()
        && request.query == normalize_call(&request.arguments)
        && event.kind == event_kind::AGENT_MEMORY_REQUESTED
        && event.actor == EventActor::Model
        && event.span_id.as_deref() == Some(request.call_id.as_str())
        && same_scope(event, input)
}

/// The recorded result must be the search replay recomputes.
pub(crate) fn valid_output(
    output: &RecallToolOutput,
    event: &EventRecord,
    request: &RecallToolRequested,
    requested: &EventRecord,
    input: &EventRecord,
    space: &[ContextNode],
) -> bool {
    let expected = match &request.query {
        Some(query) => RecallToolResult::search(query, space),
        None => RecallToolResult::InvalidArguments,
    };
    output.event_version == 1
        && output.turn_id == request.turn_id
        && output.request_index == request.request_index
        && output.call_id == request.call_id
        && output.result == expected
        && event.kind == event_kind::AGENT_MEMORY_OUTPUT
        && event.actor == EventActor::Capability
        && event.span_id.as_deref() == Some(output.call_id.as_str())
        && event.causation_id.as_deref() == Some(&requested.event_id)
        && event.seq > requested.seq
        && same_scope(event, input)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ditto_context::{ContextLens, ContextNodeKind, ContextScope};

    fn node(
        id: &str,
        summary: &str,
        origin: ContextOrigin,
        epistemic: EpistemicStatus,
    ) -> ContextNode {
        ContextNode {
            id: id.into(),
            kind: ContextNodeKind::Claim,
            summary: summary.into(),
            origin,
            epistemic,
            scope: ContextScope::Session,
            lens: ContextLens::Personal,
            confidence: 1.0,
            source_event_ids: vec!["source".into()],
            supersedes: Vec::new(),
            valid_from: None,
            valid_until: None,
        }
    }

    fn ids(result: &RecallToolResult) -> Vec<&str> {
        match result {
            RecallToolResult::Found { memories, .. } => {
                memories.iter().map(|memory| memory.id.as_str()).collect()
            }
            RecallToolResult::InvalidArguments => Vec::new(),
        }
    }

    #[test]
    fn only_memories_are_searched() {
        use ContextOrigin::*;
        use EpistemicStatus::*;
        let space = [
            node("said", "my dog Miso", User, Asserted),
            node("inferred", "my dog Rex", User, Inferred),
            node("verified", "my dog Bo", User, Verified),
            node("model", "my dog Max", Model, Inferred),
            node("tool", "my dog Ace", Capability, Verified),
            node("policy", "my dog Kit", Policy, Verified),
            node("system", "my dog Leo", System, Asserted),
        ];
        // The user's own assertions rank above what Ditto inferred.
        let result = RecallToolResult::search("dog", &space);
        assert_eq!(ids(&result), ["said", "model"]);
        assert!(matches!(
            result,
            RecallToolResult::Found { searched: 2, .. }
        ));
    }

    #[test]
    fn results_are_bounded_in_count_and_text() {
        let many = (0..20)
            .map(|index| {
                node(
                    &format!("m{index:02}"),
                    "dog",
                    ContextOrigin::User,
                    EpistemicStatus::Asserted,
                )
            })
            .collect::<Vec<_>>();
        let result = RecallToolResult::search("dog", &many);
        assert_eq!(ids(&result).len(), MAX_RESULTS);
        assert_eq!(ids(&result)[0], "m00");
        assert!(matches!(
            result,
            RecallToolResult::Found { searched: 20, .. }
        ));

        // Best first: the text bound keeps a prefix of the ranking.
        let long = "dog ".repeat(MAX_RESULT_TEXT_BYTES / 4 / 3 + 1);
        let large = (0..4)
            .map(|index| {
                node(
                    &format!("l{index}"),
                    &long,
                    ContextOrigin::User,
                    EpistemicStatus::Asserted,
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(ids(&RecallToolResult::search("dog", &large)), ["l0", "l1"]);
    }
}
