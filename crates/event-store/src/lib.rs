use std::{
    cell::Cell,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
};

use chrono::{DateTime, SecondsFormat, Utc};
use ditto_protocol::{EventActor, EventQuery, EventRecord, NewEvent};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use thiserror::Error;
use ulid::Ulid;

pub mod recurrence;
mod schedule;
pub use schedule::{ScheduleEntry, ScheduleState};

const CURRENT_SCHEMA_VERSION: i64 = 7;

thread_local! {
    static ASYNC_RUNTIME_THREAD: Cell<bool> = const { Cell::new(false) };
}

/// Mark the calling thread as one that drives async tasks. Debug builds then
/// reject any journal access on it: SQLite work belongs on blocking threads,
/// never on a thread that other tasks wait for (ADR 0028 Phase B).
pub fn mark_async_runtime_thread() {
    ASYNC_RUNTIME_THREAD.with(|marked| marked.set(true));
}

/// Conversation markers: resets bound a thread; finished turns are its history.
const MIGRATION_V7: &str = r#"
CREATE INDEX IF NOT EXISTS events_conversation ON events(session_id, seq)
    WHERE kind IN ('conversation.reset', 'turn.finished');
"#;

const MIGRATION_V4: &str = r#"
CREATE INDEX IF NOT EXISTS events_agent_sort ON events(session_id, task_id, correlation_id, seq)
    WHERE kind IN ('agent.sort.started', 'agent.sort.output');
"#;

const MIGRATION_V3: &str = r#"
CREATE INDEX IF NOT EXISTS events_session_task_seq ON events(session_id, task_id, seq)
    WHERE correlation_id GLOB 'turn_*';
CREATE INDEX IF NOT EXISTS events_session_task_correlation_seq
    ON events(session_id, task_id, correlation_id, seq) WHERE correlation_id GLOB 'turn_*';
"#;

const MIGRATION_V1: &str = r#"
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
"#;

const MIGRATION_V2: &str = r#"
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
    #[error("schedule journal or index is invalid")]
    InvalidSchedule,
    #[error("event store mutex was poisoned")]
    Poisoned,
    #[error("database schema version {found} is newer than supported version {supported}")]
    UnsupportedSchemaVersion { found: i64, supported: i64 },
}

#[derive(Clone)]
pub struct EventStore {
    connection: Arc<Mutex<Connection>>,
}

impl EventStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, EventStoreError> {
        let path = path.as_ref();
        let sqlite_path = prepare_private_sqlite_path(path)?;

