use std::{
    fs,
    path::Path,
    sync::{Arc, Mutex, MutexGuard},
};

use chrono::{DateTime, SecondsFormat, Utc};
use ditto_protocol::{EventActor, EventQuery, EventRecord, NewEvent};
use rusqlite::{Connection, params};
use thiserror::Error;
use ulid::Ulid;

const MIGRATION: &str = r#"
CREATE TABLE IF NOT EXISTS events (
    seq             INTEGER PRIMARY KEY AUTOINCREMENT,
    event_id        TEXT NOT NULL UNIQUE,
    recorded_at     TEXT NOT NULL,
    session_id      TEXT,
    task_id         TEXT,
    actor           TEXT NOT NULL,
    kind            TEXT NOT NULL,
    payload_json    TEXT NOT NULL,
    causation_id    TEXT,
    correlation_id  TEXT,
    span_id         TEXT
);

CREATE INDEX IF NOT EXISTS events_session_seq
    ON events(session_id, seq);
CREATE INDEX IF NOT EXISTS events_task_seq
    ON events(task_id, seq);
CREATE INDEX IF NOT EXISTS events_kind_seq
    ON events(kind, seq);

CREATE TRIGGER IF NOT EXISTS events_reject_update
BEFORE UPDATE ON events
BEGIN
    SELECT RAISE(ABORT, 'events are append-only');
END;

CREATE TRIGGER IF NOT EXISTS events_reject_delete
BEFORE DELETE ON events
BEGIN
    SELECT RAISE(ABORT, 'events are append-only');
END;
"#;

#[derive(Debug, Error)]
pub enum EventStoreError {
    #[error("event store I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("event store SQLite operation failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("event payload is invalid JSON: {0}")]
    Payload(#[from] serde_json::Error),
    #[error("event timestamp is invalid: {0}")]
    Timestamp(#[from] chrono::ParseError),
    #[error("event actor is invalid: {0}")]
    InvalidActor(String),
    #[error("event store mutex was poisoned")]
    Poisoned,
}

#[derive(Clone)]
pub struct EventStore {
    connection: Arc<Mutex<Connection>>,
}

impl EventStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, EventStoreError> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)?;
            }
        }

        let connection = Connection::open(path)?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "synchronous", "NORMAL")?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        connection.execute_batch(MIGRATION)?;

        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
        })
    }

    pub fn append(&self, event: NewEvent) -> Result<EventRecord, EventStoreError> {
        let recorded_at = Utc::now();
        let record = EventRecord {
            seq: 0,
            event_id: Ulid::new().to_string(),
            recorded_at,
            session_id: event.session_id,
            task_id: event.task_id,
            actor: event.actor,
            kind: event.kind,
            payload: event.payload,
            causation_id: event.causation_id,
            correlation_id: event.correlation_id,
            span_id: event.span_id,
        };
        let payload_json = serde_json::to_string(&record.payload)?;
        let recorded_at = record
            .recorded_at
            .to_rfc3339_opts(SecondsFormat::Millis, true);

        let connection = self.connection()?;
        connection.execute(
            r#"
            INSERT INTO events (
                event_id, recorded_at, session_id, task_id, actor, kind,
                payload_json, causation_id, correlation_id, span_id
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
            "#,
            params![
                &record.event_id,
                recorded_at,
                record.session_id.as_deref(),
                record.task_id.as_deref(),
                record.actor.as_str(),
                &record.kind,
                payload_json,
                record.causation_id.as_deref(),
                record.correlation_id.as_deref(),
                record.span_id.as_deref(),
            ],
        )?;
        let seq = connection.last_insert_rowid();

        Ok(EventRecord { seq, ..record })
    }

    pub fn list(&self, query: &EventQuery) -> Result<Vec<EventRecord>, EventStoreError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            r#"
            SELECT
                seq, event_id, recorded_at, session_id, task_id, actor, kind,
                payload_json, causation_id, correlation_id, span_id
            FROM events
            WHERE seq > ?1
              AND (?2 IS NULL OR session_id = ?2)
              AND (?3 IS NULL OR task_id = ?3)
            ORDER BY seq ASC
            LIMIT ?4
            "#,
        )?;

        let after_seq = query.after_seq.unwrap_or(0);
        let limit = i64::try_from(query.normalized_limit()).unwrap_or(1_000);
        let rows = statement.query_map(
            params![
                after_seq,
                query.session_id.as_deref(),
                query.task_id.as_deref(),
                limit,
            ],
            |row| {
                Ok(RawEventRecord {
                    seq: row.get(0)?,
                    event_id: row.get(1)?,
                    recorded_at: row.get(2)?,
                    session_id: row.get(3)?,
                    task_id: row.get(4)?,
                    actor: row.get(5)?,
                    kind: row.get(6)?,
                    payload_json: row.get(7)?,
                    causation_id: row.get(8)?,
                    correlation_id: row.get(9)?,
                    span_id: row.get(10)?,
                })
            },
        )?;

        let mut events = Vec::new();
        for row in rows {
            events.push(row?.try_into()?);
        }
        Ok(events)
    }

    pub fn count(&self) -> Result<u64, EventStoreError> {
        let connection = self.connection()?;
        let count: i64 =
            connection.query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))?;
        Ok(u64::try_from(count).unwrap_or_default())
    }

    fn connection(&self) -> Result<MutexGuard<'_, Connection>, EventStoreError> {
        self.connection
            .lock()
            .map_err(|_| EventStoreError::Poisoned)
    }
}

