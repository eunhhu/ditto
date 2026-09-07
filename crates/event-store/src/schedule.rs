use ditto_protocol::{EventActor, EventRecord, MAX_PENDING_SCHEDULES, ScheduleRunCommand};
use rusqlite::{Connection, OptionalExtension, params};

use crate::{EventStore, EventStoreError, raw_event_record_from_row};

pub(super) const MIGRATION_V5: &str = r#"
CREATE TABLE IF NOT EXISTS schedule_index (
    session_id TEXT NOT NULL,
    request_id TEXT NOT NULL,
    source_event_id TEXT NOT NULL REFERENCES events(event_id),
    last_event_id TEXT NOT NULL REFERENCES events(event_id),
    run_request_id TEXT NOT NULL UNIQUE,
    due_at_ms INTEGER NOT NULL,
    expires_at_ms INTEGER NOT NULL,
    state TEXT NOT NULL CHECK(state IN ('pending','claimed','cancel_requested','cancelled','missed')),
    PRIMARY KEY(session_id,request_id)
);
CREATE INDEX IF NOT EXISTS schedule_pending ON schedule_index(due_at_ms,session_id,request_id)
    WHERE state = 'pending';
CREATE INDEX IF NOT EXISTS events_schedules ON events(seq)
    WHERE kind IN ('schedule.requested','schedule.claimed','schedule.cancel_requested',
                   'schedule.cancelled','schedule.missed');
"#;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScheduleState {
    Pending,
    Claimed,
    CancelRequested,
    Cancelled,
    Missed,
}