        let mut connection = Connection::open_with_flags(
            &sqlite_path,
            OpenFlags::default() | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "synchronous", "NORMAL")?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        apply_migrations(&mut connection)?;
        schedule::rebuild(&mut connection)?;
        enforce_private_sqlite_files(path)?;

        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
        })
    }

    pub fn append(&self, event: NewEvent) -> Result<EventRecord, EventStoreError> {
        let mut connection = self.connection()?;
        if schedule::is_schedule_kind(&event.kind) {
            let transaction = connection.transaction()?;
            let record = insert_event(&transaction, event)?;
            schedule::project(&transaction, &record)?;
            transaction.commit()?;
            Ok(record)
        } else {
            insert_event(&connection, event)
        }
    }

    /// Append events in one transaction, in order. Their IDs are assigned by
    /// the caller, so a later event can name an earlier one as its cause.
    pub fn append_batch(
        &self,
        events: Vec<(String, NewEvent)>,
    ) -> Result<Vec<EventRecord>, EventStoreError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let mut records = Vec::with_capacity(events.len());
        for (event_id, event) in events {
            let record = insert_event_with_id(&transaction, event_id, event)?;
            if schedule::is_schedule_kind(&record.kind) {
                schedule::project(&transaction, &record)?;
            }
            records.push(record);
        }
        transaction.commit()?;
        Ok(records)
    }

    pub fn list(&self, query: &EventQuery) -> Result<Vec<EventRecord>, EventStoreError> {
        self.list_through(query, i64::MAX)
    }

    /// Returns one stable page bounded by an inclusive high-water sequence.
    pub fn list_through(
        &self,
        query: &EventQuery,
        through_seq: i64,
    ) -> Result<Vec<EventRecord>, EventStoreError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            r#"
            SELECT
                seq, event_id, recorded_at, session_id, task_id, actor, kind,
                payload_json, causation_id, correlation_id, span_id
            FROM events
            WHERE seq > ?1
              AND seq <= ?2
              AND (?3 IS NULL OR session_id = ?3)
              AND (?4 IS NULL OR task_id = ?4)
            ORDER BY seq ASC
            LIMIT ?5
            "#,
        )?;

        let after_seq = query.after_seq.unwrap_or(0).max(0);
        let limit = i64::try_from(query.normalized_limit()).unwrap_or(1_000);
        let rows = statement.query_map(
            params![
                after_seq,
                through_seq.max(0),
                query.session_id.as_deref(),
                query.task_id.as_deref(),
                limit,
            ],
            raw_event_record_from_row,
        )?;

        let mut events = Vec::new();
        for row in rows {
            events.push(row?.try_into()?);
        }
        Ok(events)
    }

    /// One page of a single event kind after `after_seq` through the inclusive
    /// high-water, in sequence order, read from the kind index.
    pub fn list_kind_through(
        &self,
        kind: &str,
        after_seq: i64,
        through_seq: i64,
        limit: usize,
    ) -> Result<Vec<EventRecord>, EventStoreError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare_cached(
            "SELECT seq, event_id, recorded_at, session_id, task_id, actor, kind,
                    payload_json, causation_id, correlation_id, span_id
             FROM events INDEXED BY events_kind_seq
             WHERE kind = ?1 AND seq > ?2 AND seq <= ?3
             ORDER BY seq ASC LIMIT ?4",
        )?;
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let rows = statement.query_map(
            params![kind, after_seq.max(0), through_seq, limit],
            raw_event_record_from_row,
        )?;
        rows.map(|row| row?.try_into()).collect()
    }

    /// Whether an event of `kind` lies after `after_seq` through the inclusive
    /// high-water; one kind-index probe.
    pub fn has_kind_between(
        &self,
        kind: &str,
        after_seq: i64,
        through_seq: i64,
    ) -> Result<bool, EventStoreError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare_cached(
            "SELECT EXISTS(SELECT 1 FROM events INDEXED BY events_kind_seq
                           WHERE kind = ?1 AND seq > ?2 AND seq <= ?3)",
        )?;
        Ok(statement.query_row(params![kind, after_seq, through_seq], |row| row.get(0))?)
    }

    /// Two indexed boundary reads, independent of transcript length. The latest
    /// event is restricted to the first event's kernel-assigned correlation.
    pub fn task_turn_boundary(
        &self,
        session_id: &str,
        task_id: &str,
    ) -> Result<Option<(EventRecord, EventRecord)>, EventStoreError> {
        let connection = self.connection()?;
        let first = connection
            .query_row(
                "SELECT seq, event_id, recorded_at, session_id, task_id, actor, kind,
                    payload_json, causation_id, correlation_id, span_id
             FROM events INDEXED BY events_session_task_seq WHERE session_id = ?1 AND task_id = ?2 AND correlation_id GLOB 'turn_*' ORDER BY seq LIMIT 1",
                params![session_id, task_id],
                raw_event_record_from_row,
            )
            .optional()?;
        let Some(first) = first else { return Ok(None) };
        let first: EventRecord = first.try_into()?;
        let last = connection.query_row(
            "SELECT seq, event_id, recorded_at, session_id, task_id, actor, kind,
                    payload_json, causation_id, correlation_id, span_id
             FROM events INDEXED BY events_session_task_correlation_seq WHERE session_id = ?1 AND task_id = ?2 AND correlation_id = ?3 AND correlation_id GLOB 'turn_*'
             ORDER BY seq DESC LIMIT 1",
            params![session_id, task_id, first.correlation_id],
            raw_event_record_from_row,
        )?;
        Ok(Some((first, last.try_into()?)))
    }

    pub fn task_has_event_kind(
        &self,
        session: &str,
        task: &str,
        kind: &str,
    ) -> Result<bool, EventStoreError> {
        let connection = self.connection()?;
        Ok(connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM events WHERE session_id = ?1 AND task_id = ?2 AND kind = ?3)",
            params![session, task, kind], |row| row.get(0),
        )?)
    }

    /// A bounded projection input: one claim plus at most seven tool results.
    /// The ninth record is a corruption sentinel, never silently truncated state.
    pub fn agent_sort_events(
        &self,
        session: &str,
        task: &str,
        correlation: &str,
    ) -> Result<Vec<EventRecord>, EventStoreError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT seq, event_id, recorded_at, session_id, task_id, actor, kind,
                    payload_json, causation_id, correlation_id, span_id
             FROM events INDEXED BY events_agent_sort
             WHERE session_id = ?1 AND task_id = ?2 AND correlation_id = ?3
               AND kind IN ('agent.sort.started', 'agent.sort.output') ORDER BY seq LIMIT 9",
        )?;
        let rows = statement.query_map(
            params![session, task, correlation],
            raw_event_record_from_row,
        )?;
        rows.map(|row| row?.try_into()).collect()
    }

    /// Newest-first `turn.finished` events of the session's current thread
    /// before `before_seq`: after its latest `conversation.reset`, at most
    /// `limit`. Two partial-index reads, independent of transcript length.
    pub fn conversation_finished_turns(
        &self,
        session: &str,
        before_seq: i64,
        limit: usize,
    ) -> Result<Vec<EventRecord>, EventStoreError> {
        let connection = self.connection()?;
        let boundary: i64 = connection.query_row(
            "SELECT COALESCE(MAX(seq), 0) FROM events INDEXED BY events_conversation
             WHERE session_id = ?1 AND seq < ?2
               AND kind IN ('conversation.reset', 'turn.finished')
               AND kind = 'conversation.reset'",
            params![session, before_seq],
            |row| row.get(0),
        )?;
        let mut statement = connection.prepare(
            "SELECT seq, event_id, recorded_at, session_id, task_id, actor, kind,
                    payload_json, causation_id, correlation_id, span_id
             FROM events INDEXED BY events_conversation
             WHERE session_id = ?1 AND seq > ?2 AND seq < ?3
               AND kind IN ('conversation.reset', 'turn.finished')
               AND kind = 'turn.finished'
             ORDER BY seq DESC LIMIT ?4",
        )?;
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let rows = statement.query_map(
            params![session, boundary, before_seq, limit],
            raw_event_record_from_row,
        )?;
        rows.map(|row| row?.try_into()).collect()
    }

    /// The session's `conversation.reset` and `turn.finished` events after
    /// `after_seq` and before `before_seq`, oldest first, at most `limit`.
    pub fn conversation_events_between(
        &self,
        session: &str,
        after_seq: i64,
        before_seq: i64,
        limit: usize,
    ) -> Result<Vec<EventRecord>, EventStoreError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare_cached(
            "SELECT seq, event_id, recorded_at, session_id, task_id, actor, kind,
                    payload_json, causation_id, correlation_id, span_id
             FROM events INDEXED BY events_conversation
             WHERE session_id = ?1 AND seq > ?2 AND seq < ?3
               AND kind IN ('conversation.reset', 'turn.finished')
             ORDER BY seq ASC LIMIT ?4",
        )?;
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let rows = statement.query_map(
            params![session, after_seq, before_seq, limit],
            raw_event_record_from_row,
        )?;
        rows.map(|row| row?.try_into()).collect()
    }

    /// Finished agent-run turns (task `run_*`) of the session's current thread
    /// before `before_seq`: after its latest `conversation.reset`. This is the
    /// thread position that anchors stepped history windows.
    pub fn conversation_finished_count(
        &self,
        session: &str,
        before_seq: i64,
    ) -> Result<usize, EventStoreError> {
        let connection = self.connection()?;
        let count: i64 = connection.query_row(
            "SELECT COUNT(*) FROM events INDEXED BY events_conversation
             WHERE session_id = ?1 AND seq < ?2
               AND kind IN ('conversation.reset', 'turn.finished')
               AND kind = 'turn.finished' AND task_id GLOB 'run_*'
               AND seq > (SELECT COALESCE(MAX(seq), 0) FROM events INDEXED BY events_conversation
                          WHERE session_id = ?1 AND seq < ?2
                            AND kind IN ('conversation.reset', 'turn.finished')
                            AND kind = 'conversation.reset')",
            params![session, before_seq],
            |row| row.get(0),
        )?;
        Ok(usize::try_from(count).unwrap_or_default())
    }

    /// The first event of one kernel turn, which is its input; indexed.
    pub fn turn_input(
        &self,
        session: &str,
        task: &str,
        turn_id: &str,
    ) -> Result<Option<EventRecord>, EventStoreError> {
        let connection = self.connection()?;
        let raw = connection
            .query_row(
                "SELECT seq, event_id, recorded_at, session_id, task_id, actor, kind,
                        payload_json, causation_id, correlation_id, span_id
                 FROM events INDEXED BY events_session_task_correlation_seq
                 WHERE session_id = ?1 AND task_id = ?2 AND correlation_id = ?3
                   AND correlation_id GLOB 'turn_*'
                 ORDER BY seq LIMIT 1",
                params![session, task, turn_id],
                raw_event_record_from_row,
            )
            .optional()?;
        raw.map(TryInto::try_into).transpose()
    }

    /// Returns the event with the exact globally unique event ID, if present.
    pub fn get_by_event_id(&self, event_id: &str) -> Result<Option<EventRecord>, EventStoreError> {
        let connection = self.connection()?;
        let raw = connection
            .query_row(
                r#"
                SELECT
                    seq, event_id, recorded_at, session_id, task_id, actor, kind,
                    payload_json, causation_id, correlation_id, span_id
                FROM events
                WHERE event_id = ?1
                "#,
                [event_id],
                raw_event_record_from_row,
            )
            .optional()?;

        raw.map(TryInto::try_into).transpose()
    }

    /// Returns the event with the exact durable sequence, if present.
    pub fn get_by_seq(&self, seq: i64) -> Result<Option<EventRecord>, EventStoreError> {
        let connection = self.connection()?;
        let raw = connection
            .query_row(
                r#"
                SELECT
                    seq, event_id, recorded_at, session_id, task_id, actor, kind,
                    payload_json, causation_id, correlation_id, span_id
                FROM events
                WHERE seq = ?1
                "#,
                [seq],
                raw_event_record_from_row,
            )
            .optional()?;

        raw.map(TryInto::try_into).transpose()
    }

    pub fn latest_seq(&self) -> Result<i64, EventStoreError> {
        let connection = self.connection()?;
        let latest =
            connection.query_row("SELECT COALESCE(MAX(seq), 0) FROM events", [], |row| {
                row.get(0)
            })?;
        Ok(latest)
    }

    pub fn count(&self) -> Result<u64, EventStoreError> {
        let connection = self.connection()?;
        let count: i64 =
            connection.query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))?;
        Ok(u64::try_from(count).unwrap_or_default())
    }

    fn connection(&self) -> Result<MutexGuard<'_, Connection>, EventStoreError> {
        debug_assert!(
            !ASYNC_RUNTIME_THREAD.with(Cell::get),
            "journal access on an async runtime thread; run it on a blocking thread"
        );
        self.connection
            .lock()
            .map_err(|_| EventStoreError::Poisoned)
    }
}

