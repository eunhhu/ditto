use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
};

use chrono::{DateTime, SecondsFormat, Utc};
use ditto_protocol::{EventActor, EventQuery, EventRecord, NewEvent};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use thiserror::Error;
use ulid::Ulid;

const CURRENT_SCHEMA_VERSION: i64 = 2;

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
        enforce_private_sqlite_files(path)?;

        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
        })
    }

    pub fn append(&self, event: NewEvent) -> Result<EventRecord, EventStoreError> {
        let recorded_at = DateTime::from_timestamp_millis(Utc::now().timestamp_millis())
            .expect("a current UTC timestamp is representable at millisecond precision");
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
