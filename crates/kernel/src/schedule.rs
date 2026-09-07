use std::{
    sync::{Arc, atomic::Ordering},
    time::Duration,
};

use chrono::{DateTime, Utc};
use ditto_event_store::{ScheduleEntry, ScheduleState};
use ditto_model::{CancellationToken, ModelDriver};
use ditto_protocol::{
    AgentRunQuery, AgentRunStatus, EventActor, EventRecord, MAX_AGENT_RUN_TEXT_BYTES,
    MAX_PENDING_SCHEDULES, NewEvent, ScheduleResponse, ScheduleRunCommand, ScheduleStatus,
    ScheduleWaitReason, StartAgentRunCommand,
};
use serde_json::json;

use crate::{
    AgentRunError, DittoKernel,
    agent_run::{RunSlot, validate_query},
    normalize_input_text,
};

impl DittoKernel {
    pub fn schedule_run(
        &self,
        mut command: ScheduleRunCommand,
    ) -> Result<ScheduleResponse, AgentRunError> {
        validate_query(&identity(&command))?;
        if command.text.len() > MAX_AGENT_RUN_TEXT_BYTES {
            return Err(AgentRunError::Invalid("text exceeds 16 KiB"));
        }
        command.text = normalize_input_text(&command.text)
            .map_err(|_| AgentRunError::Invalid("text is empty or invalid"))?;
        validate_times(&command)?;
        let slot = self
            .inner
            .agent_runs
            .lock()
            .map_err(|_| AgentRunError::Storage)?;
        if let Some(entry) = self
            .inner
            .events
            .schedule_entry(&command.session_id, &command.request_id)
            .map_err(storage)?
        {
            if self.schedule_source(&entry)? != command {
                return Err(AgentRunError::Conflict);
            }
            return self.schedule_status(&entry, &slot);
        }
        if slot.stopping {
            return Err(AgentRunError::Stopping);
        }
        let now = Utc::now();
        if command.due_at <= now || command.due_at > now + chrono::Duration::days(365) {
            return Err(AgentRunError::Invalid(
                "due time must be in the future and within 365 days",
            ));
        }
        if self
            .inner
            .events
            .pending_schedules()
            .map_err(storage)?
            .len()
            >= MAX_PENDING_SCHEDULES
        {
            return Err(AgentRunError::ScheduleFull);
        }
        let run_request_id = ulid::Ulid::new().to_string();
        if self
            .inner
            .events
            .is_scheduled_run(&run_request_id)
            .map_err(storage)?
            || self
                .inner
                .events
                .task_turn_boundary(&command.session_id, &format!("run_{run_request_id}"))
                .map_err(storage)?
                .is_some()
        {
            return Err(AgentRunError::Storage);
        }
        self.append_and_publish(NewEvent {
            session_id: Some(command.session_id.clone()),
            task_id: Some(format!("schedule_{}", command.request_id)),
            actor: EventActor::User,
            kind: ScheduleState::Pending.event_kind().into(),
            payload: json!({"version":1,"command":command,"run_request_id":run_request_id}),
            causation_id: None,
            correlation_id: None,
            span_id: None,
        })?;
        self.inner.scheduler_wake.notify_one();
        let entry = self.schedule_entry(&identity(&command))?;
        self.schedule_status(&entry, &slot)
    }

    pub fn inspect_schedule(
        &self,
        query: AgentRunQuery,
    ) -> Result<ScheduleResponse, AgentRunError> {
        let slot = self
            .inner
            .agent_runs
            .lock()
            .map_err(|_| AgentRunError::Storage)?;
        self.schedule_status(&self.schedule_entry(&query)?, &slot)
    }

    pub fn list_pending_schedules(
        &self,
        session: &str,
    ) -> Result<Vec<ScheduleResponse>, AgentRunError> {
        ditto_retrieval::SessionId::new(session)
            .map_err(|_| AgentRunError::Invalid("session ID is not canonical"))?;
        let slot = self
            .inner
            .agent_runs
            .lock()
            .map_err(|_| AgentRunError::Storage)?;
        self.inner
            .events
            .pending_schedules()
            .map_err(storage)?
            .iter()
            .filter(|entry| entry.session_id == session)
            .map(|entry| self.schedule_status(entry, &slot))
            .collect()
    }

