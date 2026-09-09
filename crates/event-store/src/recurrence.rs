use chrono::{DateTime, Utc};
use ditto_protocol::{
    EventActor, EventRecord, MAX_AGENT_RUN_TEXT_BYTES, MAX_PENDING_SCHEDULES,
    RepeatScheduleCommand, ScheduleRunCommand,
};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};

use crate::{EventStore, EventStoreError, ScheduleEntry};

pub const REPEAT_REQUESTED: &str = "schedule.repeat.requested";
pub const REPEAT_SKIPPED: &str = "schedule.repeat.skipped";
pub const REPEAT_CANCELLED: &str = "schedule.repeat.cancelled";
pub const OCCURRENCE_CLAIMED: &str = "schedule.occurrence.claimed";

pub(super) const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS repeat_index (
    session_id TEXT NOT NULL,
    request_id TEXT NOT NULL,
    source_event_id TEXT NOT NULL REFERENCES events(event_id),
    last_event_id TEXT NOT NULL REFERENCES events(event_id),
    first_due_ms INTEGER NOT NULL,
    first_expiry_ms INTEGER NOT NULL,
    interval_ms INTEGER NOT NULL,
    occurrences INTEGER NOT NULL,
    next_occurrence INTEGER NOT NULL,
    claimed INTEGER NOT NULL,
    missed INTEGER NOT NULL,
    last_child_id TEXT,
    state TEXT NOT NULL CHECK(state IN ('active','exhausted','cancelled')),
    PRIMARY KEY(session_id,request_id)
);
CREATE INDEX IF NOT EXISTS repeat_active_due
    ON repeat_index(first_due_ms + (next_occurrence - 1) * interval_ms,session_id,request_id)
    WHERE state = 'active';
"#;

pub(super) const SOURCE_INDEX: &str = r#"
CREATE INDEX IF NOT EXISTS events_schedules ON events(seq) WHERE kind GLOB 'schedule.*';
"#;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RepeatTiming {
    pub first_due_ms: i64,
    pub first_expiry_ms: i64,
    pub interval_ms: i64,
    pub occurrences: u32,
}