fn prepare_private_sqlite_path(path: &Path) -> Result<PathBuf, std::io::Error> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "SQLite database path requires an explicit private parent directory",
            )
        })?;
    ensure_private_directory(parent)?;
    for candidate in sqlite_family_paths(path) {
        validate_private_file(&candidate)?;
    }
    if fs::symlink_metadata(path).is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound) {
        let mut options = fs::OpenOptions::new();
        options.read(true).write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        options.open(path)?;
    }
    validate_private_file(path)?;
    let filename = path.file_name().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "SQLite database path has no filename",
        )
    })?;
    Ok(fs::canonicalize(parent)?.join(filename))
}

fn enforce_private_sqlite_files(path: &Path) -> Result<(), std::io::Error> {
    for candidate in sqlite_family_paths(path) {
        validate_private_file(&candidate)?;
    }
    Ok(())
}

fn sqlite_family_paths(path: &Path) -> [PathBuf; 3] {
    let mut wal = path.as_os_str().to_os_string();
    wal.push("-wal");
    let mut shm = path.as_os_str().to_os_string();
    shm.push("-shm");
    [path.to_path_buf(), PathBuf::from(wal), PathBuf::from(shm)]
}

fn ensure_private_directory(path: &Path) -> Result<(), std::io::Error> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => validate_private_directory_metadata(path, &metadata)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                let mut builder = fs::DirBuilder::new();
                builder.recursive(true).mode(0o700).create(path)?;
            }
            #[cfg(not(unix))]
            fs::create_dir_all(path)?;
            let metadata = fs::symlink_metadata(path)?;
            validate_private_directory_metadata(path, &metadata)?;
        }
        Err(error) => return Err(error),
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn validate_private_directory_metadata(
    path: &Path,
    metadata: &fs::Metadata,
) -> Result<(), std::io::Error> {
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(unsafe_path(
            path,
            "private SQLite parent is not a real directory",
        ));
    }
    validate_current_owner(path, metadata)
}

