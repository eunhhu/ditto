//! SQLite-backed append-only event spine.

use std::{
    fs,
    path::Path,
    sync::{Arc, Mutex, MutexGuard},
};

use ditto_protocol::{Actor, EventDraft, EventRecord, PayloadRef};
use rusqlite::{Connection, OptionalExtension, params};
use thiserror::Error;

const MIGRATION: &str = r"
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS events (
    seq            INTEGER PRIMARY KEY AUTOINCREMENT,
    timestamp_ms   INTEGER NOT NULL,
    session_id     TEXT,
    task_id        TEXT,
    actor          TEXT NOT NULL,
    kind           TEXT NOT NULL,
    payload_ref    TEXT NOT NULL,
    causation_id   TEXT,
    correlation_id TEXT,
    span_id        TEXT
);

CREATE INDEX IF NOT EXISTS events_session_seq ON events(session_id, seq);
CREATE INDEX IF NOT EXISTS events_task_seq ON events(task_id, seq);
CREATE INDEX IF NOT EXISTS events_kind_seq ON events(kind, seq);

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
";

#[derive(Clone, Debug)]
pub struct EventStore {
    connection: Arc<Mutex<Connection>>,
}

#[derive(Debug, Error)]
pub enum EventStoreError {
    #[error("event store lock was poisoned")]
    Poisoned,
    #[error("failed to prepare event store directory: {0}")]
    Io(#[from] std::io::Error),
    #[error("SQLite event store error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("event payload serialization failed: {0}")]
    Serialization(#[from] serde_json::Error),
}

impl EventStore {
    /// Opens a durable store and applies idempotent schema migrations.
    ///
    /// # Errors
    ///
    /// Returns an I/O or `SQLite` error when the database cannot be prepared.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, EventStoreError> {
        let path = path.as_ref();
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent)?;
        }
        let connection = Connection::open(path)?;
        Self::from_connection(connection)
    }

    /// Creates an ephemeral store with the same schema and invariants.
    ///
    /// # Errors
    ///
    /// Returns a `SQLite` error when the in-memory database cannot be initialized.
    pub fn in_memory() -> Result<Self, EventStoreError> {
        Self::from_connection(Connection::open_in_memory()?)
    }

    fn from_connection(connection: Connection) -> Result<Self, EventStoreError> {
        connection.execute_batch(MIGRATION)?;
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
        })
    }

    /// Appends one immutable event and returns its assigned sequence number.
    ///
    /// # Errors
    ///
    /// Returns an error when locking, serialization, or `SQLite` insertion fails.
    pub fn append(&self, event: EventDraft) -> Result<EventRecord, EventStoreError> {
        let actor = serde_json::to_string(&event.actor)?;
        let payload_ref = serde_json::to_string(&event.payload_ref)?;
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO events (
                timestamp_ms, session_id, task_id, actor, kind, payload_ref,
                causation_id, correlation_id, span_id
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                event.timestamp_ms,
                event.session_id,
                event.task_id,
                actor,
                event.kind,
                payload_ref,
                event.causation_id,
                event.correlation_id,
                event.span_id,
            ],
        )?;
        let seq = connection.last_insert_rowid();
        Ok(EventRecord { seq, event })
    }

    /// Replays matching events after an exclusive sequence cursor.
    ///
    /// # Errors
    ///
    /// Returns an error when the store cannot be queried or decoded.
    pub fn replay(
        &self,
        session_id: Option<&str>,
        task_id: Option<&str>,
        after_seq: i64,
    ) -> Result<Vec<EventRecord>, EventStoreError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT seq, timestamp_ms, session_id, task_id, actor, kind, payload_ref,
                    causation_id, correlation_id, span_id
             FROM events
             WHERE seq > ?1
               AND (?2 IS NULL OR session_id = ?2)
               AND (?3 IS NULL OR task_id = ?3)
             ORDER BY seq ASC",
        )?;
        let rows = statement.query_map(params![after_seq, session_id, task_id], decode_row)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Returns the latest assigned sequence, or zero for an empty store.
    ///
    /// # Errors
    ///
    /// Returns an error when the store cannot be locked or queried.
    pub fn latest_seq(&self) -> Result<i64, EventStoreError> {
        let connection = self.connection()?;
        let seq = connection
            .query_row("SELECT MAX(seq) FROM events", [], |row| row.get(0))
            .optional()?
            .flatten()
            .unwrap_or(0);
        Ok(seq)
    }

    fn connection(&self) -> Result<MutexGuard<'_, Connection>, EventStoreError> {
        self.connection
            .lock()
            .map_err(|_| EventStoreError::Poisoned)
    }
}

fn decode_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<EventRecord> {
    let actor_json: String = row.get(4)?;
    let payload_json: String = row.get(6)?;
    let actor: Actor = serde_json::from_str(&actor_json).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            actor_json.len(),
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    })?;
    let payload_ref: PayloadRef = serde_json::from_str(&payload_json).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            payload_json.len(),
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    })?;

    Ok(EventRecord {
        seq: row.get(0)?,
        event: EventDraft {
            timestamp_ms: row.get(1)?,
            session_id: row.get(2)?,
            task_id: row.get(3)?,
            actor,
            kind: row.get(5)?,
            payload_ref,
            causation_id: row.get(7)?,
            correlation_id: row.get(8)?,
            span_id: row.get(9)?,
        },
    })
}

#[cfg(test)]
mod tests {
    use ditto_protocol::{Actor, EventDraft, event_kind};
    use serde_json::json;
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn events_survive_reopen_and_replay_in_sequence() {
        let directory = tempdir().unwrap();
        let database = directory.path().join("state.db");
        let store = EventStore::open(&database).unwrap();
        let first = store
            .append(
                EventDraft::inline(
                    Actor::User,
                    event_kind::INPUT_RECEIVED,
                    json!({"text": "hi"}),
                )
                .scoped("session-a", "task-a"),
            )
            .unwrap();
        store
            .append(
                EventDraft::inline(Actor::Kernel, event_kind::TASK_COMPLETED, json!({}))
                    .scoped("session-a", "task-a"),
            )
            .unwrap();
        drop(store);

        let reopened = EventStore::open(&database).unwrap();
        let replay = reopened.replay(None, Some("task-a"), first.seq).unwrap();
        assert_eq!(replay.len(), 1);
        assert_eq!(replay[0].event.kind, event_kind::TASK_COMPLETED);
    }

    #[test]
    fn event_rows_cannot_be_updated_or_deleted() {
        let store = EventStore::in_memory().unwrap();
        store
            .append(EventDraft::inline(
                Actor::User,
                event_kind::INPUT_RECEIVED,
                json!({}),
            ))
            .unwrap();
        let connection = store.connection().unwrap();
        let error = connection.execute("DELETE FROM events", []).unwrap_err();
        assert!(error.to_string().contains("append-only"));
    }
}