    pub fn cancel_schedule(&self, query: AgentRunQuery) -> Result<ScheduleResponse, AgentRunError> {
        let slot = self
            .inner
            .agent_runs
            .lock()
            .map_err(|_| AgentRunError::Storage)?;
        let entry = self.schedule_entry(&query)?;
        self.schedule_source(&entry)?;
        if entry.state == ScheduleState::Pending {
            self.schedule_transition(&entry, ScheduleState::Cancelled)?;
        } else if entry.state == ScheduleState::Claimed {
            let result = self.schedule_status(&entry, &slot)?;
            if result.status == ScheduleStatus::Running {
                self.schedule_transition(&entry, ScheduleState::CancelRequested)?;
                // Status proves that this run owns the single active slot.
                if let Some(active) = &slot.active {
                    active.cancellation.cancel();
                }
            }
        }
        self.inner.scheduler_wake.notify_one();
        self.schedule_status(&self.schedule_entry(&query)?, &slot)
    }

    /// Exactly one daemon-owned future. Empty queues have no timer or model work.
    pub async fn run_scheduler(
        &self,
        driver: Option<Arc<dyn ModelDriver>>,
        shutdown: CancellationToken,
    ) -> Result<(), AgentRunError> {
        self.inner
            .scheduler_state
            .compare_exchange(
                0,
                if driver.is_some() { 2 } else { 1 },
                Ordering::SeqCst,
                Ordering::SeqCst,
            )
            .map_err(|_| AgentRunError::Busy)?;
        let _guard = SchedulerGuard(self.clone());
        loop {
            // Register before reading durable state to avoid lost wakeups.
            let notified = self.inner.scheduler_wake.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if shutdown.is_cancelled() {
                return Ok(());
            }
            match self.scheduler_step(driver.as_ref(), Utc::now)? {
                Step::Again => continue,
                Step::Stop => return Ok(()),
                Step::Wait(deadline) => {
                    let timer = async {
                        match deadline {
                            Some(at) => {
                                let remaining = at
                                    .signed_duration_since(Utc::now())
                                    .to_std()
                                    .unwrap_or(Duration::ZERO);
                                tokio::time::sleep(remaining).await;
                            }
                            None => std::future::pending::<()>().await,
                        }
                    };
                    tokio::select! {
                        biased;
                        _ = shutdown.cancelled() => return Ok(()),
                        _ = &mut notified => {},
                        _ = timer => {},
                    }
                }
            }
        }
    }

    fn scheduler_step(
        &self,
        driver: Option<&Arc<dyn ModelDriver>>,
        clock: impl Fn() -> DateTime<Utc>,
    ) -> Result<Step, AgentRunError> {
        let mut slot = self
            .inner
            .agent_runs
            .lock()
            .map_err(|_| AgentRunError::Storage)?;
        if slot.stopping {
            return Ok(Step::Stop);
        }
        let now = clock();
        let entries = self.inner.events.pending_schedules().map_err(storage)?;
        // Expiry is serviced even with a disabled provider or occupied slot.
        if let Some(entry) = entries
            .iter()
            .find(|entry| entry.expires_at_ms <= now.timestamp_millis())
        {
            self.schedule_source(entry)?;
            self.schedule_transition(entry, ScheduleState::Missed)?;
            return Ok(Step::Again);
        }
        if let Some(driver) = driver
            && slot.active.is_none()
            && let Some(entry) = entries
                .first()
                .filter(|entry| entry.due_at_ms <= now.timestamp_millis())
        {
            let command = self.schedule_source(entry)?;
            let claim_at = clock();
            if claim_at < command.due_at {
                return Ok(Step::Wait(Some(command.due_at)));
            }
            if claim_at >= command.expires_at {
                self.schedule_transition(entry, ScheduleState::Missed)?;
                return Ok(Step::Again);
            }
            self.schedule_transition(entry, ScheduleState::Claimed)?;
            // A durable claim is consumed even when admission/storage fails. Never
            // turn uncertain work back into pending or mint a replacement identity.
            self.start_agent_run_locked(
                StartAgentRunCommand {
                    request_id: entry.run_request_id.clone(),
                    session_id: entry.session_id.clone(),
                    text: command.text,
                    sort: None,
                },
                driver.clone(),
                &mut slot,
            )?;
            return Ok(Step::Again);
        }
        let next = entries
            .iter()
            .map(|entry| {
                if driver.is_some() && slot.active.is_none() {
                    entry.due_at_ms
                } else {
                    entry.expires_at_ms
                }
            })
            .min()
            .map(|ms| DateTime::from_timestamp_millis(ms).ok_or(AgentRunError::Storage))
            .transpose()?;
        Ok(Step::Wait(next))
    }

