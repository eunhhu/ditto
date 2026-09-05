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
}

#[derive(Default)]
pub(crate) struct RunSlot {
    active: Option<ActiveRun>,
    stopping: bool,
}

struct ActiveRun {
    input_event_id: String,
    cancellation: CancellationToken,
    finished: CancellationToken,
}

#[derive(Debug, Error)]
pub enum AgentRunError {
    #[error("invalid run command: {0}")]
    Invalid(&'static str),
    #[error("request identity is already used for different input")]
    Conflict,
    #[error("another run is active; no work was queued")]
    Busy,
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
        // Check the executor before durable acceptance, including non-async callers.
        let runtime = tokio::runtime::Handle::try_current().map_err(|_| AgentRunError::Stopping)?;
        let mut slot = self
            .inner
            .agent_runs
            .lock()
            .map_err(|_| AgentRunError::Storage)?;
        if let Some((input, last)) = self.run_boundary(&query, &task_id)? {
            validate_agent_input(&input, &query)?;
            if input
                .payload
                .get("text")
                .and_then(serde_json::Value::as_str)
                != Some(&text)
            {
                return Err(AgentRunError::Conflict);
            }
            return status_from_boundary(query, input, last, &slot);
        }
        if slot.stopping {
            return Err(AgentRunError::Stopping);
        }
        if slot.active.is_some() {
            return Err(AgentRunError::Busy);
        }
        let admitted = self.admit_read_only_turn(
            SubmitInputCommand {
                text,
                session_id: Some(query.session_id.clone()),
                task_id: Some(task_id),
            },
            Some(AgentRunMetadata {
                version: 1,
                request_id: query.request_id.clone(),
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
        status_from_boundary(query, input.clone(), input, &slot)
    }

    pub fn inspect_agent_run(
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
        status_from_boundary(query, input, last, &slot)
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
        status_from_boundary(query, input, last, &slot)
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

struct ActiveGuard {
    kernel: DittoKernel,
    input_event_id: String,
    finished: CancellationToken,
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
    }
}

fn validate_query(query: &AgentRunQuery) -> Result<String, AgentRunError> {
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
    if metadata.version != 1
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
    Ok(())
}

fn status_from_boundary(
    query: AgentRunQuery,
    input: EventRecord,
    last: EventRecord,
    slot: &RunSlot,
) -> Result<AgentRunResponse, AgentRunError> {
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
