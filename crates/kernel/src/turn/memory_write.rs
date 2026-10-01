//! `memory.remember` and `memory.forget` (ADR 0031): Ditto keeps the user's
//! memories current on its own. A write records what Ditto chose in a
//! session-scoped, task-free `memory.written` event, which sources the
//! context node, so the memory outlives the run. Every refusal follows one
//! rule order that runtime and replay share.
use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use ditto_capability::{
    CanonicalResource, CapabilityDeriver, CapabilityManifest, CapabilitySchema, DataAccess,
    DerivationBudget, DeriverError, DeriverRevision, EffectProfile, Externality, Mutation,
    Privilege, canonical_manifest_digest,
};
use ditto_context::{
    ContextLens, ContextNode, ContextNodeKind, ContextOrigin, ContextScope, EpistemicStatus,
};
use ditto_model::ProviderCallId;
use ditto_protocol::{EventActor, EventRecord, event_kind};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const REMEMBER_ID: &str = "memory.remember";
pub const FORGET_ID: &str = "memory.forget";
pub(crate) const VERSION: &str = "0.1.0";
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

pub(crate) fn remember_manifest() -> CapabilityManifest {
    toml::from_str(include_str!(
        "../../../../capabilities/core/memory-remember/capability.toml"
    ))
    .expect("the bundled memory.remember manifest parses")
}

pub(crate) fn forget_manifest() -> CapabilityManifest {
    toml::from_str(include_str!(
        "../../../../capabilities/core/memory-forget/capability.toml"
    ))
    .expect("the bundled memory.forget manifest parses")
}

/// The installed package must be the bundled one.
pub(crate) fn validate_manifest(installed: &CapabilityManifest) -> bool {
    let bundled = match installed.id.as_str() {
        REMEMBER_ID => remember_manifest(),
        FORGET_ID => forget_manifest(),
        _ => return false,
    };
    canonical_manifest_digest(installed) == canonical_manifest_digest(&bundled)
}

/// A local, reversible change to the session's memories that reads nothing.
pub(crate) fn effect() -> EffectProfile {
    EffectProfile {
        access: DataAccess::None,
        mutation: Mutation::Reversible,
        externality: Externality::Local,
        privilege: Privilege::User,
    }
}

pub(crate) fn schema(capability_id: &str) -> CapabilitySchema {
    let memory_id = json!({"type": "string", "pattern": MEMORY_ID_PATTERN});
    let (manifest, input_schema, output_key) = if capability_id == FORGET_ID {
        (
            forget_manifest(),
            json!({
                "type": "object", "additionalProperties": false, "required": ["memory_id"],
                "properties": {"memory_id": memory_id}
            }),
            "forgotten",
        )
    } else {
        (
            remember_manifest(),
            json!({
                "type": "object", "additionalProperties": false, "required": ["text"],
                "properties": {
                    "text": {"type": "string", "minLength": 1, "maxLength": MAX_MEMORY_TEXT_CHARS},
                    "replaces": memory_id
                }
            }),
            "remembered",
        )
    };
    CapabilitySchema {
        id: manifest.id,
        version: VERSION.into(),
        summary: manifest.summary,
        input_schema,
        output_schema: json!({
            "type": "object", "additionalProperties": false, "required": [output_key],
            "properties": {output_key: {"type": "string"}}
        }),
    }
}

/// What a write asks for, normalized: a remembered fact is trimmed.
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

    /// Schema-valid arguments as a write; `None` when the fact is blank.
    fn from_arguments(capability_id: &str, arguments: &Value) -> Option<Self> {
        let field = |name: &str| arguments.get(name).and_then(Value::as_str);
        match capability_id {
            REMEMBER_ID => {
                let text = field("text")?.trim();
                (!text.is_empty()).then(|| Self::Remember {
                    text: text.to_owned(),
                    replaces: field("replaces").map(str::to_owned),
                })
            }
            FORGET_ID => Some(Self::Forget {
                memory_id: field("memory_id")?.to_owned(),
            }),
            _ => None,
        }
    }

    /// The normalized arguments an invocation commits to.
    fn arguments(&self) -> Value {
        match self {
            Self::Remember {
                text,
                replaces: None,
            } => json!({"text": text}),
            Self::Remember {
                text,
                replaces: Some(replaces),
            } => json!({"text": text, "replaces": replaces}),
            Self::Forget { memory_id } => json!({"memory_id": memory_id}),
        }
    }
}

