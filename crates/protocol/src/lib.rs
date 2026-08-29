//! Stable wire and event contracts shared by every Ditto client and worker.

use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;

static ID_COUNTER: AtomicU64 = AtomicU64::new(0);

pub mod event_kind {
    pub const INPUT_RECEIVED: &str = "input.received";
    pub const CONTEXT_COMPILED: &str = "context.compiled";
    pub const CAPABILITIES_SELECTED: &str = "capabilities.selected";
    pub const MODEL_STARTED: &str = "model.started";
    pub const MODEL_DELTA: &str = "model.delta";
    pub const CAPABILITY_REQUESTED: &str = "capability.requested";
    pub const POLICY_APPROVAL_REQUIRED: &str = "policy.approval_required";
    pub const POLICY_LEASE_GRANTED: &str = "policy.lease_granted";
    pub const EXECUTION_STARTED: &str = "execution.started";
    pub const EXECUTION_OUTPUT: &str = "execution.output";
    pub const ARTIFACT_CREATED: &str = "artifact.created";
    pub const STATE_PATCH_PROPOSED: &str = "state.patch_proposed";
    pub const TASK_BLOCKED: &str = "task.blocked";
    pub const TASK_VERIFYING: &str = "task.verifying";
    pub const TASK_COMPLETED: &str = "task.completed";
    pub const TASK_FAILED: &str = "task.failed";
    pub const TASK_CANCELLED: &str = "task.cancelled";
    pub const IMPROVEMENT_CANDIDATE_CREATED: &str = "improvement.candidate_created";
    pub const IMPROVEMENT_PROMOTED: &str = "improvement.promoted";
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Actor {
    User,
    Model,
    Tool,
    Policy,
    Scheduler,
    Kernel,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "storage", content = "value", rename_all = "snake_case")]
pub enum PayloadRef {
    Inline(Value),
    Artifact(String),
    Empty,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct EventDraft {
    pub timestamp_ms: i64,
    pub session_id: Option<String>,
    pub task_id: Option<String>,
    pub actor: Actor,
    pub kind: String,
    pub payload_ref: PayloadRef,
    pub causation_id: Option<String>,
    pub correlation_id: Option<String>,
    pub span_id: Option<String>,
}

impl EventDraft {
    pub fn inline(actor: Actor, kind: impl Into<String>, payload: Value) -> Self {
        Self {
            timestamp_ms: Utc::now().timestamp_millis(),
            session_id: None,
            task_id: None,
            actor,
            kind: kind.into(),
            payload_ref: PayloadRef::Inline(payload),
            causation_id: None,
            correlation_id: None,
            span_id: None,
        }
    }

    #[must_use]
    pub fn scoped(mut self, session_id: impl Into<String>, task_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self.task_id = Some(task_id.into());
        self
    }

    #[must_use]
    pub fn correlated(mut self, correlation_id: impl Into<String>) -> Self {
        self.correlation_id = Some(correlation_id.into());
        self
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct EventRecord {
    pub seq: i64,
    #[serde(flatten)]
    pub event: EventDraft,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum EffectClass {
    Pure,
    Read,
    WriteReversible,
    WriteIrreversible,
    ExternalCommunication,
    Privileged,
    CredentialAccess,
}

impl EffectClass {
    pub fn permits(self, claimed: Self) -> bool {
        use EffectClass::{
            CredentialAccess, ExternalCommunication, Privileged, Pure, Read, WriteIrreversible,
            WriteReversible,
        };
        match self {
            Pure => claimed == Pure,
            Read => matches!(claimed, Pure | Read),
            WriteReversible => matches!(claimed, Pure | Read | WriteReversible),
            WriteIrreversible => {
                matches!(claimed, Pure | Read | WriteReversible | WriteIrreversible)
            }
            ExternalCommunication => matches!(claimed, Pure | Read | ExternalCommunication),
            Privileged => claimed != CredentialAccess,
            CredentialAccess => matches!(claimed, Pure | Read | CredentialAccess),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ContextReceiptItem {
    pub node_id: String,
    pub label: String,
    pub source: String,
    pub epistemic: String,
    pub reason: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ContextReceipt {
    pub capsule: String,
    pub included: Vec<ContextReceiptItem>,
    pub token_estimate: usize,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CapabilityCard {
    pub id: String,
    pub summary: String,
    pub namespace: String,
    pub maximum_effect: EffectClass,
    pub placements: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientCommand {
    Submit {
        input: String,
        session_id: Option<String>,
        task_id: Option<String>,
    },
    Replay {
        session_id: Option<String>,
        task_id: Option<String>,
        #[serde(default)]
        after_seq: i64,
    },
    Cancel {
        task_id: String,
    },
    Ping,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    Accepted { session_id: String, task_id: String },
    ContextReceipt { receipt: ContextReceipt },
    CapabilitiesSelected { capabilities: Vec<CapabilityCard> },
    Event { event: EventRecord },
    ModelDelta { text: String },
    End { verified: bool },
    Error { message: String },
    Pong,
}

/// Creates sortable-enough local identifiers without a global service.
pub fn new_id(prefix: &str) -> String {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let counter = ID_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{prefix}_{:x}{:08x}", elapsed.as_millis(), counter)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effect_ceiling_is_monotonic() {
        assert!(EffectClass::WriteReversible.permits(EffectClass::Read));
        assert!(!EffectClass::Read.permits(EffectClass::WriteReversible));
        assert!(!EffectClass::ExternalCommunication.permits(EffectClass::WriteReversible));
    }

    #[test]
    fn identifiers_keep_prefix_and_are_unique() {
        let first = new_id("task");
        let second = new_id("task");
        assert!(first.starts_with("task_"));
        assert_ne!(first, second);
    }
}
