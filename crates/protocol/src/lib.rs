use std::{fmt, str::FromStr};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub mod event_kind {
    pub const INPUT_RECEIVED: &str = "input.received";
    pub const RUNTIME_STARTED: &str = "runtime.started";
    pub const CONTEXT_COMPILED: &str = "context.compiled";
    pub const CAPABILITIES_SELECTED: &str = "capabilities.selected";
    pub const CAPABILITY_REQUESTED: &str = "capability.requested";
    pub const POLICY_APPROVAL_REQUIRED: &str = "policy.approval_required";
    pub const POLICY_LEASE_GRANTED: &str = "policy.lease_granted";
    pub const EXECUTION_STARTED: &str = "execution.started";
    pub const EXECUTION_OUTPUT: &str = "execution.output";
    pub const ARTIFACT_CREATED: &str = "artifact.created";
    pub const TASK_COMPLETED: &str = "task.completed";
    pub const TASK_BLOCKED: &str = "task.blocked";
    pub const TASK_CANCEL_REQUESTED: &str = "task.cancel_requested";
    pub const IMPROVEMENT_CANDIDATE_CREATED: &str = "improvement.candidate_created";
    pub const IMPROVEMENT_PROMOTED: &str = "improvement.promoted";
    pub const MODEL_REQUESTED: &str = "model.requested";
    pub const MODEL_OUTPUT: &str = "model.output";
    pub const TURN_FINISHED: &str = "turn.finished";
    pub const TURN_FAILED: &str = "turn.failed";
}

#[cfg(test)]
mod tests {
    use super::event_kind;

    #[test]
    fn task_003_turn_event_kinds_are_stable() {
        assert_eq!(event_kind::MODEL_REQUESTED, "model.requested");
        assert_eq!(event_kind::MODEL_OUTPUT, "model.output");
        assert_eq!(event_kind::TURN_FINISHED, "turn.finished");
        assert_eq!(event_kind::TURN_FAILED, "turn.failed");
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventActor {
    User,
    Model,
    Capability,
    Policy,
    Scheduler,
    System,
}

impl EventActor {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Model => "model",
            Self::Capability => "capability",
            Self::Policy => "policy",
            Self::Scheduler => "scheduler",
            Self::System => "system",
        }
    }
}

impl fmt::Display for EventActor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseEventActorError(String);

impl fmt::Display for ParseEventActorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "unknown event actor: {}", self.0)
    }
}

impl std::error::Error for ParseEventActorError {}

impl FromStr for EventActor {
    type Err = ParseEventActorError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "user" => Ok(Self::User),
            "model" => Ok(Self::Model),
            "capability" | "tool" => Ok(Self::Capability),
            "policy" => Ok(Self::Policy),
            "scheduler" => Ok(Self::Scheduler),
            "system" => Ok(Self::System),
            other => Err(ParseEventActorError(other.to_owned())),
        }
    }
}

/// Internal event draft. Public network ingress must use typed commands instead.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewEvent {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    pub actor: EventActor,
    pub kind: String,
    #[serde(default)]
    pub payload: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub causation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub span_id: Option<String>,
}

impl NewEvent {
    pub fn user_input(
        session_id: impl Into<String>,
        task_id: Option<String>,
        text: impl Into<String>,
    ) -> Self {
        Self {
            session_id: Some(session_id.into()),
            task_id,
            actor: EventActor::User,
            kind: event_kind::INPUT_RECEIVED.to_owned(),
            payload: serde_json::json!({ "text": text.into() }),
            causation_id: None,
            correlation_id: None,
            span_id: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventRecord {
    pub seq: i64,
    pub event_id: String,
    pub recorded_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    pub actor: EventActor,
    pub kind: String,
    pub payload: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub causation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub span_id: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EventQuery {
    #[serde(default)]
    pub after_seq: Option<i64>,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub task_id: Option<String>,
}

impl EventQuery {
    pub fn normalized_limit(&self) -> usize {
        self.limit.unwrap_or(100).clamp(1, 1_000)
    }

    pub fn matches_scope(&self, event: &EventRecord) -> bool {
        let session_matches = self
            .session_id
            .as_ref()
            .is_none_or(|session| event.session_id.as_ref() == Some(session));
        let task_matches = self
            .task_id
            .as_ref()
            .is_none_or(|task| event.task_id.as_ref() == Some(task));

        session_matches && task_matches
    }

    pub fn matches(&self, event: &EventRecord) -> bool {
        self.after_seq.is_none_or(|after| event.seq > after) && self.matches_scope(event)
    }
}

/// Trusted user-input command accepted by the public daemon API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubmitInputCommand {
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubmitInputResponse {
    pub event: EventRecord,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthResponse {
    pub status: String,
    pub version: String,
    pub durable_events: u64,
    pub latest_seq: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CapabilitySearchQuery {
    #[serde(default)]
    pub query: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
}
