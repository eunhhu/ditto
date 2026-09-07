use std::sync::Arc;

use ditto_model::{CancellationToken, ModelDriver};
use ditto_protocol::{
    AgentRunQuery, AgentRunResponse, AgentRunStatus, EventActor, EventRecord,
    MAX_AGENT_RUN_TEXT_BYTES, StartAgentRunCommand, SubmitInputCommand, event_kind,
};
use ditto_retrieval::SessionId;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use ulid::Ulid;

use crate::{
    DittoKernel, KernelError, TurnFailedPayload, TurnFinishedPayload, normalize_input_text,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AgentRunMetadata {
    pub version: u16,
    pub request_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sort: Option<crate::turn::sort::SortGrant>,
}

#[derive(Default)]
pub(crate) struct RunSlot {
    pub(crate) active: Option<ActiveRun>,
    pub(crate) stopping: bool,
}

pub(crate) struct ActiveRun {
    pub(crate) input_event_id: String,
    pub(crate) cancellation: CancellationToken,
    pub(crate) finished: CancellationToken,
}

#[derive(Debug, Error)]
pub enum AgentRunError {
    #[error("invalid run command: {0}")]
    Invalid(&'static str),
    #[error("request identity is already used for different input")]
    Conflict,
    #[error("another run is active; no work was queued")]
    Busy,
    #[error("pending schedule limit (100) reached; cancel a pending request before adding another")]
    ScheduleFull,
    #[error("runtime is shutting down")]
    Stopping,
    #[error("run is unavailable in this session")]
    NotFound,
    #[error("run storage or source verification is unavailable")]
    Storage,
}

impl From<KernelError> for AgentRunError {
    fn from(_: KernelError) -> Self {
        Self::Storage
    }
}

impl DittoKernel {
    pub(crate) fn agent_context_candidates(
        &self,
        session: &str,
        task: &str,
        evaluated_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<ditto_context::ContextCandidate>, KernelError> {
        let _gate = self
            .inner
            .context_admission_gate
            .lock()
            .map_err(|_| KernelError::ContextAdmissionGatePoisoned)?;
        let high_water = self.inner.events.latest_seq()?;
        let snapshot = self
            .inner
            .context_projection
            .synchronize_and_verified_snapshot_through_at(
                &self.inner.events,
                high_water,
                session,
                Some(task),
                evaluated_at,
                &mut ditto_retrieval::RetrievalWorkBudget::new(),
            )?;
        Ok(snapshot
            .into_candidates()
            .into_iter()
            .map(ditto_context::ContextCandidate::ranked)
            .collect())
    }

    /// Admit once before dispatch. HTTP ownership is intentionally absent.
    pub fn start_agent_run(
        &self,
        command: StartAgentRunCommand,
        driver: Arc<dyn ModelDriver>,
    ) -> Result<AgentRunResponse, AgentRunError> {
        validate_query(&AgentRunQuery {
            request_id: command.request_id.clone(),
            session_id: command.session_id.clone(),
        })?;
        let mut slot = self
            .inner
            .agent_runs
            .lock()
            .map_err(|_| AgentRunError::Storage)?;
        if self
            .inner
            .events
            .is_scheduled_run(&command.request_id)
            .map_err(|_| AgentRunError::Storage)?
        {
            return Err(AgentRunError::Conflict);
        }
        self.start_agent_run_locked(command, driver, &mut slot)
    }

    pub(crate) fn start_agent_run_locked(
        &self,
        command: StartAgentRunCommand,
        driver: Arc<dyn ModelDriver>,
        slot: &mut RunSlot,
    ) -> Result<AgentRunResponse, AgentRunError> {
        let query = AgentRunQuery {
            request_id: command.request_id,
            session_id: command.session_id,
        };
        let task_id = validate_query(&query)?;
        if command.text.len() > MAX_AGENT_RUN_TEXT_BYTES {
            return Err(AgentRunError::Invalid("text exceeds 16 KiB"));
        }
        let text = normalize_input_text(&command.text)
            .map_err(|_| AgentRunError::Invalid("text is empty or invalid"))?;
        if let Some(permission) = &command.sort {
            ditto_artifact_sort::validate_input(permission.text.as_bytes()).map_err(|_| {
                AgentRunError::Invalid(
                    "sort attachment exceeds 64 KiB / 4096 UTF-8 lines or contains NUL",
                )
            })?;
        }
        // Check the executor before durable acceptance, including non-async callers.
        let runtime = tokio::runtime::Handle::try_current().map_err(|_| AgentRunError::Stopping)?;
        if let Some((input, last)) = self.run_boundary(&query, &task_id)? {
            validate_agent_input(&input, &query)?;
            let metadata: AgentRunMetadata =
                serde_json::from_value(input.payload["agent_run"].clone())
                    .map_err(|_| AgentRunError::Storage)?;
            if !crate::turn::sort::permission_matches(metadata.sort.as_ref(), command.sort.as_ref())
            {
                return Err(AgentRunError::Conflict);
            }
            if input
                .payload
                .get("text")
                .and_then(serde_json::Value::as_str)
                != Some(&text)
            {
                return Err(AgentRunError::Conflict);
            }
            return self.agent_status_from_boundary(query, input, last, slot);
        }
        if slot.stopping {
            return Err(AgentRunError::Stopping);
        }
        if slot.active.is_some() {
            return Err(AgentRunError::Busy);
        }
        let sort = command
            .sort
            .map(|permission| {
                let stored = self.store_artifact(
                    permission.text.as_bytes(),
                    crate::ArtifactWriteContext {
                        session_id: Some(query.session_id.clone()),
                        task_id: Some(task_id.clone()),
                        mime: Some("text/plain; charset=utf-8".into()),
                        purpose: Some("model sort attachment".into()),
                        ..Default::default()
                    },
                )?;
                Ok::<_, AgentRunError>(crate::turn::sort::SortGrant {
                    reference: stored.metadata.reference.to_string(),
                    source_event_id: stored.event.event_id,
                    allow_deduplicate: permission.allow_deduplicate,
                })
            })
            .transpose()?;
        let admitted = self.admit_read_only_turn(
            SubmitInputCommand {
                text,
                session_id: Some(query.session_id.clone()),
                task_id: Some(task_id),
            },
            Some(AgentRunMetadata {
                version: if sort.is_some() { 2 } else { 1 },
                request_id: query.request_id.clone(),
                sort,
            }),
        )?;
        let input = admitted.input().clone();
        let cancellation = CancellationToken::new();
        let finished = CancellationToken::new();
        slot.active = Some(ActiveRun {
            input_event_id: input.event_id.clone(),
            cancellation: cancellation.clone(),
            finished: finished.clone(),
        });
        // Construct the guard before spawning: even a dropped/unpolled future or
        // panic releases the slot. Durable nonterminal state then reads interrupted.
        let guard = ActiveGuard {
            kernel: self.clone(),
            input_event_id: input.event_id.clone(),
            finished,
        };
        let kernel = self.clone();
        let task = runtime.spawn(async move {
            let _guard = guard;
            let _result = kernel
                .continue_read_only_turn(
                    admitted,
                    None::<Vec<ditto_context::ContextCandidate>>,
                    driver.as_ref(),
                    cancellation,
                    Default::default(),
                )
                .await;
            // Turn failures are journaled by the loop. A storage failure is left
            // interrupted; no invented terminal and no automatic provider retry.
        });
        // The slot's cancellation/completion tokens own the lifetime. Dropping
        // the join handle detaches HTTP ownership, not shutdown accounting.
        drop(task);
        self.agent_status_from_boundary(query, input.clone(), input, slot)
    }

    pub fn inspect_agent_run(
        &self,
        query: AgentRunQuery,
    ) -> Result<AgentRunResponse, AgentRunError> {
        let slot = self
            .inner
            .agent_runs
            .lock()
            .map_err(|_| AgentRunError::Storage)?;
        self.inspect_agent_run_locked(query, &slot)
    }

    pub(crate) fn inspect_agent_run_locked(
        &self,
        query: AgentRunQuery,
        slot: &RunSlot,
    ) -> Result<AgentRunResponse, AgentRunError> {
        let task_id = validate_query(&query)?;
        let (input, last) = self
            .run_boundary(&query, &task_id)?
            .ok_or(AgentRunError::NotFound)?;
        validate_agent_input(&input, &query)?;
        self.agent_status_from_boundary(query, input, last, slot)
    }

    pub fn cancel_agent_run(
        &self,
        query: AgentRunQuery,
    ) -> Result<AgentRunResponse, AgentRunError> {
        let task_id = validate_query(&query)?;
        let slot = self
            .inner
            .agent_runs
            .lock()
            .map_err(|_| AgentRunError::Storage)?;
        let (input, last) = self
            .run_boundary(&query, &task_id)?
            .ok_or(AgentRunError::NotFound)?;
        validate_agent_input(&input, &query)?;
        if let Some(active) = &slot.active
            && active.input_event_id == input.event_id
        {
            active.cancellation.cancel();
        }
        self.agent_status_from_boundary(query, input, last, &slot)
    }

    /// Close admission and drain the one owned execution, without a heartbeat.
    pub async fn shutdown_agent_runs(&self) -> Result<(), AgentRunError> {
        let finished = {
            let mut slot = self
                .inner
                .agent_runs
                .lock()
                .map_err(|_| AgentRunError::Storage)?;
            slot.stopping = true;
            self.inner.scheduler_wake.notify_one();
            slot.active.as_ref().map(|active| {
                active.cancellation.cancel();
                active.finished.clone()
            })
        };
        if let Some(finished) = finished {
            finished.cancelled().await;
        }
        Ok(())
    }

    fn run_boundary(
        &self,
        query: &AgentRunQuery,
        task_id: &str,
    ) -> Result<Option<(EventRecord, EventRecord)>, AgentRunError> {
        self.inner
            .events
            .task_turn_boundary(&query.session_id, task_id)
            .map_err(|_| AgentRunError::Storage)
    }
}

pub(crate) struct ActiveGuard {
    pub(crate) kernel: DittoKernel,
    pub(crate) input_event_id: String,
    pub(crate) finished: CancellationToken,
}

impl Drop for ActiveGuard {
    fn drop(&mut self) {
        if let Ok(mut slot) = self.kernel.inner.agent_runs.lock()
            && slot
                .active
                .as_ref()
                .is_some_and(|active| active.input_event_id == self.input_event_id)
        {
            slot.active = None;
        }
        self.finished.cancel();
        self.kernel.inner.scheduler_wake.notify_one();
    }
}

pub(crate) fn validate_query(query: &AgentRunQuery) -> Result<String, AgentRunError> {
    SessionId::new(&query.session_id)
        .map_err(|_| AgentRunError::Invalid("session ID is not canonical"))?;
    let id = query
        .request_id
        .parse::<Ulid>()
        .map_err(|_| AgentRunError::Invalid("request ID must be a canonical uppercase ULID"))?;
    if id.to_string() != query.request_id {
        return Err(AgentRunError::Invalid(
            "request ID must be a canonical uppercase ULID",
        ));
    }
    Ok(format!("run_{}", query.request_id))
}

pub(crate) fn validate_agent_input(
    input: &EventRecord,
    query: &AgentRunQuery,
) -> Result<(), AgentRunError> {
    let task_id = validate_query(query)?;
    let metadata: AgentRunMetadata = serde_json::from_value(
        input
            .payload
            .get("agent_run")
            .cloned()
            .ok_or(AgentRunError::Conflict)?,
    )
    .map_err(|_| AgentRunError::Storage)?;
    if metadata.version != if metadata.sort.is_some() { 2 } else { 1 }
        || metadata.request_id != query.request_id
        || input.actor != EventActor::User
        || input.kind != event_kind::INPUT_RECEIVED
        || input.session_id.as_deref() != Some(&query.session_id)
        || input.task_id.as_deref() != Some(&task_id)
        || input
            .correlation_id
            .as_deref()
            .is_none_or(|id| !id.starts_with("turn_"))
    {
        return Err(AgentRunError::Conflict);
    }
    if let Some(grant) = &metadata.sort {
        grant.validate().map_err(|_| AgentRunError::Storage)?;
    }
    Ok(())
}

impl DittoKernel {
    fn agent_status_from_boundary(
        &self,
        query: AgentRunQuery,
        input: EventRecord,
        last: EventRecord,
        slot: &RunSlot,
    ) -> Result<AgentRunResponse, AgentRunError> {
        let sort = self.agent_sort_progress(
            &input,
            slot.active
                .as_ref()
                .is_some_and(|active| active.input_event_id == input.event_id),
        )?;
        let turn_id = input.correlation_id.ok_or(AgentRunError::Storage)?;
        let task_id = input.task_id.ok_or(AgentRunError::Storage)?;
        let active = slot
            .active
            .as_ref()
            .filter(|active| active.input_event_id == input.event_id);
        let mut response = AgentRunResponse {
            request_id: query.request_id,
            session_id: query.session_id,
            task_id,
            turn_id,
            status: if active.is_some() {
                AgentRunStatus::Running
            } else {
                AgentRunStatus::Interrupted
            },
            cancellation_requested: active.is_some_and(|active| active.cancellation.is_cancelled()),
            sort,
            response: None,
            failure_code: None,
        };
        match last.kind.as_str() {
            event_kind::TURN_FINISHED => {
                let terminal: TurnFinishedPayload =
                    serde_json::from_value(last.payload).map_err(|_| AgentRunError::Storage)?;
                if last.actor != EventActor::System
                    || terminal.event_version != 1
                    || terminal.turn_id != response.turn_id
                    || terminal.outcome.turn_id != response.turn_id
                    || terminal.outcome.session_id != response.session_id
                    || terminal.outcome.task_id != response.task_id
                {
                    return Err(AgentRunError::Storage);
                }
                response.status = AgentRunStatus::Unverified;
                response.response = Some(terminal.outcome.response);
                response.cancellation_requested = false;
            }
            event_kind::TURN_FAILED => {
                let terminal: TurnFailedPayload =
                    serde_json::from_value(last.payload).map_err(|_| AgentRunError::Storage)?;
                if last.actor != EventActor::System
                    || terminal.event_version != 1
                    || terminal.turn_id != response.turn_id
                    || terminal.failure.turn_id != response.turn_id
                    || terminal.failure.session_id != response.session_id
                    || terminal.failure.task_id != response.task_id
                {
                    return Err(AgentRunError::Storage);
                }
                response.status = AgentRunStatus::Failed;
                response.failure_code = serde_json::to_value(terminal.failure.code)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_owned));
                response.cancellation_requested = false;
            }
            _ => {}
        }
        Ok(response)
    }
}
