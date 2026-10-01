//! `memory.manage` (ADR 0036): one tool to search the memories Ditto keeps
//! for the user (ADR 0029) and to remember or forget them on its own
//! (ADR 0031). A search is local and read-only, and replay recomputes it. A
//! write records what Ditto chose in a session-scoped, task-free
//! `memory.written` event, which sources the context node, so the memory
//! outlives the run; every refusal follows one rule order that runtime and
//! replay share.
use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use ditto_capability::{
    CanonicalResource, CapabilityDeriver, CapabilityManifest, CapabilitySchema, DataAccess,
    DerivationBudget, DeriverError, DeriverRevision, EffectProfile, Externality, Mutation,
    Privilege, canonical_manifest_digest,
};
use ditto_context::{
    ContextExclusionReason, ContextLens, ContextNode, ContextNodeKind, ContextOrigin, ContextScope,
    EpistemicStatus, lexical_recall,
};
use ditto_model::ProviderCallId;
use ditto_protocol::{EventActor, EventRecord, event_kind};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::tool::{ToolOutput, ToolRequested};

pub const ID: &str = "memory.manage";
pub(crate) const VERSION: &str = "0.1.0";
/// Memories one search returns at most.
pub const MAX_RESULTS: usize = 8;
/// Memory text one search returns at most, across its results.
pub const MAX_RESULT_TEXT_BYTES: usize = 8 * 1_024;
const MAX_QUERY_CHARS: usize = 200;
/// Characters one remembered fact may hold.
pub const MAX_MEMORY_TEXT_CHARS: usize = 500;
/// Memory writes one turn may make.
pub const MAX_MEMORY_WRITES: u32 = 3;
/// The lease that bounds a turn's memory writes.
pub(crate) const LEASE_ID: &str = "agent-memory";
/// A memory Ditto inferred is likely, not asserted by the user.
const INFERRED_CONFIDENCE: f32 = 0.8;
const MEMORY_ID_PATTERN: &str = "^memory-[0-9a-z]{26}$";
/// The text of the disputed node that makes a memory forgotten.
const FORGOTTEN_SUMMARY: &str = "forgotten";

pub(crate) fn manifest() -> CapabilityManifest {
    toml::from_str(include_str!(
        "../../../../capabilities/core/memory-manage/capability.toml"
    ))
    .expect("the bundled memory.manage manifest parses")
}

/// The installed package must be the bundled one.
pub(crate) fn validate_manifest(installed: &CapabilityManifest) -> bool {
    canonical_manifest_digest(installed) == canonical_manifest_digest(&manifest())
}

/// A write: a local, reversible change to the session's memories that reads
/// nothing.
pub(crate) fn write_effect() -> EffectProfile {
    EffectProfile {
        access: DataAccess::None,
        mutation: Mutation::Reversible,
        externality: Externality::Local,
        privilege: Privilege::User,
    }
}

pub(crate) fn schema() -> CapabilitySchema {
    let memory_id = json!({"type": "string", "pattern": MEMORY_ID_PATTERN});
    CapabilitySchema {
        id: ID.into(),
        version: VERSION.into(),
        summary: "The user's memories: search them by query for facts missing from DITTO_CONTEXT_V1, remember one short lasting fact as text (replaces: the ID of the memory it updates), or forget the memory with memory_id.".into(),
        input_schema: json!({
            "type": "object", "additionalProperties": false, "required": ["action"],
            "properties": {
                "action": {"type": "string", "enum": ["search", "remember", "forget"]},
                "query": {"type": "string", "minLength": 1, "maxLength": MAX_QUERY_CHARS},
                "text": {"type": "string", "minLength": 1, "maxLength": MAX_MEMORY_TEXT_CHARS},
                "replaces": memory_id,
                "memory_id": memory_id
            }
        }),
        output_schema: json!({"type": "object"}),
    }
}

pub(crate) fn deriver_revision() -> DeriverRevision {
    DeriverRevision::new("memory-manage-v1").expect("static deriver revision is valid")
}

/// What one call asks for, normalized: a search query or a fact trimmed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum MemoryAction {
    Search {
        query: String,
    },
    Remember {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        replaces: Option<String>,
    },
    Forget {
        memory_id: String,
    },
}

impl MemoryAction {
    /// The action schema-valid arguments ask for, from the fields it takes;
    /// `None` when one is missing or the query or fact is blank.
    pub(crate) fn from_arguments(arguments: &Value) -> Option<Self> {
        let field = |name: &str| arguments.get(name).and_then(Value::as_str);
        let nonblank = |name: &str| {
            let value = field(name)?.trim();
            (!value.is_empty()).then(|| value.to_owned())
        };
        match field("action")? {
            "search" => Some(Self::Search {
                query: nonblank("query")?,
            }),
            "remember" => Some(Self::Remember {
                text: nonblank("text")?,
                replaces: field("replaces").map(str::to_owned),
            }),
            "forget" => Some(Self::Forget {
                memory_id: field("memory_id")?.to_owned(),
            }),
            _ => None,
        }
    }