/// The replay-side normalization: the raw schema, then the write.
pub(crate) fn normalize_call(capability_id: &str, arguments: &Value) -> Option<MemoryWrite> {
    if capability_id != REMEMBER_ID && capability_id != FORGET_ID {
        return None;
    }
    ditto_capability::validate_invocation_instance(&schema(capability_id).input_schema, arguments)
        .ok()?;
    MemoryWrite::from_arguments(capability_id, arguments)
}

/// The write a normalized invocation carries.
pub(crate) fn write_from_normalized(
    capability_id: &str,
    normalized: &Value,
) -> Option<MemoryWrite> {
    MemoryWrite::from_arguments(capability_id, normalized)
}

pub(crate) struct MemoryWriteDeriver {
    capability_id: &'static str,
    revision: DeriverRevision,
}

impl MemoryWriteDeriver {
    pub(crate) fn for_capability(capability_id: &str) -> Self {
        let (capability_id, revision) = if capability_id == FORGET_ID {
            (FORGET_ID, "memory-forget-v1")
        } else {
            (REMEMBER_ID, "memory-remember-v1")
        };
        Self {
            capability_id,
            revision: DeriverRevision::new(revision).expect("static deriver revision is valid"),
        }
    }
}

impl CapabilityDeriver for MemoryWriteDeriver {
    fn capability_id(&self) -> &str {
        self.capability_id
    }

    fn revision(&self) -> &DeriverRevision {
        &self.revision
    }

    fn normalize(
        &self,
        arguments: &Value,
        budget: &mut DerivationBudget,
    ) -> Result<Value, DeriverError> {
        budget.charge(1)?;
        MemoryWrite::from_arguments(self.capability_id, arguments)
            .map(|write| write.arguments())
            .ok_or_else(|| DeriverError::new("memory fact is empty"))
    }

