use std::sync::Arc;

use chrono::{DateTime, Utc};
use ditto_event_store::recurrence::{
    OCCURRENCE_CLAIMED, REPEAT_CANCELLED, REPEAT_REQUESTED, REPEAT_SKIPPED, RepeatEntry,
    RepeatState, RepeatTiming,
};
use ditto_model::ModelDriver;
use ditto_protocol::{
    AgentRunQuery, EventActor, MAX_AGENT_RUN_TEXT_BYTES, MAX_PENDING_SCHEDULES, NewEvent,
    RepeatScheduleCommand, RepeatScheduleResponse, RepeatScheduleStatus, ScheduleStatus,
    StartAgentRunCommand,
};
use serde_json::json;

use crate::{
    AgentRunError, DittoKernel,
    agent_run::{RunSlot, validate_query},
    normalize_input_text,
};

impl DittoKernel {
    pub fn repeat_schedule(
        &self,
        mut command: RepeatScheduleCommand,
    ) -> Result<RepeatScheduleResponse, AgentRunError> {
        validate_query(&identity(&command))?;
        if command.text.len() > MAX_AGENT_RUN_TEXT_BYTES {
            return Err(AgentRunError::Invalid("text exceeds 16 KiB"));
        }
        command.text = normalize_input_text(&command.text)
            .map_err(|_| AgentRunError::Invalid("text is empty or invalid"))?;
        let timing = RepeatTiming::new(&command).map_err(AgentRunError::Invalid)?;
        let slot = self.inner.agent_runs.lock().map_err(storage)?;
        if let Some(entry) = self
            .inner
            .events
            .repeat_entry(&command.session_id, &command.request_id)
            .map_err(storage)?
        {
            if self.repeat_source(&entry)? != command {
                return Err(AgentRunError::Conflict);
            }
            return self.repeat_status(&entry, &slot, true);
        }
        if slot.stopping {
            return Err(AgentRunError::Stopping);
        }
        let now = Utc::now();
        if command.due_at <= now
            || timing.window(command.occurrences).map_err(storage)?.0
                > (now + chrono::Duration::days(365)).timestamp_millis()
        {
            return Err(AgentRunError::Invalid(
                "first due time must be future and final due time within 365 days",
            ));
        }
        if self.inner.events.future_schedule_count().map_err(storage)? >= MAX_PENDING_SCHEDULES {
            return Err(AgentRunError::ScheduleFull);
        }
        self.append_and_publish(NewEvent {
            session_id: Some(command.session_id.clone()),
            task_id: Some(format!("repeat_{}", command.request_id)),
            actor: EventActor::User,
            kind: REPEAT_REQUESTED.into(),
            payload: json!({"version":1,"command":command}),
            causation_id: None,
            correlation_id: None,
            span_id: None,
        })?;
        self.inner.scheduler_wake.notify_one();
        self.repeat_status(&self.repeat_entry(&identity(&command))?, &slot, true)
    }

    pub fn inspect_repeat(
        &self,
        query: AgentRunQuery,
    ) -> Result<RepeatScheduleResponse, AgentRunError> {
        let slot = self.inner.agent_runs.lock().map_err(storage)?;
        self.repeat_status(&self.repeat_entry(&query)?, &slot, true)
    }

    pub fn list_active_repeats(
        &self,
        session: &str,
    ) -> Result<Vec<RepeatScheduleResponse>, AgentRunError> {
        ditto_retrieval::SessionId::new(session)
            .map_err(|_| AgentRunError::Invalid("session ID is not canonical"))?;
        let slot = self.inner.agent_runs.lock().map_err(storage)?;
        self.inner
            .events
            .active_repeats()
            .map_err(storage)?
            .iter()
            .filter(|e| e.session_id == session)
            .map(|e| self.repeat_status(e, &slot, false))
            .collect()
    }

    pub fn cancel_repeat(
        &self,
        query: AgentRunQuery,
    ) -> Result<RepeatScheduleResponse, AgentRunError> {
        let slot = self.inner.agent_runs.lock().map_err(storage)?;
        let entry = self.repeat_entry(&query)?;
        let response = self.repeat_status(&entry, &slot, true)?;
        let running = response
            .last_occurrence
            .as_ref()
            .is_some_and(|child| child.status == ScheduleStatus::Running);
        if entry.state == RepeatState::Cancelled
            || (entry.state == RepeatState::Exhausted && !running)
        {
            return Ok(response);
        }
        // The durable parent cancellation covers future claims and this active
        // child even if the process dies before it can signal the token.
        self.append_and_publish(NewEvent {
            session_id: Some(entry.session_id.clone()), task_id: Some(format!("repeat_{}", entry.request_id)),
            actor: EventActor::User, kind: REPEAT_CANCELLED.into(),
            payload: json!({"version":1,"source_event_id":entry.source_event_id,"progress":entry.progress()}),
            causation_id: Some(entry.last_event_id.clone()), correlation_id: None, span_id: None,
        })?;
        if running && let Some(active) = &slot.active {
            active.cancellation.cancel();
        }
        self.inner.scheduler_wake.notify_one();
        self.repeat_status(&self.repeat_entry(&query)?, &slot, true)
    }

    pub(crate) fn skip_repeat(
        &self,
        entry: &RepeatEntry,
        observed_at_ms: i64,
    ) -> Result<(), AgentRunError> {
        self.repeat_source(entry)?;
        let through = entry
            .timing
            .expired_through(observed_at_ms)
            .map_err(storage)?;
        let progress = entry.skip_progress(through).map_err(storage)?;
        self.append_and_publish(NewEvent {
            session_id: Some(entry.session_id.clone()), task_id: Some(format!("repeat_{}", entry.request_id)),
            actor: EventActor::Scheduler, kind: REPEAT_SKIPPED.into(),
            payload: json!({"version":1,"source_event_id":entry.source_event_id,"observed_at_ms":observed_at_ms,
                "from_occurrence":entry.next_occurrence,"through_occurrence":through,"progress":progress}),
            causation_id: Some(entry.last_event_id.clone()), correlation_id: None, span_id: None,
        })?;
        Ok(())
    }