struct RawEventRecord {
    seq: i64,
    event_id: String,
    recorded_at: String,
    session_id: Option<String>,
    task_id: Option<String>,
    actor: String,
    kind: String,
    payload_json: String,
    causation_id: Option<String>,
    correlation_id: Option<String>,
    span_id: Option<String>,
}

impl TryFrom<RawEventRecord> for EventRecord {
    type Error = EventStoreError;

    fn try_from(raw: RawEventRecord) -> Result<Self, Self::Error> {
        let actor = raw
            .actor
            .parse::<EventActor>()
            .map_err(|_| EventStoreError::InvalidActor(raw.actor.clone()))?;
        let recorded_at = DateTime::parse_from_rfc3339(&raw.recorded_at)?.with_timezone(&Utc);
        let payload = serde_json::from_str(&raw.payload_json)?;

        Ok(Self {
            seq: raw.seq,
            event_id: raw.event_id,
            recorded_at,
            session_id: raw.session_id,
            task_id: raw.task_id,
            actor,
            kind: raw.kind,
            payload,
            causation_id: raw.causation_id,
            correlation_id: raw.correlation_id,
            span_id: raw.span_id,
        })
    }
}

#[cfg(test)]
mod tests {
    use ditto_protocol::{EventActor, EventQuery, NewEvent, event_kind};
    use serde_json::json;
    use tempfile::tempdir;

    use super::EventStore;

    #[test]
    fn appends_and_filters_events() {
        let directory = tempdir().expect("temporary directory");
        let store = EventStore::open(directory.path().join("state.db")).expect("open store");

        let first = store
            .append(NewEvent::input("alpha", "hello"))
            .expect("append first event");
        let second = store
            .append(NewEvent {
                session_id: Some("beta".into()),
                task_id: Some("task-1".into()),
                actor: EventActor::System,
                kind: event_kind::TASK_COMPLETED.into(),
                payload: json!({ "verified": true }),
                causation_id: Some(first.event_id.clone()),
                correlation_id: None,
                span_id: None,
            })
            .expect("append second event");

        assert!(second.seq > first.seq);
        assert_eq!(store.count().expect("count events"), 2);

        let events = store
            .list(&EventQuery {
                session_id: Some("beta".into()),
                ..EventQuery::default()
            })
            .expect("list events");

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].task_id.as_deref(), Some("task-1"));
    }

    #[test]
    fn rejects_updates_and_deletes() {
        let directory = tempdir().expect("temporary directory");
        let store = EventStore::open(directory.path().join("state.db")).expect("open store");
        let event = store
            .append(NewEvent::input("alpha", "immutable"))
            .expect("append event");

        let connection = store.connection().expect("lock connection");
        let update_error = connection
            .execute(
                "UPDATE events SET kind = 'tampered' WHERE event_id = ?1",
                [&event.event_id],
            )
            .expect_err("updates must be rejected");
        let delete_error = connection
            .execute("DELETE FROM events WHERE event_id = ?1", [&event.event_id])
            .expect_err("deletes must be rejected");

        assert!(update_error.to_string().contains("events are append-only"));
        assert!(delete_error.to_string().contains("events are append-only"));
        drop(connection);
        assert_eq!(store.count().expect("count events"), 1);
    }
}