impl RepeatTiming {
    pub fn new(command: &RepeatScheduleCommand) -> Result<Self, &'static str> {
        if !(60..=31 * 86400).contains(&command.every_seconds)
            || !(2..=1000).contains(&command.occurrences)
        {
            return Err("repeat requires a 60-second to 31-day interval and 2 to 1000 occurrences");
        }
        let window = command.expires_at.signed_duration_since(command.due_at);
        if command.due_at.timestamp_subsec_nanos() % 1_000_000 != 0
            || command.expires_at.timestamp_subsec_nanos() % 1_000_000 != 0
            || window <= chrono::Duration::zero()
            || window > chrono::Duration::hours(24)
            || window > chrono::Duration::seconds(i64::from(command.every_seconds))
        {
            return Err(
                "repeat times need millisecond precision and a positive window no longer than its interval or 24 hours",
            );
        }
        let result = Self {
            first_due_ms: command.due_at.timestamp_millis(),
            first_expiry_ms: command.expires_at.timestamp_millis(),
            interval_ms: i64::from(command.every_seconds) * 1000,
            occurrences: command.occurrences,
        };
        if result.interval_ms * i64::from(result.occurrences - 1) > 365 * 86400 * 1000
            || result.window(result.occurrences).is_err()
        {
            return Err("repeat timetable must fit within 365 days");
        }
        Ok(result)
    }

    pub fn window(&self, occurrence: u32) -> Result<(i64, i64), EventStoreError> {
        if occurrence == 0 || occurrence > self.occurrences || self.interval_ms <= 0 {
            return Err(EventStoreError::InvalidSchedule);
        }
        let offset = i64::from(occurrence - 1)
            .checked_mul(self.interval_ms)
            .ok_or(EventStoreError::InvalidSchedule)?;
        let due = self
            .first_due_ms
            .checked_add(offset)
            .ok_or(EventStoreError::InvalidSchedule)?;
        let expiry = self
            .first_expiry_ms
            .checked_add(offset)
            .ok_or(EventStoreError::InvalidSchedule)?;
        if DateTime::<Utc>::from_timestamp_millis(due).is_none()
            || DateTime::<Utc>::from_timestamp_millis(expiry).is_none()
        {
            return Err(EventStoreError::InvalidSchedule);
        }
        Ok((due, expiry))
    }

    /// Arithmetic over a fixed anchor, independent of downtime and missed count.
    pub fn expired_through(&self, now_ms: i64) -> Result<u32, EventStoreError> {
        if self.interval_ms <= 0 || !(2..=1000).contains(&self.occurrences) {
            return Err(EventStoreError::InvalidSchedule);
        }
        if now_ms < self.first_expiry_ms {
            return Ok(0);
        }
        Ok((((i128::from(now_ms) - i128::from(self.first_expiry_ms))
            / i128::from(self.interval_ms)
            + 1)
        .min(i128::from(self.occurrences))) as u32)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepeatState {
    Active,
    Exhausted,
    Cancelled,
}

#[derive(Debug, Clone)]
pub struct RepeatEntry {
    pub session_id: String,
    pub request_id: String,
    pub source_event_id: String,
    pub last_event_id: String,
    pub timing: RepeatTiming,
    pub next_occurrence: u32,
    pub claimed: u32,
    pub missed: u32,
    pub last_child_id: Option<String>,
    pub state: RepeatState,
}

impl RepeatEntry {
    pub fn progress(&self) -> Value {
        progress(
            self.next_occurrence,
            self.claimed,
            self.missed,
            self.last_child_id.as_deref(),
        )
    }
    pub fn claim_progress(&self, child: &str) -> Value {
        progress(
            self.next_occurrence + 1,
            self.claimed + 1,
            self.missed,
            Some(child),
        )
    }
    pub fn skip_progress(&self, through: u32) -> Result<Value, EventStoreError> {
        if through < self.next_occurrence || through > self.timing.occurrences {
            return Err(EventStoreError::InvalidSchedule);
        }
        Ok(progress(
            through + 1,
            self.claimed,
            self.missed + through - self.next_occurrence + 1,
            self.last_child_id.as_deref(),
        ))
    }
}
fn progress(next: u32, claimed: u32, missed: u32, child: Option<&str>) -> Value {
    json!({"next_occurrence":next,"claimed":claimed,"missed":missed,"last_child_id":child})
}

fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RepeatEntry> {
    let state: String = row.get(12)?;
    Ok(RepeatEntry {
        session_id: row.get(0)?,
        request_id: row.get(1)?,
        source_event_id: row.get(2)?,
        last_event_id: row.get(3)?,
        timing: RepeatTiming {
            first_due_ms: row.get(4)?,
            first_expiry_ms: row.get(5)?,
            interval_ms: row.get(6)?,
            occurrences: row.get(7)?,
        },
        next_occurrence: row.get(8)?,
        claimed: row.get(9)?,
        missed: row.get(10)?,
        last_child_id: row.get(11)?,
        state: match state.as_str() {
            "active" => RepeatState::Active,
            "exhausted" => RepeatState::Exhausted,
            "cancelled" => RepeatState::Cancelled,
            _ => return Err(rusqlite::Error::InvalidQuery),
        },
    })
}
fn lookup(
    db: &Connection,
    session: &str,
    request: &str,
) -> Result<Option<RepeatEntry>, EventStoreError> {
    Ok(db
        .query_row(
            "SELECT * FROM repeat_index WHERE session_id = ?1 AND request_id = ?2",
            params![session, request],
            from_row,
        )
        .optional()?)
}

pub(super) fn future_count(db: &Connection) -> Result<usize, EventStoreError> {
    Ok(db.query_row(
        "SELECT COUNT(*) FROM (SELECT 1 FROM schedule_index WHERE state = 'pending'
        UNION ALL SELECT 1 FROM repeat_index WHERE state = 'active' LIMIT ?1)",
        [MAX_PENDING_SCHEDULES as i64 + 1],
        |row| row.get(0),
    )?)
}