    /// A remember or forget, as the write it records.
    pub(crate) fn write(&self) -> Option<MemoryWrite> {
        match self {
            Self::Search { .. } => None,
            Self::Remember { text, replaces } => Some(MemoryWrite::Remember {
                text: text.clone(),
                replaces: replaces.clone(),
            }),
            Self::Forget { memory_id } => Some(MemoryWrite::Forget {
                memory_id: memory_id.clone(),
            }),
        }
    }
}

/// Replay's normalization: the raw schema, then the action.
pub(crate) fn normalize_call(arguments: &Value) -> Option<MemoryAction> {
    ditto_capability::validate_invocation_instance(&schema().input_schema, arguments).ok()?;
    MemoryAction::from_arguments(arguments)
}

pub(crate) struct MemoryDeriver(DeriverRevision);

impl Default for MemoryDeriver {
    fn default() -> Self {
        Self(deriver_revision())
    }
}

impl CapabilityDeriver for MemoryDeriver {
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
        MemoryAction::from_arguments(arguments)
            .map(|action| json!(action))
            .ok_or_else(|| DeriverError::new("memory.manage call is incomplete"))
    }

    fn derive_effect(
        &self,
        normalized_arguments: &Value,
        budget: &mut DerivationBudget,
    ) -> Result<EffectProfile, DeriverError> {
        budget.charge(1)?;
        Ok(if normalized_arguments["action"] == "search" {
            EffectProfile::read_content()
        } else {
            write_effect()
        })
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
    /// Ditto inferred it from the conversation (ADR 0031).
    #[serde(default, skip_serializing_if = "is_false")]
    pub inferred: bool,
}

fn is_false(value: &bool) -> bool {
    !value
}

/// A write as `memory.written` records it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum MemoryWrite {
    Remember {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        replaces: Option<String>,
    },
    Forget {
        memory_id: String,
    },
}