fn validate_private_file(path: &Path) -> Result<(), std::io::Error> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(unsafe_path(
            path,
            "SQLite family member is not a regular file",
        ));
    }
    validate_current_owner(path, &metadata)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

#[cfg(unix)]
fn validate_current_owner(path: &Path, metadata: &fs::Metadata) -> Result<(), std::io::Error> {
    use std::os::unix::fs::MetadataExt;

    // SAFETY: `geteuid` reads process identity and has no preconditions.
    let effective_uid = unsafe { libc::geteuid() };
    if metadata.uid() != effective_uid {
        return Err(unsafe_path(
            path,
            "SQLite path is not owned by the current user",
        ));
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_current_owner(_path: &Path, _metadata: &fs::Metadata) -> Result<(), std::io::Error> {
    Ok(())
}

fn unsafe_path(path: &Path, reason: &str) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::PermissionDenied,
        format!("{reason}: {}", path.display()),
    )
}

fn insert_event(connection: &Connection, event: NewEvent) -> Result<EventRecord, EventStoreError> {
    insert_event_with_id(connection, Ulid::new().to_string(), event)
}

fn insert_event_with_id(
    connection: &Connection,
    event_id: String,
    event: NewEvent,
) -> Result<EventRecord, EventStoreError> {
    let recorded_at = DateTime::from_timestamp_millis(Utc::now().timestamp_millis())
        .expect("a current UTC timestamp is representable at millisecond precision");
    let record = EventRecord {
        seq: 0,
        event_id,
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

    connection
        .prepare_cached(
            r#"
            INSERT INTO events (
                event_id, recorded_at, session_id, task_id, actor, kind,
                payload_json, causation_id, correlation_id, span_id
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
            "#,
        )?
        .execute(params![
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
        ])?;
    let seq = connection.last_insert_rowid();

    Ok(EventRecord { seq, ..record })
}

fn apply_migrations(connection: &mut Connection) -> Result<(), EventStoreError> {
    let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version > CURRENT_SCHEMA_VERSION {
        return Err(EventStoreError::UnsupportedSchemaVersion {
            found: version,
            supported: CURRENT_SCHEMA_VERSION,
        });
    }

    let transaction = connection.transaction()?;
    if version < 1 {
        transaction.execute_batch(MIGRATION_V1)?;
    }
    if version < 2 {
        transaction.execute_batch(MIGRATION_V2)?;
    }
    if version < 3 {
        transaction.execute_batch(MIGRATION_V3)?;
    }
    if version < 4 {
        transaction.execute_batch(MIGRATION_V4)?;
    }
    if version < 5 {
        transaction.execute_batch(schedule::TABLE_SCHEMA)?;
        transaction.execute_batch(schedule::MIGRATION_V5)?;
    }
    if version < 6 {
        transaction.execute_batch(recurrence::SCHEMA)?;
        transaction.execute("DROP INDEX IF EXISTS events_schedules", [])?;
        transaction.execute_batch(recurrence::SOURCE_INDEX)?;
    }
    if version < 7 {
        transaction.execute_batch(MIGRATION_V7)?;
    }
    transaction.pragma_update(None, "user_version", CURRENT_SCHEMA_VERSION)?;
    transaction.commit()?;
    Ok(())
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

fn raw_event_record_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawEventRecord> {
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
    fn a_batch_commits_every_event_or_none() {
        let directory = tempdir().expect("temporary directory");
        let store = EventStore::open(directory.path().join("state.db")).expect("open store");
        let input = |text: &str| NewEvent::user_input("batch", None, text);
        let first = ulid::Ulid::new().to_string();
        let committed = store
            .append_batch(vec![
                (first.clone(), input("first")),
                (ulid::Ulid::new().to_string(), input("second")),
            ])
            .expect("append batch");
        assert_eq!(committed[0].event_id, first);
        assert_eq!(committed[1].seq, committed[0].seq + 1);
        let high_water = store.latest_seq().expect("latest seq");
        // A repeated ID in the second event rolls back the first as well.
        assert!(
            store
                .append_batch(vec![
                    (ulid::Ulid::new().to_string(), input("third")),
                    (first, input("fourth")),
                ])
                .is_err()
        );
        assert_eq!(store.latest_seq().expect("latest seq"), high_water);
    }

    #[test]
    #[cfg(debug_assertions)]
    fn journal_access_is_rejected_on_a_marked_async_runtime_thread() {
        let directory = tempdir().expect("temporary directory");
        let store = EventStore::open(directory.path().join("state.db")).expect("open store");
        let unmarked = store.clone();
        assert_eq!(
            std::thread::spawn(move || unmarked.latest_seq().expect("latest seq"))
                .join()
                .expect("an unmarked thread reads the journal"),
            0
        );
        let marked = store.clone();
        let rejected = std::thread::spawn(move || {
            super::mark_async_runtime_thread();
            marked.latest_seq()
        })
        .join();
        assert!(rejected.is_err(), "a marked thread must not reach SQLite");
        // The rejection happens before the connection lock: nothing is poisoned.
        assert_eq!(store.latest_seq().expect("store still usable"), 0);
    }

    #[test]
    fn schema_four_sort_lookup_migrates_without_rewrite_and_bounds_exact_turn_work() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("state.db");
        let store = EventStore::open(&path).unwrap();
        let old = store
            .append(NewEvent::user_input("s", None, "legacy"))
            .unwrap();
        drop(store);
        let db = rusqlite::Connection::open(&path).unwrap();
        db.execute_batch("DROP INDEX events_agent_sort; PRAGMA user_version=3;")
            .unwrap();
        drop(db);
        let store = EventStore::open(&path).unwrap();
        assert_eq!(
            serde_json::to_value(store.get_by_event_id(&old.event_id).unwrap().unwrap()).unwrap(),
            serde_json::to_value(old).unwrap()
        );
        let plan=store.connection().unwrap().prepare(
            "EXPLAIN QUERY PLAN SELECT seq FROM events INDEXED BY events_agent_sort WHERE session_id='s' AND task_id='t' AND correlation_id='turn_x' AND kind IN ('agent.sort.started','agent.sort.output') ORDER BY seq LIMIT 9"
        ).unwrap().query_map([],|row|row.get::<_,String>(3)).unwrap().collect::<Result<Vec<_>,_>>().unwrap().join(" ");
        assert!(
            plan.contains("SEARCH events USING")
                && plan.contains("events_agent_sort")
                && !plan.contains("TEMP B-TREE"),
            "{plan}"
        );
        let mut draft = NewEvent::user_input("s", Some("t".into()), "fixture");
        draft.correlation_id = Some("turn_x".into());
        for _ in 0..40 {
            store.append(draft.clone()).unwrap();
        }
        draft.kind = event_kind::AGENT_SORT_OUTPUT.into();
        for _ in 0..12 {
            store.append(draft.clone()).unwrap();
        }
        draft.session_id = Some("other".into());
        store.append(draft.clone()).unwrap();
        draft.session_id = Some("s".into());
        draft.correlation_id = Some("turn_other".into());
        store.append(draft).unwrap();
        let results = store.agent_sort_events("s", "t", "turn_x").unwrap();
        assert_eq!(results.len(), 9);
        assert!(
            results
                .iter()
                .all(|e| e.kind == event_kind::AGENT_SORT_OUTPUT
                    && e.session_id.as_deref() == Some("s")
                    && e.correlation_id.as_deref() == Some("turn_x"))
        );
        assert!(
            store
                .agent_sort_events("s", "missing", "turn_x")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn schema_three_boundary_indexes_preserve_scope_and_correlation_after_migration() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("state.db");
        let mut connection = rusqlite::Connection::open(&path).unwrap();
        connection.execute_batch(super::MIGRATION_V1).unwrap();
        connection.execute_batch(super::MIGRATION_V2).unwrap();
        connection.pragma_update(None, "user_version", 2).unwrap();
        super::apply_migrations(&mut connection).unwrap();
        assert_eq!(
            connection
                .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
                .unwrap(),
            super::CURRENT_SCHEMA_VERSION
        );
        for (sql, index) in [
            (
                "SELECT seq FROM events INDEXED BY events_session_task_seq WHERE session_id='s' AND task_id='t' AND correlation_id GLOB 'turn_*' ORDER BY seq LIMIT 1",
                "events_session_task_seq",
            ),
            (
                "SELECT seq FROM events INDEXED BY events_session_task_correlation_seq WHERE session_id='s' AND task_id='t' AND correlation_id = 'turn_c' AND correlation_id GLOB 'turn_*' ORDER BY seq DESC LIMIT 1",
                "events_session_task_correlation_seq",
            ),
        ] {
            let plan = connection
                .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
                .unwrap()
                .query_map([], |row| row.get::<_, String>(3))
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap()
                .join(" ");
            assert!(plan.contains(index), "{plan}");
            assert!(
                !plan.contains("SCAN events") && !plan.contains("TEMP B-TREE"),
                "{plan}"
            );
        }
        drop(connection);
        let store = EventStore::open(&path).unwrap();
        assert!(store.task_turn_boundary("s", "t").unwrap().is_none());
        store
            .append(NewEvent::user_input(
                "s",
                Some("t".into()),
                "uncorrelated note",
            ))
            .unwrap();
        assert!(store.task_turn_boundary("s", "t").unwrap().is_none());
        let mut draft = NewEvent::user_input("s", Some("t".into()), "first");
        draft.correlation_id = Some("turn_c".into());
        let first = store.append(draft.clone()).unwrap();
        draft.kind = event_kind::TURN_FINISHED.into();
        draft.actor = EventActor::System;
        let last = store.append(draft.clone()).unwrap();
        draft.correlation_id = Some("turn_another".into());
        store.append(draft.clone()).unwrap();
        draft.session_id = Some("other".into());
        store.append(draft).unwrap();
        assert!(
            !store
                .task_has_event_kind("s", "t", event_kind::TASK_COMPLETED)
                .unwrap()
        );
        assert!(
            store
                .task_has_event_kind("s", "t", event_kind::TURN_FINISHED)
                .unwrap()
        );
        drop(store);
        let reopened = EventStore::open(&path).unwrap();
        let boundary = reopened.task_turn_boundary("s", "t").unwrap().unwrap();
        assert_eq!(boundary.0.event_id, first.event_id);
        assert_eq!(boundary.1.event_id, last.event_id);
        assert!(
            reopened
                .task_turn_boundary("missing", "t")
                .unwrap()
                .is_none()
        );
        assert!(
            reopened
                .connection()
                .unwrap()
                .execute("DELETE FROM events", [])
                .is_err()
        );
    }

    #[test]
    fn schema_six_gains_conversation_lookups_bounded_by_reset_and_sequence() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("state.db");
        let store = EventStore::open(&path).unwrap();
        let old = store
            .append(NewEvent::user_input("s", None, "legacy"))
            .unwrap();
        drop(store);
        let db = rusqlite::Connection::open(&path).unwrap();
        db.execute_batch("DROP INDEX events_conversation; PRAGMA user_version=6;")
            .unwrap();
        drop(db);
        let store = EventStore::open(&path).unwrap();
        assert_eq!(
            serde_json::to_value(store.get_by_event_id(&old.event_id).unwrap().unwrap()).unwrap(),
            serde_json::to_value(old).unwrap()
        );
        let plan = store
            .connection()
            .unwrap()
            .prepare(
                "EXPLAIN QUERY PLAN SELECT seq FROM events INDEXED BY events_conversation \
                 WHERE session_id='s' AND seq > 0 AND seq < 99 \
                 AND kind IN ('conversation.reset', 'turn.finished') AND kind = 'turn.finished' \
                 ORDER BY seq DESC LIMIT 32",
            )
            .unwrap()
            .query_map([], |row| row.get::<_, String>(3))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
            .join(" ");
        assert!(
            plan.contains("events_conversation") && !plan.contains("TEMP B-TREE"),
            "{plan}"
        );

        let marker = |session: &str, kind: &str, turn: &str| NewEvent {
            session_id: Some(session.into()),
            task_id: Some(format!("run_{turn}")),
            actor: EventActor::System,
            kind: kind.into(),
            payload: json!({ "turn_id": turn }),
            causation_id: None,
            correlation_id: Some(turn.into()),
            span_id: None,
        };
        let mut input = NewEvent::user_input("s", Some("run_turn_a".into()), "first");
        input.correlation_id = Some("turn_a".into());
        let first_input = store.append(input).unwrap();
        store
            .append(marker("s", event_kind::TURN_FINISHED, "turn_a"))
            .unwrap();
        store
            .append(marker("s", event_kind::CONVERSATION_RESET, "reset"))
            .unwrap();
        let b = store
            .append(marker("s", event_kind::TURN_FINISHED, "turn_b"))
            .unwrap();
        store
            .append(marker("other", event_kind::TURN_FINISHED, "turn_x"))
            .unwrap();
        let c = store
            .append(marker("s", event_kind::TURN_FINISHED, "turn_c"))
            .unwrap();
        let later = store
            .append(marker("s", event_kind::TURN_FINISHED, "turn_d"))
            .unwrap();

        let ids = |events: Vec<ditto_protocol::EventRecord>| {
            events
                .into_iter()
                .map(|event| event.event_id)
                .collect::<Vec<_>>()
        };
        // Newest first, after the reset, before the cutoff, same session only.
        assert_eq!(
            ids(store
                .conversation_finished_turns("s", later.seq, 32)
                .unwrap()),
            [c.event_id.clone(), b.event_id.clone()]
        );
        assert_eq!(
            ids(store
                .conversation_finished_turns("s", later.seq, 1)
                .unwrap()),
            [c.event_id]
        );
        assert!(
            store
                .conversation_finished_turns("s", b.seq, 32)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            store
                .turn_input("s", "run_turn_a", "turn_a")
                .unwrap()
                .unwrap()
                .event_id,
            first_input.event_id
        );
        assert!(
            store
                .turn_input("s", "run_turn_a", "turn_b")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn appends_and_filters_events() {
        let directory = tempdir().expect("temporary directory");
        let store = EventStore::open(directory.path().join("state.db")).expect("open store");

        let first = store
            .append(NewEvent::user_input("alpha", None, "hello"))
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
    fn looks_up_exact_events_by_id_and_sequence_after_reopen() {
        let directory = tempdir().expect("temporary directory");
        let path = directory.path().join("state.db");
        let store = EventStore::open(&path).expect("open store");
        let appended = store
            .append(NewEvent::user_input(
                "session",
                Some("task".into()),
                "lookup",
            ))
            .expect("append event");

        let by_id = store
            .get_by_event_id(&appended.event_id)
            .expect("lookup by event ID")
            .expect("event ID is present");
        let by_seq = store
            .get_by_seq(appended.seq)
            .expect("lookup by sequence")
            .expect("sequence is present");
        assert_eq!(
            serde_json::to_string(&by_id).expect("serialize ID lookup"),
            serde_json::to_string(&appended).expect("serialize appended event")
        );
        assert_eq!(
            serde_json::to_string(&by_seq).expect("serialize sequence lookup"),
            serde_json::to_string(&appended).expect("serialize appended event")
        );
        assert!(
            store
                .get_by_event_id("01J00000000000000000000000")
                .expect("lookup absent event ID")
                .is_none()
        );
        assert!(
            store
                .get_by_seq(appended.seq + 1)
                .expect("lookup absent sequence")
                .is_none()
        );

        drop(store);
        let reopened = EventStore::open(&path).expect("reopen store");
        let reopened_by_id = reopened
            .get_by_event_id(&appended.event_id)
            .expect("lookup reopened event by ID")
            .expect("reopened event ID is present");
        let reopened_by_seq = reopened
            .get_by_seq(appended.seq)
            .expect("lookup reopened event by sequence")
            .expect("reopened sequence is present");
        assert_eq!(
            serde_json::to_string(&reopened_by_id).expect("serialize reopened ID lookup"),
            serde_json::to_string(&appended).expect("serialize appended event")
        );
        assert_eq!(
            serde_json::to_string(&reopened_by_seq).expect("serialize reopened sequence lookup"),
            serde_json::to_string(&appended).expect("serialize appended event")
        );
    }

    #[test]
    fn exact_event_lookup_distinguishes_missing_from_invalid_persisted_rows() {
        let directory = tempdir().expect("temporary directory");
        let store = EventStore::open(directory.path().join("state.db")).expect("open store");
        let invalid_actor = store
            .append(NewEvent::user_input("session", None, "invalid actor"))
            .expect("append actor fixture");
        let invalid_timestamp = store
            .append(NewEvent::user_input("session", None, "invalid timestamp"))
            .expect("append timestamp fixture");
        let invalid_payload = store
            .append(NewEvent::user_input("session", None, "invalid payload"))
            .expect("append payload fixture");

        assert!(
            store
                .get_by_event_id("01J00000000000000000000000")
                .expect("lookup missing event ID")
                .is_none()
        );
        assert!(
            store
                .get_by_seq(0)
                .expect("lookup missing sequence")
                .is_none()
        );

        let connection = store.connection().expect("lock connection");
        connection
            .execute_batch("DROP TRIGGER events_reject_update;")
            .expect("drop update guard for malformed-row fixture");
        connection
            .execute(
                "UPDATE events SET actor = 'forged' WHERE seq = ?1",
                [invalid_actor.seq],
            )
            .expect("corrupt actor fixture");
        connection
            .execute(
                "UPDATE events SET recorded_at = 'not-a-timestamp' WHERE seq = ?1",
                [invalid_timestamp.seq],
            )
            .expect("corrupt timestamp fixture");
        connection
            .execute(
                "UPDATE events SET payload_json = '{not-json' WHERE seq = ?1",
                [invalid_payload.seq],
            )
            .expect("corrupt payload fixture");
        drop(connection);

        assert!(matches!(
            store.get_by_event_id(&invalid_actor.event_id),
            Err(super::EventStoreError::InvalidActor(actor)) if actor == "forged"
        ));
        assert!(matches!(
            store.get_by_seq(invalid_actor.seq),
            Err(super::EventStoreError::InvalidActor(actor)) if actor == "forged"
        ));
        assert!(matches!(
            store.get_by_event_id(&invalid_timestamp.event_id),
            Err(super::EventStoreError::Timestamp(_))
        ));
        assert!(matches!(
            store.get_by_seq(invalid_timestamp.seq),
            Err(super::EventStoreError::Timestamp(_))
        ));
        assert!(matches!(
            store.get_by_event_id(&invalid_payload.event_id),
            Err(super::EventStoreError::Payload(_))
        ));
        assert!(matches!(
            store.get_by_seq(invalid_payload.seq),
            Err(super::EventStoreError::Payload(_))
        ));
    }

    #[test]
    fn append_timestamp_matches_reopened_record() {
        let directory = tempdir().expect("temporary directory");
        let path = directory.path().join("state.db");
        let store = EventStore::open(&path).expect("open store");
        let appended = store
            .append(NewEvent::user_input("session", None, "timestamp"))
            .expect("append event");
        drop(store);

        let reopened = EventStore::open(&path).expect("reopen store");
        let persisted = reopened.list(&EventQuery::default()).expect("list event");

        assert_eq!(persisted.len(), 1);
        assert_eq!(persisted[0].recorded_at, appended.recorded_at);
        assert_eq!(appended.recorded_at.timestamp_subsec_nanos() % 1_000_000, 0);
    }

    #[test]
    fn rejects_updates_and_deletes() {
        let directory = tempdir().expect("temporary directory");
        let store = EventStore::open(directory.path().join("state.db")).expect("open store");
        let event = store
            .append(NewEvent::user_input("alpha", None, "immutable"))
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

    #[test]
    fn paginates_a_stable_high_water_snapshot_without_gaps() {
        let directory = tempdir().expect("temporary directory");
        let store = EventStore::open(directory.path().join("state.db")).expect("open store");
        for index in 0..2_005 {
            store
                .append(NewEvent::user_input(
                    "session",
                    None,
                    format!("event-{index}"),
                ))
                .expect("append fixture");
        }

        let high_water = store.latest_seq().expect("latest sequence");
        store
            .append(NewEvent::user_input("session", None, "after-high-water"))
            .expect("append newer event");

        let mut cursor = 0;
        let mut collected = Vec::new();
        while cursor < high_water {
            let page = store
                .list_through(
                    &EventQuery {
                        after_seq: Some(cursor),
                        limit: Some(137),
                        session_id: Some("session".into()),
                        task_id: None,
                    },
                    high_water,
                )
                .expect("read page");
            if page.is_empty() {
                break;
            }
            cursor = page.last().expect("non-empty page").seq;
            collected.extend(page);
        }

        assert_eq!(collected.len(), 2_005);
        assert_eq!(collected.first().expect("first").seq, 1);
        assert_eq!(collected.last().expect("last").seq, high_water);
        assert!(
            collected
                .windows(2)
                .all(|pair| pair[1].seq == pair[0].seq + 1)
        );
    }

    #[cfg(unix)]
    #[test]
    fn sqlite_family_is_private_regular_and_owned_by_the_effective_user() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        let directory = tempdir().expect("temporary directory");
        let private = directory.path().join("private");
        std::fs::create_dir(&private).expect("create data directory");
        std::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o777))
            .expect("loosen fixture directory");
        let path = private.join("state.db");
        let store = EventStore::open(&path).expect("open private store");
        store
            .append(NewEvent::user_input("session", None, "private"))
            .expect("create WAL family");

        let directory_metadata = std::fs::symlink_metadata(&private).expect("directory metadata");
        assert_eq!(directory_metadata.permissions().mode() & 0o777, 0o700);
        // SAFETY: `geteuid` reads process identity and has no preconditions.
        let effective_uid = unsafe { libc::geteuid() };
        assert_eq!(directory_metadata.uid(), effective_uid);
        for member in super::sqlite_family_paths(&path) {
            let Ok(metadata) = std::fs::symlink_metadata(&member) else {
                continue;
            };
            assert!(metadata.is_file());
            assert!(!metadata.file_type().is_symlink());
            assert_eq!(metadata.uid(), effective_uid);
            assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
        }
    }

    #[cfg(unix)]
    #[test]
    fn sqlite_open_rejects_database_and_parent_symlinks() {
        use std::os::unix::fs::symlink;

        let directory = tempdir().expect("temporary directory");
        let private = directory.path().join("private");
        std::fs::create_dir(&private).expect("create private directory");
        let target = directory.path().join("target.db");
        std::fs::File::create(&target).expect("create symlink target");
        let database_link = private.join("state.db");
        symlink(&target, &database_link).expect("create database symlink");
        assert!(matches!(
            EventStore::open(&database_link),
            Err(super::EventStoreError::Io(error))
                if error.kind() == std::io::ErrorKind::PermissionDenied
        ));

        let real_parent = directory.path().join("real-parent");
        std::fs::create_dir(&real_parent).expect("create real parent");
        let parent_link = directory.path().join("parent-link");
        symlink(&real_parent, &parent_link).expect("create parent symlink");
        assert!(matches!(
            EventStore::open(parent_link.join("state.db")),
            Err(super::EventStoreError::Io(error))
                if error.kind() == std::io::ErrorKind::PermissionDenied
        ));
    }
}