impl EventStore {
    pub fn future_schedule_count(&self) -> Result<usize, EventStoreError> {
        let db = self.connection()?;
        future_count(&db)
    }
    pub fn repeat_entry(
        &self,
        session: &str,
        request: &str,
    ) -> Result<Option<RepeatEntry>, EventStoreError> {
        let db = self.connection()?;
        lookup(&db, session, request)
    }
    pub fn active_repeats(&self) -> Result<Vec<RepeatEntry>, EventStoreError> {
        let db = self.connection()?;
        let mut stmt = db.prepare("SELECT * FROM repeat_index INDEXED BY repeat_active_due WHERE state = 'active'
            ORDER BY first_due_ms + (next_occurrence - 1) * interval_ms,session_id,request_id LIMIT ?1")?;
        let entries = stmt
            .query_map([MAX_PENDING_SCHEDULES as i64 + 1], from_row)?
            .collect::<Result<Vec<_>, _>>()?;
        if entries.len() > MAX_PENDING_SCHEDULES {
            return Err(EventStoreError::InvalidSchedule);
        }
        Ok(entries)
    }

    /// Rebind all cached scheduling fields to their immutable source and checkpoint.
    pub fn verified_repeat(
        &self,
        entry: &RepeatEntry,
    ) -> Result<RepeatScheduleCommand, EventStoreError> {
        let source = self
            .get_by_event_id(&entry.source_event_id)?
            .ok_or(EventStoreError::InvalidSchedule)?;
        let command = requested_command(&source)?;
        if source.task_id.as_deref() != Some(&format!("repeat_{}", entry.request_id))
            || command.session_id != entry.session_id
            || command.request_id != entry.request_id
            || RepeatTiming::new(&command).map_err(|_| EventStoreError::InvalidSchedule)?
                != entry.timing
            || entry.next_occurrence == 0
            || entry.next_occurrence > entry.timing.occurrences + 1
            || entry.claimed.checked_add(entry.missed) != Some(entry.next_occurrence - 1)
            || (entry.claimed == 0) != entry.last_child_id.is_none()
        {
            return Err(EventStoreError::InvalidSchedule);
        }
        if entry.last_event_id == entry.source_event_id {
            if entry.progress() != progress(1, 0, 0, None) || entry.state != RepeatState::Active {
                return Err(EventStoreError::InvalidSchedule);
            }
        } else {
            let last = self
                .get_by_event_id(&entry.last_event_id)?
                .ok_or(EventStoreError::InvalidSchedule)?;
            check_transition_scope(&last, entry)?;
            let expected_state = if last.kind == REPEAT_CANCELLED {
                RepeatState::Cancelled
            } else if entry.next_occurrence > entry.timing.occurrences {
                RepeatState::Exhausted
            } else {
                RepeatState::Active
            };
            if last.payload["progress"] != entry.progress()
                || entry.state != expected_state
                || last.seq <= source.seq
            {
                return Err(EventStoreError::InvalidSchedule);
            }
        }
        Ok(command)
    }

    pub fn occurrence_source(
        &self,
        entry: &ScheduleEntry,
        claim: &EventRecord,
    ) -> Result<ScheduleRunCommand, EventStoreError> {
        let source_id = string(&claim.payload, "source_event_id")?;
        let source = self
            .get_by_event_id(source_id)?
            .ok_or(EventStoreError::InvalidSchedule)?;
        let parent = requested_command(&source)?;
        let ordinal = number(&claim.payload, "occurrence")?;
        let timing = RepeatTiming::new(&parent).map_err(|_| EventStoreError::InvalidSchedule)?;
        let (due, expiry) = timing.window(ordinal)?;
        let observed = integer(&claim.payload, "observed_at_ms")?;
        if claim.kind != OCCURRENCE_CLAIMED
            || claim.actor != EventActor::Scheduler
            || claim.payload["version"] != 1
            || claim.session_id.as_deref() != Some(&parent.session_id)
            || parent.session_id != entry.session_id
            || claim.task_id.as_deref() != Some(&format!("schedule_{}", entry.request_id))
            || claim.correlation_id.is_some()
            || claim.span_id.is_some()
            || string(&claim.payload, "parent_request_id")? != parent.request_id
            || string(&claim.payload, "run_request_id")? != entry.run_request_id
            || due != entry.due_at_ms
            || expiry != entry.expires_at_ms
            || observed < due
            || observed >= expiry
        {
            return Err(EventStoreError::InvalidSchedule);
        }
        let prior = self
            .get_by_event_id(
                claim
                    .causation_id
                    .as_deref()
                    .ok_or(EventStoreError::InvalidSchedule)?,
            )?
            .ok_or(EventStoreError::InvalidSchedule)?;
        let next = if prior.event_id == source.event_id {
            1
        } else {
            if !matches!(prior.kind.as_str(), REPEAT_SKIPPED | OCCURRENCE_CLAIMED)
                || prior.actor != EventActor::Scheduler
                || prior.session_id != source.session_id
                || prior.payload["source_event_id"].as_str() != Some(source_id)
            {
                return Err(EventStoreError::InvalidSchedule);
            }
            number(&prior.payload["progress"], "next_occurrence")?
        };
        if next != ordinal || prior.seq >= claim.seq {
            return Err(EventStoreError::InvalidSchedule);
        }
        Ok(ScheduleRunCommand {
            request_id: entry.request_id.clone(),
            session_id: parent.session_id,
            text: parent.text,
            due_at: DateTime::from_timestamp_millis(due).ok_or(EventStoreError::InvalidSchedule)?,
            expires_at: DateTime::from_timestamp_millis(expiry)
                .ok_or(EventStoreError::InvalidSchedule)?,
        })
    }
}

fn requested_command(event: &EventRecord) -> Result<RepeatScheduleCommand, EventStoreError> {
    let command: RepeatScheduleCommand = serde_json::from_value(event.payload["command"].clone())?;
    if event.kind != REPEAT_REQUESTED
        || event.actor != EventActor::User
        || event.payload["version"] != 1
        || event.session_id.as_deref() != Some(&command.session_id)
        || event.task_id.as_deref() != Some(&format!("repeat_{}", command.request_id))
        || event.causation_id.is_some()
        || event.correlation_id.is_some()
        || event.span_id.is_some()
        || command.text.is_empty()
        || command.text.trim() != command.text
        || command.text.len() > MAX_AGENT_RUN_TEXT_BYTES
        || !canonical_id(&command.request_id)
    {
        return Err(EventStoreError::InvalidSchedule);
    }
    RepeatTiming::new(&command).map_err(|_| EventStoreError::InvalidSchedule)?;
    Ok(command)
}
fn check_transition_scope(event: &EventRecord, entry: &RepeatEntry) -> Result<(), EventStoreError> {
    let expected_actor = if event.kind == REPEAT_CANCELLED {
        EventActor::User
    } else {
        EventActor::Scheduler
    };
    let valid_task = if event.kind == OCCURRENCE_CLAIMED {
        event
            .task_id
            .as_deref()
            .is_some_and(|task| task.starts_with("schedule_"))
            && event.payload["parent_request_id"].as_str() == Some(&entry.request_id)
    } else {
        event.task_id.as_deref() == Some(&format!("repeat_{}", entry.request_id))
    };
    if !matches!(
        event.kind.as_str(),
        REPEAT_SKIPPED | REPEAT_CANCELLED | OCCURRENCE_CLAIMED
    ) || event.actor != expected_actor
        || !valid_task
        || event.payload["version"] != 1
        || event.session_id.as_deref() != Some(&entry.session_id)
        || event.payload["source_event_id"].as_str() != Some(&entry.source_event_id)
        || event.causation_id.is_none()
        || event.correlation_id.is_some()
        || event.span_id.is_some()
    {
        return Err(EventStoreError::InvalidSchedule);
    }
    Ok(())
}

pub(super) fn project(db: &Connection, event: &EventRecord) -> Result<(), EventStoreError> {
    if event.kind == REPEAT_REQUESTED {
        let command = requested_command(event)?;
        let timing = RepeatTiming::new(&command).map_err(|_| EventStoreError::InvalidSchedule)?;
        if future_count(db)? >= MAX_PENDING_SCHEDULES {
            return Err(EventStoreError::InvalidSchedule);
        }
        db.execute(
            "INSERT INTO repeat_index VALUES (?1,?2,?3,?3,?4,?5,?6,?7,1,0,0,NULL,'active')",
            params![
                command.session_id,
                command.request_id,
                event.event_id,
                timing.first_due_ms,
                timing.first_expiry_ms,
                timing.interval_ms,
                timing.occurrences
            ],
        )?;
        return Ok(());
    }
    let session = event
        .session_id
        .as_deref()
        .ok_or(EventStoreError::InvalidSchedule)?;
    let parent_id = if event.kind == OCCURRENCE_CLAIMED {
        string(&event.payload, "parent_request_id")?
    } else {
        event
            .task_id
            .as_deref()
            .and_then(|s| s.strip_prefix("repeat_"))
            .ok_or(EventStoreError::InvalidSchedule)?
    };
    let old = lookup(db, session, parent_id)?.ok_or(EventStoreError::InvalidSchedule)?;
    check_transition_scope(event, &old)?;
    if event.causation_id.as_deref() != Some(&old.last_event_id)
        || old.state == RepeatState::Cancelled
    {
        return Err(EventStoreError::InvalidSchedule);
    }
    let (next, claimed, missed, child, state) = match event.kind.as_str() {
        REPEAT_CANCELLED => (
            old.next_occurrence,
            old.claimed,
            old.missed,
            old.last_child_id.clone(),
            "cancelled",
        ),
        REPEAT_SKIPPED if old.state == RepeatState::Active => {
            let from = number(&event.payload, "from_occurrence")?;
            let through = number(&event.payload, "through_occurrence")?;
            if from != old.next_occurrence
                || through
                    != old
                        .timing
                        .expired_through(integer(&event.payload, "observed_at_ms")?)?
                || through < from
            {
                return Err(EventStoreError::InvalidSchedule);
            }
            (
                through + 1,
                old.claimed,
                old.missed + through - from + 1,
                old.last_child_id.clone(),
                if through == old.timing.occurrences {
                    "exhausted"
                } else {
                    "active"
                },
            )
        }
        OCCURRENCE_CLAIMED if old.state == RepeatState::Active => {
            let occurrence = number(&event.payload, "occurrence")?;
            let run = string(&event.payload, "run_request_id")?;
            let child = event
                .task_id
                .as_deref()
                .and_then(|s| s.strip_prefix("schedule_"))
                .ok_or(EventStoreError::InvalidSchedule)?;
            let (due, expiry) = old.timing.window(occurrence)?;
            let observed = integer(&event.payload, "observed_at_ms")?;
            if occurrence != old.next_occurrence
                || observed < due
                || observed >= expiry
                || !canonical_id(run)
                || !canonical_id(child)
            {
                return Err(EventStoreError::InvalidSchedule);
            }
            db.execute(
                "INSERT INTO schedule_index VALUES (?1,?2,?3,?3,?4,?5,?6,'claimed')",
                params![session, child, event.event_id, run, due, expiry],
            )?;
            (
                occurrence + 1,
                old.claimed + 1,
                old.missed,
                Some(child.to_owned()),
                if occurrence == old.timing.occurrences {
                    "exhausted"
                } else {
                    "active"
                },
            )
        }
        _ => return Err(EventStoreError::InvalidSchedule),
    };
    if event.payload["progress"] != progress(next, claimed, missed, child.as_deref()) {
        return Err(EventStoreError::InvalidSchedule);
    }
    db.execute("UPDATE repeat_index SET last_event_id = ?1,next_occurrence = ?2,claimed = ?3,missed = ?4,last_child_id = ?5,state = ?6 WHERE session_id = ?7 AND request_id = ?8",
        params![event.event_id,next,claimed,missed,child,state,session,parent_id])?;
    Ok(())
}

fn canonical_id(value: &str) -> bool {
    value
        .parse::<ulid::Ulid>()
        .is_ok_and(|id| id.to_string() == value)
}
fn string<'a>(value: &'a Value, field: &str) -> Result<&'a str, EventStoreError> {
    value[field]
        .as_str()
        .ok_or(EventStoreError::InvalidSchedule)
}
fn number(value: &Value, field: &str) -> Result<u32, EventStoreError> {
    value[field]
        .as_u64()
        .and_then(|n| u32::try_from(n).ok())
        .ok_or(EventStoreError::InvalidSchedule)
}
fn integer(value: &Value, field: &str) -> Result<i64, EventStoreError> {
    value[field]
        .as_i64()
        .ok_or(EventStoreError::InvalidSchedule)
}

#[cfg(test)]
mod tests;