    fn schedule_entry(&self, query: &AgentRunQuery) -> Result<ScheduleEntry, AgentRunError> {
        validate_query(query)?;
        self.inner
            .events
            .schedule_entry(&query.session_id, &query.request_id)
            .map_err(storage)?
            .ok_or(AgentRunError::NotFound)
    }

    fn schedule_source(&self, entry: &ScheduleEntry) -> Result<ScheduleRunCommand, AgentRunError> {
        let source = self
            .inner
            .events
            .get_by_event_id(&entry.source_event_id)
            .map_err(storage)?
            .ok_or(AgentRunError::Storage)?;
        check_event(&source, entry, ScheduleState::Pending, None)?;
        let command: ScheduleRunCommand =
            serde_json::from_value(source.payload["command"].clone()).map_err(storage)?;
        if command.session_id != entry.session_id
            || command.request_id != entry.request_id
            || command.due_at.timestamp_millis() != entry.due_at_ms
            || command.expires_at.timestamp_millis() != entry.expires_at_ms
            || source.payload["run_request_id"].as_str() != Some(&entry.run_request_id)
            || command.text.len() > MAX_AGENT_RUN_TEXT_BYTES
            || normalize_input_text(&command.text).map_err(storage)? != command.text
        {
            return Err(AgentRunError::Storage);
        }
        validate_query(&identity(&command)).map_err(storage)?;
        validate_query(&AgentRunQuery {
            request_id: entry.run_request_id.clone(),
            session_id: entry.session_id.clone(),
        })
        .map_err(storage)?;
        validate_times(&command).map_err(storage)?;
        if entry.state != ScheduleState::Pending {
            let last = self
                .inner
                .events
                .get_by_event_id(&entry.last_event_id)
                .map_err(storage)?
                .ok_or(AgentRunError::Storage)?;
            if entry.state == ScheduleState::CancelRequested {
                let claim = self
                    .inner
                    .events
                    .get_by_event_id(last.causation_id.as_deref().ok_or(AgentRunError::Storage)?)
                    .map_err(storage)?
                    .ok_or(AgentRunError::Storage)?;
                check_event(
                    &claim,
                    entry,
                    ScheduleState::Claimed,
                    Some(&entry.source_event_id),
                )?;
                check_event(&last, entry, entry.state, Some(&claim.event_id))?;
            } else {
                check_event(&last, entry, entry.state, Some(&entry.source_event_id))?;
            }
        } else if entry.last_event_id != entry.source_event_id {
            return Err(AgentRunError::Storage);
        }
        Ok(command)
    }

    fn schedule_transition(
        &self,
        entry: &ScheduleEntry,
        state: ScheduleState,
    ) -> Result<(), AgentRunError> {
        self.append_and_publish(NewEvent {
            session_id: Some(entry.session_id.clone()),
            task_id: Some(format!("schedule_{}", entry.request_id)),
            actor: actor(state),
            kind: state.event_kind().into(),
            payload: json!({"version":1}),
            causation_id: Some(entry.last_event_id.clone()),
            correlation_id: None,
            span_id: None,
        })?;
        Ok(())
    }