    fn derive_effect(
        &self,
        _normalized_arguments: &Value,
        budget: &mut DerivationBudget,
    ) -> Result<EffectProfile, DeriverError> {
        budget.charge(1)?;
        Ok(effect())
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryWriteRefusal {
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

/// The first rule a write breaks, in the order runtime and replay share.
/// `active` tells whether an ID names an active memory of the session.
pub(crate) fn refusal(
    write: Option<&MemoryWrite>,
    read_external_content: bool,
    writes: u32,
    active: impl FnOnce(&str) -> bool,
) -> Option<MemoryWriteRefusal> {
    let Some(write) = write else {
        return Some(MemoryWriteRefusal::InvalidArguments);
    };
    if read_external_content {
        return Some(MemoryWriteRefusal::UntrustedContentRead);
    }
    if let MemoryWrite::Remember { text, .. } = write
        && credential_like(text)
    {
        return Some(MemoryWriteRefusal::Credential);
    }
    if let Some(target) = write.target()
        && !active(target)
    {
        return Some(MemoryWriteRefusal::MemoryUnavailable);
    }
    (writes >= MAX_MEMORY_WRITES).then_some(MemoryWriteRefusal::LimitReached)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case", deny_unknown_fields)]
pub enum MemoryWriteResult {
    Remembered { memory_id: String },
    Forgotten { memory_id: String },
    Refused { code: MemoryWriteRefusal },
}

impl MemoryWriteResult {
    pub(crate) fn written(write: &MemoryWrite, memory_id: String) -> Self {
        match write {
            MemoryWrite::Remember { .. } => Self::Remembered { memory_id },
            MemoryWrite::Forget { .. } => Self::Forgotten { memory_id },
        }
    }

    /// What the model reads.
    pub(crate) fn model_value(&self) -> Value {
        match self {
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
            Self::Refused { .. } => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryWriteRequested {
    pub event_version: u16,
    pub turn_id: String,
    pub request_index: u8,
    pub call_id: ProviderCallId,
    pub capability_id: String,
    pub arguments: Value,
    pub write: Option<MemoryWrite>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryWriteOutput {
    pub event_version: u16,
    pub turn_id: String,
    pub request_index: u8,
    pub call_id: ProviderCallId,
    pub result: MemoryWriteResult,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplayedMemoryWrite {
    pub requested: MemoryWriteRequested,
    pub output: MemoryWriteOutput,
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

fn same_scope(event: &EventRecord, input: &EventRecord) -> bool {
    event.session_id == input.session_id
        && event.task_id == input.task_id
        && event.correlation_id == input.correlation_id
        && event.seq > input.seq
}

pub(crate) fn valid_requested(
    request: &MemoryWriteRequested,
    event: &EventRecord,
    input: &EventRecord,
) -> bool {
    request.event_version == 1
        && request.turn_id == input.correlation_id.as_deref().unwrap_or_default()
        && request.write == normalize_call(&request.capability_id, &request.arguments)
        && event.kind == event_kind::AGENT_MEMORY_WRITE_REQUESTED
        && event.actor == EventActor::Model
        && event.span_id.as_deref() == Some(request.call_id.as_str())
        && same_scope(event, input)
}

pub(crate) fn valid_output(
    output: &MemoryWriteOutput,
    event: &EventRecord,
    request: &MemoryWriteRequested,
    requested: &EventRecord,
    input: &EventRecord,
) -> bool {
    output.event_version == 1
        && output.turn_id == request.turn_id
        && output.request_index == request.request_index
        && output.call_id == request.call_id
        && event.kind == event_kind::AGENT_MEMORY_WRITE_OUTPUT
        && event.actor == EventActor::Capability
        && event.span_id.as_deref() == Some(output.call_id.as_str())
        && event.causation_id.as_deref() == Some(&requested.event_id)
        && event.seq > requested.seq
        && same_scope(event, input)
}

/// The `memory.written` event and context node a written result names, from
/// the session snapshot, checked against the request; `None` when either is
/// missing or differs.
pub(crate) fn written_records<'a>(
    snapshot: &'a [EventRecord],
    request: &MemoryWriteRequested,
    requested: &EventRecord,
    output: &MemoryWriteOutput,
    output_event: &EventRecord,
) -> Option<(&'a EventRecord, &'a EventRecord)> {
    let write = request.write.as_ref()?;
    let memory_id = output.result.memory_id()?;
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
        && MemoryWriteResult::written(write, node.id.clone()) == output.result)
        .then_some((written, recorded))
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let remember = MemoryWrite::Remember {
            text: "The user's dog is called Miso.".into(),
            replaces: Some("memory-01k00000000000000000000000".into()),
        };
        let secret = MemoryWrite::Remember {
            text: "password: x".into(),
            replaces: None,
        };
        use MemoryWriteRefusal::*;
        assert_eq!(refusal(None, true, 9, |_| false), Some(InvalidArguments));
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
        assert_eq!(
            normalize_call(REMEMBER_ID, &json!({"text": "  Miso is the dog.  "})),
            Some(MemoryWrite::Remember {
                text: "Miso is the dog.".into(),
                replaces: None
            })
        );
        assert_eq!(normalize_call(REMEMBER_ID, &json!({"text": "   "})), None);
        assert_eq!(
            normalize_call(
                REMEMBER_ID,
                &json!({"text": "x".repeat(MAX_MEMORY_TEXT_CHARS + 1)})
            ),
            None
        );
        assert_eq!(
            normalize_call(REMEMBER_ID, &json!({"text": "x", "replaces": "memory-1"})),
            None
        );
        assert_eq!(
            normalize_call(
                FORGET_ID,
                &json!({"memory_id": "memory-01k00000000000000000000000"})
            ),
            Some(MemoryWrite::Forget {
                memory_id: "memory-01k00000000000000000000000".into()
            })
        );
        assert_eq!(
            normalize_call(FORGET_ID, &json!({"memory_id": "x", "y": 1})),
            None
        );
        assert_eq!(
            normalize_call("memory.search", &json!({"query": "x"})),
            None
        );
    }
}
