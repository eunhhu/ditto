use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::AgentRunResponse;

pub const MAX_PENDING_SCHEDULES: usize = 100;

/// One explicit future read-only request. No effect grant or provider selection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScheduleRunCommand {
    pub request_id: String,
    pub session_id: String,
    pub text: String,
    pub due_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScheduleListQuery {
    pub session_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScheduleStatus {
    Pending,
    Running,
    Unverified,
    Failed,
    Interrupted,
    Cancelled,
    Missed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScheduleWaitReason {
    SchedulerStopped,
    ProviderDisabled,
    DueTime,
    RuntimeBusy,
    Dispatch,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduleResponse {
    pub request_id: String,
    pub session_id: String,
    pub due_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub status: ScheduleStatus,
    pub cancellation_requested: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub waiting_for: Option<ScheduleWaitReason>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run: Option<AgentRunResponse>,
}