    fn schedule_status(
        &self,
        entry: &ScheduleEntry,
        slot: &RunSlot,
    ) -> Result<ScheduleResponse, AgentRunError> {
        let command = self.schedule_source(entry)?;
        let run = if matches!(
            entry.state,
            ScheduleState::Claimed | ScheduleState::CancelRequested
        ) {
            match self.inspect_agent_run_locked(
                AgentRunQuery {
                    request_id: entry.run_request_id.clone(),
                    session_id: entry.session_id.clone(),
                },
                slot,
            ) {
                Ok(run) => Some(run),
                Err(AgentRunError::NotFound) => None,
                Err(error) => return Err(error),
            }
        } else {
            None
        };
        let status = match entry.state {
            ScheduleState::Pending => ScheduleStatus::Pending,
            ScheduleState::Cancelled => ScheduleStatus::Cancelled,
            ScheduleState::Missed => ScheduleStatus::Missed,
            ScheduleState::Claimed | ScheduleState::CancelRequested => {
                match run.as_ref().map(|run| run.status) {
                    Some(AgentRunStatus::Running) => ScheduleStatus::Running,
                    Some(AgentRunStatus::Unverified) => ScheduleStatus::Unverified,
                    Some(AgentRunStatus::Failed) => ScheduleStatus::Failed,
                    Some(AgentRunStatus::Interrupted) | None => ScheduleStatus::Interrupted,
                }
            }
        };
        let waiting_for = (status == ScheduleStatus::Pending).then(|| {
            match self.inner.scheduler_state.load(Ordering::SeqCst) {
                0 => ScheduleWaitReason::SchedulerStopped,
                1 => ScheduleWaitReason::ProviderDisabled,
                _ if Utc::now() < command.due_at => ScheduleWaitReason::DueTime,
                _ if slot.active.is_some() => ScheduleWaitReason::RuntimeBusy,
                _ => ScheduleWaitReason::Dispatch,
            }
        });
        Ok(ScheduleResponse {
            request_id: command.request_id,
            session_id: command.session_id,
            due_at: command.due_at,
            expires_at: command.expires_at,
            status,
            cancellation_requested: matches!(
                entry.state,
                ScheduleState::CancelRequested | ScheduleState::Cancelled
            ),
            waiting_for,
            run,
        })
    }
}

enum Step {
    Again,
    Stop,
    Wait(Option<DateTime<Utc>>),
}
struct SchedulerGuard(DittoKernel);
impl Drop for SchedulerGuard {
    fn drop(&mut self) {
        self.0.inner.scheduler_state.store(0, Ordering::SeqCst);
    }
}
fn identity(command: &ScheduleRunCommand) -> AgentRunQuery {
    AgentRunQuery {
        request_id: command.request_id.clone(),
        session_id: command.session_id.clone(),
    }
}
fn storage(_: impl std::fmt::Display) -> AgentRunError {
    AgentRunError::Storage
}
fn validate_times(command: &ScheduleRunCommand) -> Result<(), AgentRunError> {
    if command.due_at.timestamp_subsec_nanos() % 1_000_000 != 0
        || command.expires_at.timestamp_subsec_nanos() % 1_000_000 != 0
        || command.expires_at <= command.due_at
        || command.expires_at.signed_duration_since(command.due_at) > chrono::Duration::hours(24)
    {
        return Err(AgentRunError::Invalid(
            "times require millisecond precision and a positive grace window of at most 24 hours",
        ));
    }
    Ok(())
}
fn actor(state: ScheduleState) -> EventActor {
    match state {
        ScheduleState::Claimed | ScheduleState::Missed => EventActor::Scheduler,
        _ => EventActor::User,
    }
}
fn check_event(
    event: &EventRecord,
    entry: &ScheduleEntry,
    state: ScheduleState,
    cause: Option<&str>,
) -> Result<(), AgentRunError> {
    if event.session_id.as_deref() != Some(&entry.session_id)
        || event.task_id.as_deref() != Some(&format!("schedule_{}", entry.request_id))
        || event.actor != actor(state)
        || event.kind != state.event_kind()
        || event.causation_id.as_deref() != cause
        || event.correlation_id.is_some()
        || event.span_id.is_some()
        || event.payload["version"] != 1
    {
        return Err(AgentRunError::Storage);
    }
    Ok(())
}

#[cfg(test)]
mod tests;