impl ScheduleState {
    pub fn event_kind(self) -> &'static str {
        match self {
            Self::Pending => "schedule.requested",
            Self::Claimed => "schedule.claimed",
            Self::CancelRequested => "schedule.cancel_requested",
            Self::Cancelled => "schedule.cancelled",
            Self::Missed => "schedule.missed",
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Claimed => "claimed",
            Self::CancelRequested => "cancel_requested",
            Self::Cancelled => "cancelled",
            Self::Missed => "missed",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ScheduleEntry {
    pub session_id: String,
    pub request_id: String,
    pub source_event_id: String,
    pub last_event_id: String,
    pub run_request_id: String,
    pub due_at_ms: i64,
    pub expires_at_ms: i64,
    pub state: ScheduleState,
}

fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ScheduleEntry> {
    let state: String = row.get(7)?;
    Ok(ScheduleEntry {
        session_id: row.get(0)?,
        request_id: row.get(1)?,
        source_event_id: row.get(2)?,
        last_event_id: row.get(3)?,
        run_request_id: row.get(4)?,
        due_at_ms: row.get(5)?,
        expires_at_ms: row.get(6)?,
        state: match state.as_str() {
            "pending" => ScheduleState::Pending,
            "claimed" => ScheduleState::Claimed,
            "cancel_requested" => ScheduleState::CancelRequested,
            "cancelled" => ScheduleState::Cancelled,
            "missed" => ScheduleState::Missed,
            _ => return Err(rusqlite::Error::InvalidQuery),
        },
    })
}

impl EventStore {
    pub fn schedule_entry(
        &self,
        session: &str,
        request: &str,
    ) -> Result<Option<ScheduleEntry>, EventStoreError> {
        let db = self.connection()?;
        lookup(&db, session, request)
    }

    /// Retain only bounded headers, never prompts or completed history.
    pub fn pending_schedules(&self) -> Result<Vec<ScheduleEntry>, EventStoreError> {
        let db = self.connection()?;
        let mut stmt = db.prepare(
            "SELECT * FROM schedule_index WHERE state = 'pending'
             ORDER BY due_at_ms,session_id,request_id LIMIT ?1",
        )?;
        let entries = stmt
            .query_map([MAX_PENDING_SCHEDULES as i64 + 1], from_row)?
            .collect::<Result<Vec<_>, _>>()?;
        if entries.len() > MAX_PENDING_SCHEDULES {
            return Err(EventStoreError::InvalidSchedule);
        }
        Ok(entries)
    }

    pub fn is_scheduled_run(&self, request: &str) -> Result<bool, EventStoreError> {
        Ok(self.connection()?.query_row(
            "SELECT EXISTS(SELECT 1 FROM schedule_index WHERE run_request_id = ?1)",
            [request],
            |row| row.get(0),
        )?)
    }
}

fn lookup(
    db: &Connection,
    session: &str,
    request: &str,
) -> Result<Option<ScheduleEntry>, EventStoreError> {
    Ok(db
        .query_row(
            "SELECT * FROM schedule_index WHERE session_id = ?1 AND request_id = ?2",
            params![session, request],
            from_row,
        )
        .optional()?)
}

pub(super) fn is_schedule_kind(kind: &str) -> bool {
    matches!(
        kind,
        "schedule.requested"
            | "schedule.claimed"
            | "schedule.cancel_requested"
            | "schedule.cancelled"
            | "schedule.missed"
    )
}

pub(super) fn project(db: &Connection, event: &EventRecord) -> Result<(), EventStoreError> {
    let invalid = || EventStoreError::InvalidSchedule;
    let session = event.session_id.as_deref().ok_or_else(invalid)?;
    let request = event
        .task_id
        .as_deref()
        .and_then(|id| id.strip_prefix("schedule_"))
        .ok_or_else(invalid)?;
    if event.correlation_id.is_some() || event.span_id.is_some() || event.payload["version"] != 1 {
        return Err(invalid());
    }
    if event.kind == "schedule.requested" {
        let command: ScheduleRunCommand = serde_json::from_value(event.payload["command"].clone())?;
        let run = event.payload["run_request_id"]
            .as_str()
            .ok_or_else(invalid)?;
        if event.actor != EventActor::User
            || event.causation_id.is_some()
            || command.request_id != request
            || command.session_id != session
            || command.due_at >= command.expires_at
            || command.due_at.timestamp_subsec_nanos() % 1_000_000 != 0
            || command.expires_at.timestamp_subsec_nanos() % 1_000_000 != 0
            || run
                .parse::<ulid::Ulid>()
                .ok()
                .is_none_or(|id| id.to_string() != run)
        {
            return Err(invalid());
        }
        let pending: i64 = db.query_row(
            "SELECT COUNT(*) FROM (SELECT 1 FROM schedule_index WHERE state = 'pending' LIMIT ?1)",
            [MAX_PENDING_SCHEDULES as i64],
            |row| row.get(0),
        )?;
        if pending >= MAX_PENDING_SCHEDULES as i64 {
            return Err(invalid());
        }
        db.execute(
            "INSERT INTO schedule_index VALUES (?1,?2,?3,?3,?4,?5,?6,'pending')",
            params![
                session,
                request,
                event.event_id,
                run,
                command.due_at.timestamp_millis(),
                command.expires_at.timestamp_millis()
            ],
        )?;
    } else {
        let old = lookup(db, session, request)?.ok_or_else(invalid)?;
        let (state, actor) = match event.kind.as_str() {
            "schedule.claimed" if old.state == ScheduleState::Pending => {
                (ScheduleState::Claimed, EventActor::Scheduler)
            }
            "schedule.missed" if old.state == ScheduleState::Pending => {
                (ScheduleState::Missed, EventActor::Scheduler)
            }
            "schedule.cancelled" if old.state == ScheduleState::Pending => {
                (ScheduleState::Cancelled, EventActor::User)
            }
            "schedule.cancel_requested" if old.state == ScheduleState::Claimed => {
                (ScheduleState::CancelRequested, EventActor::User)
            }
            _ => return Err(invalid()),
        };
        if event.actor != actor || event.causation_id.as_deref() != Some(&old.last_event_id) {
            return Err(invalid());
        }
        db.execute("UPDATE schedule_index SET state = ?1, last_event_id = ?2 WHERE session_id = ?3 AND request_id = ?4",
            params![state.as_str(),event.event_id,session,request])?;
    }
    Ok(())
}

/// Event history is the authority even when this compact index is lost.
pub(super) fn rebuild(db: &mut Connection) -> Result<(), EventStoreError> {
    let tx = db.transaction()?;
    tx.execute_batch(MIGRATION_V5)?;
    tx.execute("DELETE FROM schedule_index", [])?;
    {
        let mut stmt = tx.prepare(
            "SELECT seq,event_id,recorded_at,session_id,task_id,actor,kind,payload_json,causation_id,correlation_id,span_id
             FROM events INDEXED BY events_schedules WHERE kind IN ('schedule.requested','schedule.claimed','schedule.cancel_requested','schedule.cancelled','schedule.missed') ORDER BY seq",
        )?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            project(&tx, &raw_event_record_from_row(row)?.try_into()?)?;
        }
    }
    tx.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ditto_protocol::NewEvent;
    use serde_json::json;

    #[test]
    fn schedule_append_and_index_transition_are_atomic_and_pending_reads_use_the_index() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("state.db");
        let store = EventStore::open(&path).unwrap();
        let request = ulid::Ulid::new().to_string();
        let run = ulid::Ulid::new().to_string();
        let source = store
            .append(NewEvent {
                session_id: Some("personal".into()),
                task_id: Some(format!("schedule_{request}")),
                actor: EventActor::User,
                kind: "schedule.requested".into(),
                payload: json!({"version":1,"command":{
                "request_id":request,"session_id":"personal","text":"a bounded request",
                "due_at":"2026-09-08T00:00:00Z","expires_at":"2026-09-08T01:00:00Z"
            },"run_request_id":run}),
                causation_id: None,
                correlation_id: None,
                span_id: None,
            })
            .unwrap();
        let mut claim = NewEvent {
            session_id: source.session_id.clone(),
            task_id: source.task_id.clone(),
            actor: EventActor::User,
            kind: "schedule.claimed".into(),
            payload: json!({"version":1}),
            causation_id: Some(source.event_id.clone()),
            correlation_id: None,
            span_id: None,
        };
        assert!(matches!(
            store.append(claim.clone()),
            Err(EventStoreError::InvalidSchedule)
        ));
        assert_eq!(store.count().unwrap(), 1);
        assert_eq!(store.pending_schedules().unwrap().len(), 1);
        claim.actor = EventActor::Scheduler;
        let claimed = store.append(claim).unwrap();
        assert_eq!(store.count().unwrap(), 2);
        assert!(store.pending_schedules().unwrap().is_empty());
        assert!(store.is_scheduled_run(&run).unwrap());
        {
            let db = store.connection().unwrap();
            for (query, expected) in [
                (
                    "EXPLAIN QUERY PLAN SELECT * FROM schedule_index WHERE state = 'pending' ORDER BY due_at_ms,session_id,request_id LIMIT 101",
                    "schedule_pending",
                ),
                (
                    "EXPLAIN QUERY PLAN SELECT * FROM schedule_index WHERE session_id = 'personal' AND request_id = 'id'",
                    "SEARCH schedule_index",
                ),
                (
                    "EXPLAIN QUERY PLAN SELECT 1 FROM schedule_index WHERE run_request_id = 'id'",
                    "SEARCH schedule_index",
                ),
                (
                    "EXPLAIN QUERY PLAN SELECT seq FROM events INDEXED BY events_schedules WHERE kind IN ('schedule.requested','schedule.claimed','schedule.cancel_requested','schedule.cancelled','schedule.missed') ORDER BY seq",
                    "events_schedules",
                ),
            ] {
                let mut stmt = db.prepare(query).unwrap();
                let plan = stmt
                    .query_map([], |row| row.get::<_, String>(3))
                    .unwrap()
                    .collect::<Result<Vec<_>, _>>()
                    .unwrap()
                    .join(" ");
                assert!(plan.contains(expected), "{plan}");
                assert!(!plan.contains("TEMP B-TREE"), "{plan}");
            }
            db.execute("DELETE FROM schedule_index", []).unwrap();
        }
        drop(store);
        let store = EventStore::open(path).unwrap();
        let restored = store.schedule_entry("personal", &request).unwrap().unwrap();
        assert_eq!(restored.state, ScheduleState::Claimed);
        assert_eq!(restored.last_event_id, claimed.event_id);
        assert!(store.pending_schedules().unwrap().is_empty());
    }
}