    pub(crate) fn dispatch_repeat(
        &self,
        entry: &RepeatEntry,
        driver: Arc<dyn ModelDriver>,
        slot: &mut RunSlot,
        clock: &impl Fn() -> DateTime<Utc>,
    ) -> Result<Option<DateTime<Utc>>, AgentRunError> {
        let command = self.repeat_source(entry)?;
        let (due, expiry) = entry
            .timing
            .window(entry.next_occurrence)
            .map_err(storage)?;
        let child = ulid::Ulid::new().to_string();
        let run = ulid::Ulid::new().to_string();
        if self
            .inner
            .events
            .schedule_entry(&entry.session_id, &child)
            .map_err(storage)?
            .is_some()
            || self.inner.events.is_scheduled_run(&run).map_err(storage)?
            || self
                .inner
                .events
                .task_turn_boundary(&entry.session_id, &format!("run_{run}"))
                .map_err(storage)?
                .is_some()
        {
            return Err(AgentRunError::Storage);
        }
        let observed_at_ms = clock().timestamp_millis();
        if observed_at_ms < due {
            return Ok(Some(
                DateTime::from_timestamp_millis(due).ok_or(AgentRunError::Storage)?,
            ));
        }
        if observed_at_ms >= expiry {
            self.skip_repeat(entry, observed_at_ms)?;
            return Ok(None);
        }
        self.append_and_publish(NewEvent {
            session_id: Some(entry.session_id.clone()), task_id: Some(format!("schedule_{child}")),
            actor: EventActor::Scheduler, kind: OCCURRENCE_CLAIMED.into(),
            payload: json!({"version":1,"parent_request_id":entry.request_id,"source_event_id":entry.source_event_id,
                "occurrence":entry.next_occurrence,"run_request_id":run,"observed_at_ms":observed_at_ms,
                "progress":entry.claim_progress(&child)}),
            causation_id: Some(entry.last_event_id.clone()), correlation_id: None, span_id: None,
        })?;
        self.start_agent_run_locked(
            StartAgentRunCommand {
                request_id: run,
                session_id: entry.session_id.clone(),
                text: command.text,
                sort: None,
            },
            driver,
            slot,
        )?;
        Ok(None)
    }

    fn repeat_entry(&self, query: &AgentRunQuery) -> Result<RepeatEntry, AgentRunError> {
        validate_query(query)?;
        self.inner
            .events
            .repeat_entry(&query.session_id, &query.request_id)
            .map_err(storage)?
            .ok_or(AgentRunError::NotFound)
    }
    fn repeat_source(&self, entry: &RepeatEntry) -> Result<RepeatScheduleCommand, AgentRunError> {
        let command = self.inner.events.verified_repeat(entry).map_err(storage)?;
        validate_query(&identity(&command)).map_err(storage)?;
        Ok(command)
    }
    fn repeat_status(
        &self,
        entry: &RepeatEntry,
        slot: &RunSlot,
        expand: bool,
    ) -> Result<RepeatScheduleResponse, AgentRunError> {
        let command = self.repeat_source(entry)?;
        let next_due = if entry.state == RepeatState::Active {
            Some(
                DateTime::from_timestamp_millis(
                    entry
                        .timing
                        .window(entry.next_occurrence)
                        .map_err(storage)?
                        .0,
                )
                .ok_or(AgentRunError::Storage)?,
            )
        } else {
            None
        };
        let child = if expand {
            entry
                .last_child_id
                .as_ref()
                .map(|id| {
                    let child = self.schedule_entry(&AgentRunQuery {
                        request_id: id.clone(),
                        session_id: entry.session_id.clone(),
                    })?;
                    let source = self
                        .inner
                        .events
                        .get_by_event_id(&child.source_event_id)
                        .map_err(storage)?
                        .ok_or(AgentRunError::Storage)?;
                    if source.kind != OCCURRENCE_CLAIMED
                        || source.payload["source_event_id"].as_str()
                            != Some(&entry.source_event_id)
                        || source.payload["parent_request_id"].as_str() != Some(&entry.request_id)
                    {
                        return Err(AgentRunError::Storage);
                    }
                    self.schedule_status(&child, slot)
                })
                .transpose()?
        } else {
            None
        };
        Ok(RepeatScheduleResponse {
            request_id: command.request_id,
            session_id: command.session_id,
            due_at: command.due_at,
            expires_at: command.expires_at,
            every_seconds: command.every_seconds,
            occurrences: command.occurrences,
            status: match entry.state {
                RepeatState::Active => RepeatScheduleStatus::Active,
                RepeatState::Exhausted => RepeatScheduleStatus::Exhausted,
                RepeatState::Cancelled => RepeatScheduleStatus::Cancelled,
            },
            claimed_occurrences: entry.claimed,
            missed_occurrences: entry.missed,
            next_occurrence: next_due.map(|_| entry.next_occurrence),
            next_due_at: next_due,
            waiting_for: next_due.map(|due| self.schedule_wait_reason(due, slot)),
            last_occurrence_id: entry.last_child_id.clone(),
            last_occurrence: child,
        })
    }
}
fn identity(command: &RepeatScheduleCommand) -> AgentRunQuery {
    AgentRunQuery {
        request_id: command.request_id.clone(),
        session_id: command.session_id.clone(),
    }
}
fn storage(_: impl std::fmt::Display) -> AgentRunError {
    AgentRunError::Storage
}
