//! Event-reduced task lifecycle. The ledger records external commitments, not model thought.

use ditto_protocol::{EventRecord, event_kind};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    #[default]
    Accepted,
    Assembling,
    Running,
    WaitingEvent,
    WaitingApproval,
    Blocked,
    Verifying,
    Completed,
    Failed,
    Cancelled,
}

impl TaskStatus {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct TaskLedger {
    pub status: TaskStatus,
    pub last_seq: i64,
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum TaskStateError {
    #[error("event sequence moved backwards: {next} <= {current}")]
    NonMonotonic { current: i64, next: i64 },
    #[error("terminal task cannot accept event {kind}")]
    Terminal { kind: String },
}

impl TaskLedger {
    /// Reduces one newer event into the task ledger.
    ///
    /// # Errors
    ///
    /// Rejects non-monotonic sequences and events after a terminal state.
    pub fn apply(&mut self, event: &EventRecord) -> Result<TaskStatus, TaskStateError> {
        if event.seq <= self.last_seq {
            return Err(TaskStateError::NonMonotonic {
                current: self.last_seq,
                next: event.seq,
            });
        }
        if self.status.is_terminal() {
            return Err(TaskStateError::Terminal {
                kind: event.event.kind.clone(),
            });
        }

        self.status = match event.event.kind.as_str() {
            event_kind::INPUT_RECEIVED => TaskStatus::Assembling,
            event_kind::MODEL_STARTED
            | event_kind::MODEL_DELTA
            | event_kind::EXECUTION_STARTED
            | event_kind::EXECUTION_OUTPUT => TaskStatus::Running,
            event_kind::POLICY_APPROVAL_REQUIRED => TaskStatus::WaitingApproval,
            event_kind::TASK_BLOCKED => TaskStatus::Blocked,
            event_kind::TASK_VERIFYING => TaskStatus::Verifying,
            event_kind::TASK_COMPLETED => TaskStatus::Completed,
            event_kind::TASK_FAILED => TaskStatus::Failed,
            event_kind::TASK_CANCELLED => TaskStatus::Cancelled,
            _ => self.status,
        };
        self.last_seq = event.seq;
        Ok(self.status)
    }

    /// Rebuilds a ledger from events ordered by ascending sequence.
    ///
    /// # Errors
    ///
    /// Returns [`TaskStateError`] when input order or terminal-state rules fail.
    pub fn rebuild<'a>(
        events: impl IntoIterator<Item = &'a EventRecord>,
    ) -> Result<Self, TaskStateError> {
        let mut ledger = Self::default();
        for event in events {
            ledger.apply(event)?;
        }
        Ok(ledger)
    }
}

#[cfg(test)]
mod tests {
    use ditto_protocol::{Actor, EventDraft, EventRecord, event_kind};
    use serde_json::json;

    use super::*;

    fn event(seq: i64, kind: &str) -> EventRecord {
        EventRecord {
            seq,
            event: EventDraft::inline(Actor::Kernel, kind, json!({})),
        }
    }

    #[test]
    fn rebuilds_tracked_path_from_events() {
        let events = [
            event(1, event_kind::INPUT_RECEIVED),
            event(2, event_kind::MODEL_STARTED),
            event(3, event_kind::TASK_VERIFYING),
            event(4, event_kind::TASK_COMPLETED),
        ];
        let ledger = TaskLedger::rebuild(&events).unwrap();
        assert_eq!(ledger.status, TaskStatus::Completed);
        assert_eq!(ledger.last_seq, 4);
    }
}