impl MemoryWrite {
    /// The memory it replaces or forgets.
    pub(crate) fn target(&self) -> Option<&str> {
        match self {
            Self::Remember { replaces, .. } => replaces.as_deref(),
            Self::Forget { memory_id } => Some(memory_id),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryRefusal {
    InvalidArguments,
    /// The turn already read a web page or file, whose text may carry
    /// instructions that must not become memories.
    UntrustedContentRead,
    /// The fact looks like a password, key or token.
    Credential,
    /// The memory to replace or forget is not an active memory of the session.
    MemoryUnavailable,
    LimitReached,
}

/// The first rule a call breaks, in the order runtime and replay share.
/// `active` tells whether an ID names an active memory of the session.
pub(crate) fn refusal(
    action: Option<&MemoryAction>,
    read_external_content: bool,
    writes: u32,
    active: impl FnOnce(&str) -> bool,
) -> Option<MemoryRefusal> {
    let Some(action) = action else {
        return Some(MemoryRefusal::InvalidArguments);
    };
    // A search breaks no write rule.
    let write = action.write()?;
    if read_external_content {
        return Some(MemoryRefusal::UntrustedContentRead);
    }
    if let MemoryWrite::Remember { text, .. } = &write
        && credential_like(text)
    {
        return Some(MemoryRefusal::Credential);
    }
    if let Some(target) = write.target()
        && !active(target)
    {
        return Some(MemoryRefusal::MemoryUnavailable);
    }
    (writes >= MAX_MEMORY_WRITES).then_some(MemoryRefusal::LimitReached)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case", deny_unknown_fields)]
pub enum MemoryResult {
    Found {
        memories: Vec<RecalledMemory>,
        searched: u32,
    },
    Remembered {
        memory_id: String,
    },
    Forgotten {
        memory_id: String,
    },
    Refused {
        code: MemoryRefusal,
    },
}

impl MemoryResult {
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

    pub(crate) fn written(write: &MemoryWrite, memory_id: String) -> Self {
        match write {
            MemoryWrite::Remember { .. } => Self::Remembered { memory_id },
            MemoryWrite::Forget { .. } => Self::Forgotten { memory_id },
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
            Self::Remembered { memory_id } => json!({"remembered": memory_id}),
            Self::Forgotten { memory_id } => json!({"forgotten": memory_id}),
            Self::Refused { code } => json!({"error": code}),
        }
    }

    pub(crate) fn is_error(&self) -> bool {
        matches!(self, Self::Refused { .. })
    }

    fn memory_id(&self) -> Option<&str> {
        match self {
            Self::Remembered { memory_id } | Self::Forgotten { memory_id } => Some(memory_id),
            Self::Found { .. } | Self::Refused { .. } => None,
        }
    }
}

/// The session-scoped record of a write, which sources its context node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryWrittenPayload {
    pub event_version: u16,
    pub turn_id: String,
    pub call_id: ProviderCallId,
    pub write: MemoryWrite,
}

/// The context node a `memory.written` event sources: an inferred memory,
/// or a disputed node that supersedes the forgotten one so neither is active.
pub(crate) fn memory_node(source_event_id: &str, write: &MemoryWrite) -> ContextNode {
    match write {
        MemoryWrite::Remember { text, replaces } => ContextNode {
            summary: text.clone(),
            epistemic: EpistemicStatus::Inferred,
            confidence: INFERRED_CONFIDENCE,
            supersedes: replaces.iter().cloned().collect(),
            ..sourced_node(source_event_id, ContextOrigin::Model)
        },
        MemoryWrite::Forget { memory_id } => {
            forgotten_node(source_event_id, memory_id, ContextOrigin::Model)
        }
    }
}

/// The disputed node that forgets `memory_id`, attested by the model
/// (ADR 0031) or the user (ADR 0032) through its source event.
pub(crate) fn forgotten_node(
    source_event_id: &str,
    memory_id: &str,
    origin: ContextOrigin,
) -> ContextNode {
    ContextNode {
        summary: FORGOTTEN_SUMMARY.to_owned(),
        epistemic: EpistemicStatus::Disputed,
        confidence: 0.0,
        supersedes: vec![memory_id.to_owned()],
        ..sourced_node(source_event_id, origin)
    }
}

/// The fields every memory node shares: its ID and provenance from one
/// session-scoped source event.
fn sourced_node(source_event_id: &str, origin: ContextOrigin) -> ContextNode {
    ContextNode {
        id: format!("memory-{}", source_event_id.to_ascii_lowercase()),
        kind: ContextNodeKind::Claim,
        summary: String::new(),
        origin,
        epistemic: EpistemicStatus::Inferred,
        scope: ContextScope::Session,
        lens: ContextLens::Personal,
        confidence: 0.0,
        source_event_ids: vec![source_event_id.to_owned()],
        supersedes: Vec::new(),
        valid_from: None,
        valid_until: None,
    }
}

/// A memory: the user's own (ADR 0015) or one Ditto inferred.
pub(crate) fn is_memory(node: &ContextNode) -> bool {
    node.id.len() == 33
        && node.id.starts_with("memory-")
        && node.scope == ContextScope::Session
        && matches!(
            (node.origin, node.epistemic),
            (ContextOrigin::User, EpistemicStatus::Asserted)
                | (ContextOrigin::Model, EpistemicStatus::Inferred)
        )
}

/// Whether `id` names an active memory of the session among the context
/// nodes recorded before `before_seq`: recorded, not superseded, and valid at
/// `at`. Replay's form of the projection's active set.
pub(crate) fn active_memory_in(
    snapshot: &[EventRecord],
    session: &str,
    id: &str,
    before_seq: i64,
    at: DateTime<Utc>,
) -> bool {
    let mut found = None;
    for event in snapshot.iter().filter(|event| {
        event.kind == event_kind::CONTEXT_NODE_RECORDED
            && event.seq < before_seq
            && event.session_id.as_deref() == Some(session)
            && event.task_id.is_none()
    }) {
        let Some(node) = event
            .payload
            .get("node")
            .and_then(|node| serde_json::from_value::<ContextNode>(node.clone()).ok())
        else {
            continue;
        };
        if node.supersedes.iter().any(|superseded| superseded == id) {
            return false;
        }
        if node.id == id && found.is_none() {
            found = Some(node);
        }
    }
    found.is_some_and(|node| is_memory(&node) && node.is_valid_at(at))
}

/// Best effort: a fact that looks like it holds a secret. The model is told
/// never to save secrets; this catches the common shapes.
pub(crate) fn credential_like(text: &str) -> bool {
    let lower = text.to_lowercase();
    const KEYWORDS: [&str; 13] = [
        "password",
        "passcode",
        "passwd",
        "passphrase",
        "api key",
        "api_key",
        "secret key",
        "access token",
        "auth token",
        "pin code",
        "비밀번호",
        "패스워드",
        "암호",
    ];
    const MARKERS: [&str; 6] = [":", "=", "is ", "는", "은", "이 "];
    let assigned = KEYWORDS.iter().any(|keyword| {
        lower.match_indices(keyword).any(|(index, _)| {
            let rest = lower[index + keyword.len()..].trim_start();
            MARKERS.iter().any(|marker| {
                rest.strip_prefix(marker)
                    .is_some_and(|value| !value.trim().is_empty())
            })
        })
    });
    let token = |prefix: &str, minimum: usize, allowed: fn(char) -> bool| {
        text.match_indices(prefix).any(|(index, _)| {
            text[index + prefix.len()..]
                .chars()
                .take_while(|character| allowed(*character))
                .count()
                >= minimum
        })
    };
    let alphanumeric = |character: char| character.is_ascii_alphanumeric();
    assigned
        || (lower.contains("-----begin") && lower.contains("private key"))
        || token("sk-", 20, alphanumeric)
        || ["ghp_", "gho_", "ghu_", "ghs_", "ghr_"]
            .iter()
            .any(|prefix| token(prefix, 30, alphanumeric))
        || ["xoxb-", "xoxa-", "xoxp-", "xoxr-", "xoxs-"]
            .iter()
            .any(|prefix| {
                token(prefix, 20, |character| {
                    character.is_ascii_alphanumeric() || character == '-'
                })
            })
        || token("AKIA", 16, |character| {
            character.is_ascii_uppercase() || character.is_ascii_digit()
        })
}

/// The `memory.written` event and context node a written result names, from
/// the session snapshot, checked against the request; `None` when either is
/// missing or differs.
pub(crate) fn written_records<'a>(
    snapshot: &'a [EventRecord],
    request: &ToolRequested,
    write: &MemoryWrite,
    requested: &EventRecord,
    output: &ToolOutput,
    result: &MemoryResult,
    output_event: &EventRecord,
) -> Option<(&'a EventRecord, &'a EventRecord)> {
    let memory_id = result.memory_id()?;
    let written = snapshot.iter().find(|event| {
        event.kind == event_kind::MEMORY_WRITTEN
            && event.causation_id.as_deref() == Some(&requested.event_id)
    })?;
    let payload: MemoryWrittenPayload = serde_json::from_value(written.payload.clone()).ok()?;
    let expected_payload = MemoryWrittenPayload {
        event_version: 1,
        turn_id: request.turn_id.clone(),
        call_id: request.call_id.clone(),
        write: write.clone(),
    };
    let node = memory_node(&written.event_id, write);
    let recorded = snapshot.iter().find(|event| {
        event.kind == event_kind::CONTEXT_NODE_RECORDED
            && event.causation_id.as_deref() == Some(&written.event_id)
    })?;
    let recorded_node: ContextNode =
        serde_json::from_value(recorded.payload.get("node")?.clone()).ok()?;
    (payload == expected_payload
        && output.call_id == request.call_id
        && written.actor == EventActor::Model
        && written.session_id == requested.session_id
        && written.task_id.is_none()
        && written.seq > requested.seq
        && recorded.actor == EventActor::System
        && recorded.session_id == requested.session_id
        && recorded.task_id.is_none()
        && recorded.seq > written.seq
        && output_event.seq > recorded.seq
        && recorded_node == node
        && memory_id == node.id
        && &MemoryResult::written(write, node.id.clone()) == result)
        .then_some((written, recorded))
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn ids(result: &MemoryResult) -> Vec<&str> {
        match result {
            MemoryResult::Found { memories, .. } => {
                memories.iter().map(|memory| memory.id.as_str()).collect()
            }
            _ => Vec::new(),
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
        let result = MemoryResult::search("dog", &space);
        assert_eq!(ids(&result), ["said", "model"]);
        assert!(matches!(result, MemoryResult::Found { searched: 2, .. }));
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
        let result = MemoryResult::search("dog", &many);
        assert_eq!(ids(&result).len(), MAX_RESULTS);
        assert_eq!(ids(&result)[0], "m00");
        assert!(matches!(result, MemoryResult::Found { searched: 20, .. }));

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
        assert_eq!(ids(&MemoryResult::search("dog", &large)), ["l0", "l1"]);
    }

    #[test]
    fn credential_shapes_are_recognized_and_ordinary_facts_are_not() {
        let secrets = [
            "My wifi password is hunter2".to_owned(),
            "password: correct horse".to_owned(),
            "집 비밀번호는 1234야".to_owned(),
            "현관 암호는 0000".to_owned(),
            format!("My key is sk-{}", "a1".repeat(12)),
            format!("token gh{}_{}", "p", "x".repeat(36)),
            format!("{}{}", "AKIA", "ABCDEFGHIJKLMNOP"),
            format!("{}b-{}", "xox", "1234567890-abcdefghij"),
            format!("-----BEGIN {} PRIVATE KEY-----", "RSA"),
        ];
        for secret in &secrets {
            assert!(credential_like(secret), "{secret}");
        }
        for fact in [
            "I changed my password yesterday",
            "My dog is called Miso",
            "암호화 공부를 시작했다",
            "The user's password manager is 1Password",
            "sk-short",
        ] {
            assert!(!credential_like(fact), "{fact}");
        }
    }

    #[test]
    fn refusals_follow_one_order() {
        let remember = MemoryAction::Remember {
            text: "The user's dog is called Miso.".into(),
            replaces: Some("memory-01k00000000000000000000000".into()),
        };
        let secret = MemoryAction::Remember {
            text: "password: x".into(),
            replaces: None,
        };
        let search = MemoryAction::Search {
            query: "dog".into(),
        };
        use MemoryRefusal::*;
        assert_eq!(refusal(None, true, 9, |_| false), Some(InvalidArguments));
        // A search reads; no write rule applies to it.
        assert_eq!(refusal(Some(&search), true, 9, |_| false), None);
        assert_eq!(
            refusal(Some(&secret), true, 9, |_| false),
            Some(UntrustedContentRead)
        );
        assert_eq!(
            refusal(Some(&secret), false, 9, |_| false),
            Some(Credential)
        );
        assert_eq!(
            refusal(Some(&remember), false, 9, |_| false),
            Some(MemoryUnavailable)
        );
        assert_eq!(
            refusal(Some(&remember), false, MAX_MEMORY_WRITES, |_| true),
            Some(LimitReached)
        );
        assert_eq!(refusal(Some(&remember), false, 0, |_| true), None);
    }

    #[test]
    fn forgetting_records_a_disputed_node_that_supersedes_the_memory() {
        let forget = MemoryWrite::Forget {
            memory_id: "memory-01k00000000000000000000000".into(),
        };
        let node = memory_node("01K00000000000000000000001", &forget);
        assert_eq!(node.id, "memory-01k00000000000000000000001");
        assert_eq!(node.epistemic, EpistemicStatus::Disputed);
        assert_eq!(node.supersedes, ["memory-01k00000000000000000000000"]);
        node.validate().unwrap();
        assert!(!is_memory(&node));
        let remember = MemoryWrite::Remember {
            text: "The user's dog is called Miso.".into(),
            replaces: None,
        };
        let node = memory_node("01K00000000000000000000002", &remember);
        node.validate().unwrap();
        assert!(is_memory(&node));
    }

    #[test]
    fn calls_normalize_only_schema_valid_arguments() {
        let id = "memory-01k00000000000000000000000";
        assert_eq!(
            normalize_call(&json!({"action": "remember", "text": "  Miso is the dog.  "})),
            Some(MemoryAction::Remember {
                text: "Miso is the dog.".into(),
                replaces: None
            })
        );
        assert_eq!(
            normalize_call(&json!({"action": "search", "query": " dog "})),
            Some(MemoryAction::Search {
                query: "dog".into()
            })
        );
        assert_eq!(
            normalize_call(&json!({"action": "forget", "memory_id": id})),
            Some(MemoryAction::Forget {
                memory_id: id.into()
            })
        );
        // Fields another action takes are ignored.
        assert_eq!(
            normalize_call(&json!({"action": "search", "query": "dog", "text": ""})),
            None,
            "a blank text still breaks the schema"
        );
        assert_eq!(
            normalize_call(&json!({"action": "forget", "memory_id": id, "query": "x"})),
            Some(MemoryAction::Forget {
                memory_id: id.into()
            })
        );
        for invalid in [
            json!({"action": "remember", "text": "   "}),
            json!({"action": "remember", "text": "x".repeat(MAX_MEMORY_TEXT_CHARS + 1)}),
            json!({"action": "remember", "text": "x", "replaces": "memory-1"}),
            json!({"action": "remember"}),
            json!({"action": "search", "query": "  "}),
            json!({"action": "forget", "memory_id": "x"}),
            json!({"action": "forget", "memory_id": id, "y": 1}),
            json!({"action": "update", "text": "x"}),
            json!({"query": "x"}),
        ] {
            assert_eq!(normalize_call(&invalid), None, "{invalid}");
        }
        // The normalized form is what the deriver commits to.
        assert_eq!(
            json!(MemoryAction::Remember {
                text: "x".into(),
                replaces: Some(id.into())
            }),
            json!({"action": "remember", "text": "x", "replaces": id})
        );
    }
}
