//! Rebuildable context projection over Ditto's canonical event spine.
//!
//! The SQLite database owned here is a deletable cache. It never appends to or
//! mutates the event store, and it carries no retrieval-ranking authority.

use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
};

use chrono::{DateTime, Utc};
use ditto_context::{
    ContextNode, ContextOrigin, ContextScope, ContextValidationError, EpistemicStatus,
};
use ditto_event_store::{EventStore, EventStoreError};
use ditto_protocol::{EventActor, EventQuery, EventRecord, event_kind};
use ditto_retrieval::{
    CandidateCount, ContextNodeId, MAX_CANDIDATE_COUNT, RetrievalError, RetrievalWorkBudget,
    SessionId, TaskId,
};
use rusqlite::{Connection, OpenFlags, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

/// Version of the durable context-node event payload.
pub const CONTEXT_NODE_EVENT_VERSION: u16 = 1;
/// Version of the independently rebuildable projection schema.
pub const CONTEXT_PROJECTION_SCHEMA_VERSION: i64 = 3;
/// Fixed filename of the separate derived context cache.
pub const CONTEXT_PROJECTION_DATABASE_FILENAME: &str = "context-projection.db";
/// Maximum UTF-8 byte length of a durable node ID.
pub const MAX_CONTEXT_NODE_ID_BYTES: usize = 256;
/// Maximum UTF-8 byte length of a durable node summary.
pub const MAX_CONTEXT_SUMMARY_BYTES: usize = 65_000;
/// Maximum UTF-8 byte length of a source-event or supersession ID.
pub const MAX_CONTEXT_REFERENCE_ID_BYTES: usize = 256;
/// Maximum number of durable source references.
pub const MAX_CONTEXT_SOURCE_EVENT_IDS: usize = 64;
/// Maximum number of durable supersession references.
pub const MAX_CONTEXT_SUPERSEDES: usize = 64;
/// Maximum serialized JSON byte length of a durable context node.
pub const MAX_SERIALIZED_CONTEXT_NODE_BYTES: usize = 131_072;
/// Maximum serialized JSON byte length of a version-1 node payload.
pub const MAX_SERIALIZED_CONTEXT_PAYLOAD_BYTES: usize = 131_072;

const SYNC_PAGE_SIZE: usize = 500;
const ZERO_TASK_KEY: &str = "";
const EVENT_STORE_DATABASE_FILENAME: &str = "state.db";

const SCHEMA_V3: &str = r#"
CREATE TABLE projection_checkpoint (
    singleton       INTEGER PRIMARY KEY CHECK (singleton = 1),
    schema_version  INTEGER NOT NULL,
    through_seq     INTEGER NOT NULL CHECK (through_seq >= 0),
    through_event_id TEXT,
    CHECK (
        (through_seq = 0 AND through_event_id IS NULL)
        OR (through_seq > 0 AND through_event_id IS NOT NULL)
    )
);

INSERT INTO projection_checkpoint (
    singleton, schema_version, through_seq, through_event_id
) VALUES (1, 3, 0, NULL);

CREATE TABLE projected_nodes (
    session_id   TEXT NOT NULL,
    task_id      TEXT,
    node_id      TEXT NOT NULL,
    event_seq    INTEGER NOT NULL UNIQUE,
    event_id     TEXT NOT NULL UNIQUE,
    node_json    TEXT NOT NULL,
    epistemic_status TEXT NOT NULL DEFAULT 'asserted',
    valid_from_millis INTEGER,
    valid_from_submillis_nanos INTEGER,
    valid_until_millis INTEGER,
    valid_until_submillis_nanos INTEGER,
    CHECK (
        (valid_from_millis IS NULL AND valid_from_submillis_nanos IS NULL)
        OR (
            valid_from_millis IS NOT NULL
            AND valid_from_submillis_nanos IS NOT NULL
            AND valid_from_submillis_nanos BETWEEN 0 AND 999999
        )
    ),
    CHECK (
        (valid_until_millis IS NULL AND valid_until_submillis_nanos IS NULL)
        OR (
            valid_until_millis IS NOT NULL
            AND valid_until_submillis_nanos IS NOT NULL
            AND valid_until_submillis_nanos BETWEEN 0 AND 999999
        )
    ),
    PRIMARY KEY (session_id, node_id)
);

CREATE INDEX projected_nodes_scope_seq
ON projected_nodes(session_id, task_id, event_seq);

CREATE INDEX projected_nodes_active_scope
    ON projected_nodes(
        session_id, task_id, epistemic_status,
        valid_from_millis, valid_from_submillis_nanos,
        valid_until_millis, valid_until_submillis_nanos, node_id
    );

CREATE TABLE supersession_edges (
    session_id          TEXT NOT NULL,
    task_key            TEXT NOT NULL,
    superseding_node_id TEXT NOT NULL,
    superseded_node_id  TEXT NOT NULL,
    event_seq           INTEGER NOT NULL,
    UNIQUE (
        session_id, task_key, superseding_node_id, superseded_node_id
    )
);

CREATE INDEX supersession_edges_target
    ON supersession_edges(session_id, task_key, superseded_node_id);
"#;

/// Version-1 payload for `context.node.recorded`.
///
/// Serde's default additive-field behavior is intentional: rebuild consumers
/// ignore unknown fields but still require and validate `event_version` and
/// `node`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContextNodeRecordedPayloadV1 {
    pub event_version: u16,
    pub node: ContextNode,
}

impl ContextNodeRecordedPayloadV1 {
    pub fn new(node: ContextNode) -> Self {
        Self {
            event_version: CONTEXT_NODE_EVENT_VERSION,
            node,
        }
    }
}

/// Kernel-trusted context request. This type deliberately is not
/// deserializable and contains no event-envelope authority.
#[derive(Debug, Clone)]
pub struct ContextNodeDraft {
    node: ContextNode,
    session_id: String,
    task_id: Option<String>,
}

impl ContextNodeDraft {
    pub fn session(session_id: impl Into<String>, node: ContextNode) -> Self {
        Self {
            node,
            session_id: session_id.into(),
            task_id: None,
        }
    }

    pub fn task(
        session_id: impl Into<String>,
        task_id: impl Into<String>,
        node: ContextNode,
    ) -> Self {
        Self {
            node,
            session_id: session_id.into(),
            task_id: Some(task_id.into()),
        }
    }

    pub fn node(&self) -> &ContextNode {
        &self.node
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn task_id(&self) -> Option<&str> {
        self.task_id.as_deref()
    }
}

/// A live draft whose durable node, provenance, identity, and supersession
/// constraints were checked against canonical history and whose relevant
/// projection rows were verified or rebuilt once.
///
/// The kernel uses this value to construct the fixed system-authored envelope;
/// callers cannot supply causation through [`ContextNodeDraft`].
#[derive(Debug, Clone)]
pub struct ValidatedContextNodeDraft {
    node: ContextNode,
    session_id: String,
    task_id: Option<String>,
    causation_id: String,
}

impl ValidatedContextNodeDraft {
    pub fn node(&self) -> &ContextNode {
        &self.node
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn task_id(&self) -> Option<&str> {
        self.task_id.as_deref()
    }

    pub fn causation_id(&self) -> &str {
        &self.causation_id
    }

    /// Task records correlate to their task; session records correlate to the
    /// session. This value is derived, never draft-controlled.
    pub fn correlation_id(&self) -> &str {
        self.task_id.as_deref().unwrap_or(&self.session_id)
    }

    pub fn payload(&self) -> ContextNodeRecordedPayloadV1 {
        ContextNodeRecordedPayloadV1::new(self.node.clone())
    }
}

/// Exact durable anchor of the projection cache.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectionCheckpoint {
    pub schema_version: i64,
    pub through_seq: i64,
    pub through_event_id: Option<String>,
}

impl ProjectionCheckpoint {
    fn zero() -> Self {
        Self {
            schema_version: CONTEXT_PROJECTION_SCHEMA_VERSION,
            through_seq: 0,
            through_event_id: None,
        }
    }
}

/// Observable outcome of one bounded-high-water synchronization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectionSync {
    pub captured_high_water: i64,
    pub checkpoint: ProjectionCheckpoint,
    pub rebuilt: bool,
}

/// Canonical recording identity for one session-wide node ID.
///
/// Admission retries obtain this from bounded event-spine history, never from
/// a projection row, and never compare or rewrite the committed payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommittedContextIdentity {
    pub session_id: String,
    pub task_id: Option<String>,
    pub node_id: String,
    pub event_id: String,
    pub event_seq: i64,
}

/// Immutable scope-selected projection rows copied while the caller owns its
/// admission/snapshot gate. Embedding work can safely happen after that gate is
/// released.
#[derive(Debug, Clone)]
pub struct DerivedContextSnapshot {
    checkpoint: ProjectionCheckpoint,
    scanned_rows: usize,
    candidates: Vec<ContextNode>,
}

impl DerivedContextSnapshot {
    pub fn checkpoint(&self) -> &ProjectionCheckpoint {
        &self.checkpoint
    }

    /// Count of scope-selected rows inspected before supersession and every
    /// active or relevance filter.
    pub const fn scanned_rows(&self) -> usize {
        self.scanned_rows
    }

    /// Scope-selected, non-superseded nodes in durable sequence order.
    ///
    /// This detached slice carries no ranking authority. `ditto-context`
    /// applies active/validity policy, relevance, optional embedding ranking,
    /// and the requested result limit. It carries source authority only when
    /// returned by
    /// [`ContextProjection::synchronize_and_verified_snapshot_through`].
    pub fn candidates(&self) -> &[ContextNode] {
        &self.candidates
    }
}

/// Source-verified active context rows safe to transfer into authenticated
/// ranking. Unlike [`DerivedContextSnapshot`], this type can only be returned
/// after process-local source verification or bounded delta verification.
#[derive(Debug, Clone)]
pub struct VerifiedContextSnapshot {
    snapshot: DerivedContextSnapshot,
}

impl VerifiedContextSnapshot {
    pub fn checkpoint(&self) -> &ProjectionCheckpoint {
        self.snapshot.checkpoint()
    }

    pub const fn scanned_rows(&self) -> usize {
        self.snapshot.scanned_rows()
    }

    pub fn candidates(&self) -> &[ContextNode] {
        self.snapshot.candidates()
    }

    /// Transfer only source-verified candidates into ranking without cloning.
    pub fn into_candidates(self) -> Vec<ContextNode> {
        self.snapshot.candidates
    }
}

/// Inspectable source-verification work counters for steady-state regression
/// tests and local operational evidence.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProjectionVerificationMetrics {
    pub full_replays: u64,
    pub delta_synchronizations: u64,
    pub fast_snapshots: u64,
    pub cache_repairs: u64,
}

/// Typed projection, durable-admission, and V2 scan failures.
#[derive(Debug, Error)]
pub enum ContextProjectionError {
    #[error("context projection I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("context projection SQLite operation failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("refusing to open an event-store database as the context projection")]
    SourceDatabaseCollision,
    #[error("event store access failed: {0}")]
    EventStore(#[from] EventStoreError),
    #[error("shared retrieval query or context document is invalid: {0}")]
    Retrieval(#[from] RetrievalError),
    #[error("context projection mutex was poisoned")]
    Poisoned,
    #[error("requested high-water {requested} is ahead of event-store high-water {available}")]
    HighWaterAhead { requested: i64, available: i64 },
    #[error("requested high-water {requested} is behind projection checkpoint {checkpoint}")]
    HighWaterBehindCheckpoint { requested: i64, checkpoint: i64 },
    #[error("event page is not strictly increasing after {after}: found {found}")]
    NonMonotonicPage { after: i64, found: i64 },
    #[error("event pagination stopped at {cursor} before captured high-water {high_water}")]
    HighWaterUnreachable { cursor: i64, high_water: i64 },
    #[error(
        "requested target event {event_id} at sequence {seq} is not the canonical event-store record"
    )]
    TargetEventMismatch { event_id: String, seq: i64 },
    #[error(
        "projection is not synchronized (checkpoint {checkpoint}, event high-water {high_water})"
    )]
    ProjectionNotSynchronized { checkpoint: i64, high_water: i64 },
    #[error("context event {seq} has malformed payload: {reason}")]
    MalformedPayload { seq: i64, reason: String },
    #[error("context event {seq} uses unsupported payload version {found}")]
    UnsupportedEventVersion { seq: i64, found: u64 },
    #[error("context event {seq} actor is {found}, expected system")]
    InvalidActor { seq: i64, found: EventActor },
    #[error("context event {seq} has a tracing span; version 1 requires no span")]
    UnexpectedSpan { seq: i64 },
    #[error("context event {seq} has invalid scope: {reason}")]
    InvalidScope { seq: i64, reason: String },
    #[error("context event {seq} has correlation {actual:?}, expected {expected}")]
    CorrelationMismatch {
        seq: i64,
        expected: String,
        actual: Option<String>,
    },
    #[error("context event {seq} has causation {actual:?}, expected {expected}")]
    CausationMismatch {
        seq: i64,
        expected: String,
        actual: Option<String>,
    },
    #[error("context node {node_id} is invalid: {reason}")]
    InvalidNode { node_id: String, reason: String },
    #[error("context node {node_id} {field} is not millisecond-canonical")]
    NonCanonicalValidityPrecision {
        node_id: String,
        field: &'static str,
    },
    #[error("{field} is {actual} bytes, exceeding the {maximum}-byte durable limit")]
    DurableBytesExceeded {
        field: &'static str,
        actual: usize,
        maximum: usize,
    },
    #[error("{field} has {actual} entries, outside the durable range {minimum}..={maximum}")]
    DurableListOutOfRange {
        field: &'static str,
        actual: usize,
        minimum: usize,
        maximum: usize,
    },
    #[error("{field} contains an empty identifier at index {index}")]
    EmptyReference { field: &'static str, index: usize },
    #[error("{field} contains duplicate identifier {id}")]
    DuplicateReference { field: &'static str, id: String },
    #[error("context node {node_id} cannot supersede itself")]
    SelfSupersession { node_id: String },
    #[error("context node {node_id} cites missing source event {source_event_id}")]
    MissingSource {
        node_id: String,
        source_event_id: String,
    },
    #[error(
        "context node {node_id} cites non-prior source event {source_event_id} at sequence {source_seq}"
    )]
    SourceNotPrior {
        node_id: String,
        source_event_id: String,
        source_seq: i64,
    },
    #[error("context node {node_id} source {source_event_id} belongs to another session")]
    SourceSessionMismatch {
        node_id: String,
        source_event_id: String,
    },
    #[error("context node {node_id} source {source_event_id} belongs to an incompatible task")]
    SourceTaskMismatch {
        node_id: String,
        source_event_id: String,
    },
    #[error("context node {node_id} origin {origin:?} is not attested by a matching source actor")]
    OriginNotAttested {
        node_id: String,
        origin: ContextOrigin,
    },
    #[error(
        "context node identity ({session_id}, {node_id}) already belongs to event {event_id} at sequence {seq}"
    )]
    DuplicateNodeIdentity {
        session_id: String,
        node_id: String,
        event_id: String,
        seq: i64,
    },
    #[error("context node {node_id} supersedes missing node {superseded_id} in its exact scope")]
    MissingSupersededNode {
        node_id: String,
        superseded_id: String,
    },
    #[error("context node {node_id} supersedes node {superseded_id} from another scope")]
    SupersessionScopeMismatch {
        node_id: String,
        superseded_id: String,
    },
    #[error("projection row at sequence {seq} is corrupt: {reason}")]
    CorruptProjectionRow { seq: i64, reason: String },
    #[error(
        "context projection snapshot remains inconsistent with canonical history through sequence {high_water} after one rebuild"
    )]
    ProjectionSnapshotIntegrityMismatch { high_water: i64 },
    #[error("projection singleton checkpoint disappeared during page application")]
    MissingCheckpointDuringPage,
}

/// Separately persisted, rebuildable projection cache.
#[derive(Clone)]
pub struct ContextProjection {
    connection: Arc<Mutex<Connection>>,
    sync_gate: Arc<Mutex<()>>,
    verification: Arc<Mutex<ProjectionVerificationState>>,
    path: Arc<PathBuf>,
    #[cfg(test)]
    post_rebuild_snapshot_hook: Arc<Mutex<Option<PostRebuildSnapshotHook>>>,
    #[cfg(test)]
    post_active_snapshot_hook: Arc<Mutex<Option<PostActiveSnapshotHook>>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SourceVerification {
    checkpoint: ProjectionCheckpoint,
    sqlite_data_version: i64,
}

#[derive(Debug, Default)]
struct ProjectionVerificationState {
    source: Option<SourceVerification>,
    metrics: ProjectionVerificationMetrics,
}

#[cfg(test)]
type PostRebuildSnapshotHook = Arc<dyn Fn(&Path) + Send + Sync>;
#[cfg(test)]
type PostActiveSnapshotHook = Box<dyn FnOnce(&Path) + Send>;

impl ContextProjection {
    /// Open the fixed projection database inside a Ditto data directory.
    pub fn open_in(data_directory: impl AsRef<Path>) -> Result<Self, ContextProjectionError> {
        Self::open(
            data_directory
                .as_ref()
                .join(CONTEXT_PROJECTION_DATABASE_FILENAME),
        )
    }

    pub fn open(path: impl AsRef<Path>) -> Result<Self, ContextProjectionError> {
        let path = path.as_ref();
        if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.eq_ignore_ascii_case(EVENT_STORE_DATABASE_FILENAME))
        {
            return Err(ContextProjectionError::SourceDatabaseCollision);
        }
        let sqlite_path = prepare_private_sqlite_path(path)?;

        let mut connection = Connection::open_with_flags(
            &sqlite_path,
            OpenFlags::default() | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        let contains_event_spine: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'events')",
            [],
            |row| row.get(0),
        )?;
        if contains_event_spine {
            return Err(ContextProjectionError::SourceDatabaseCollision);
        }
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "synchronous", "NORMAL")?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        ensure_schema(&mut connection)?;
        enforce_private_sqlite_files(path)?;

        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
            sync_gate: Arc::new(Mutex::new(())),
            verification: Arc::new(Mutex::new(ProjectionVerificationState::default())),
            path: Arc::new(path.to_path_buf()),
            #[cfg(test)]
            post_rebuild_snapshot_hook: Arc::new(Mutex::new(None)),
            #[cfg(test)]
            post_active_snapshot_hook: Arc::new(Mutex::new(None)),
        })
    }

    /// Path of the derived database. This supports operational deletion and
    /// real SQLite failure injection; it conveys no source authority.
    pub fn database_path(&self) -> &Path {
        self.path.as_path()
    }

    pub fn checkpoint(&self) -> Result<ProjectionCheckpoint, ContextProjectionError> {
        let connection = self.connection()?;
        read_checkpoint(&connection)?.ok_or_else(|| ContextProjectionError::CorruptProjectionRow {
            seq: 0,
            reason: "singleton checkpoint is missing".into(),
        })
    }

    pub fn verification_metrics(
        &self,
    ) -> Result<ProjectionVerificationMetrics, ContextProjectionError> {
        Ok(self.verification_state()?.metrics)
    }

    fn has_source_verification(&self) -> Result<bool, ContextProjectionError> {
        Ok(self.verification_state()?.source.is_some())
    }

    fn source_verification_checkpoint(
        &self,
    ) -> Result<Option<ProjectionCheckpoint>, ContextProjectionError> {
        Ok(self
            .verification_state()?
            .source
            .as_ref()
            .map(|verified| verified.checkpoint.clone()))
    }

    fn sqlite_data_version(&self) -> Result<i64, ContextProjectionError> {
        self.connection()?
            .pragma_query_value(None, "data_version", |row| row.get(0))
            .map_err(Into::into)
    }

    fn source_verification_matches_cache(&self) -> Result<bool, ContextProjectionError> {
        let (checkpoint, sqlite_data_version) = {
            let connection = self.connection()?;
            (
                read_checkpoint(&connection)?,
                connection.pragma_query_value(None, "data_version", |row| row.get::<_, i64>(0))?,
            )
        };
        let state = self.verification_state()?;
        Ok(match (&state.source, checkpoint) {
            (Some(source), Some(checkpoint)) => {
                source.checkpoint == checkpoint && source.sqlite_data_version == sqlite_data_version
            }
            _ => false,
        })
    }

    fn record_source_verification(
        &self,
        checkpoint: &ProjectionCheckpoint,
        full_replay: bool,
        cache_repair: bool,
    ) -> Result<(), ContextProjectionError> {
        let sqlite_data_version = self.sqlite_data_version()?;
        let mut state = self.verification_state()?;
        state.source = Some(SourceVerification {
            checkpoint: checkpoint.clone(),
            sqlite_data_version,
        });
        if full_replay {
            state.metrics.full_replays = state.metrics.full_replays.saturating_add(1);
        }
        if cache_repair {
            state.metrics.cache_repairs = state.metrics.cache_repairs.saturating_add(1);
        }
        Ok(())
    }

    fn record_delta_verification(
        &self,
        checkpoint: &ProjectionCheckpoint,
        advanced: bool,
    ) -> Result<(), ContextProjectionError> {
        let sqlite_data_version = self.sqlite_data_version()?;
        let mut state = self.verification_state()?;
        state.source = Some(SourceVerification {
            checkpoint: checkpoint.clone(),
            sqlite_data_version,
        });
        if advanced {
            state.metrics.delta_synchronizations =
                state.metrics.delta_synchronizations.saturating_add(1);
        }
        Ok(())
    }

    fn increment_fast_snapshot(&self) -> Result<(), ContextProjectionError> {
        let mut state = self.verification_state()?;
        state.metrics.fast_snapshots = state.metrics.fast_snapshots.saturating_add(1);
        Ok(())
    }

    fn preserve_source_verification_after_sync(
        &self,
        synchronized: &ProjectionSync,
        source_verified: bool,
        before: Option<&ProjectionCheckpoint>,
        cache_started_at_zero: bool,
    ) -> Result<(), ContextProjectionError> {
        if synchronized.rebuilt {
            return self.record_source_verification(
                &synchronized.checkpoint,
                true,
                source_verified,
            );
        }
        if cache_started_at_zero && !source_verified {
            return self.record_source_verification(&synchronized.checkpoint, true, false);
        }
        if source_verified {
            let advanced = before.is_none_or(|checkpoint| {
                checkpoint.through_seq < synchronized.checkpoint.through_seq
            });
            self.record_delta_verification(&synchronized.checkpoint, advanced)?;
        }
        Ok(())
    }

    fn snapshot_sync_locked(
        &self,
        event_store: &EventStore,
        high_water: i64,
    ) -> Result<ProjectionSync, ContextProjectionError> {
        match self.synchronize_through_locked_internal(event_store, high_water, false) {
            Ok(synchronized) => Ok(synchronized),
            Err(SynchronizeThroughError::Public(error)) => Err(error),
            Err(SynchronizeThroughError::PersistentCacheDivergence { .. }) => {
                Err(ContextProjectionError::ProjectionSnapshotIntegrityMismatch { high_water })
            }
        }
    }

    fn rebuild_verified_through_locked(
        &self,
        event_store: &EventStore,
        high_water: i64,
        cache_repair: bool,
    ) -> Result<ProjectionSync, ContextProjectionError> {
        {
            let mut connection = self.connection()?;
            reset_schema(&mut connection)?;
        }
        let synchronized =
            match self.synchronize_through_locked_internal(event_store, high_water, true) {
                Ok(synchronized) => synchronized,
                Err(SynchronizeThroughError::Public(error)) => return Err(error),
                Err(SynchronizeThroughError::PersistentCacheDivergence { .. }) => {
                    return Err(
                        ContextProjectionError::ProjectionSnapshotIntegrityMismatch { high_water },
                    );
                }
            };
        self.record_source_verification(&synchronized.checkpoint, true, cache_repair)?;
        Ok(synchronized)
    }

    /// Capture the event-spine high-water once and synchronize only through
    /// that stable cutoff.
    pub fn synchronize(
        &self,
        event_store: &EventStore,
    ) -> Result<ProjectionSync, ContextProjectionError> {
        let _gate = self.sync_gate()?;
        let high_water = event_store.latest_seq()?;
        let source_verified = self.source_verification_matches_cache()?;
        let before = self.source_verification_checkpoint()?;
        let cache_started_at_zero = self.checkpoint()?.through_seq == 0;
        let synchronized = self.synchronize_through_locked(event_store, high_water, false)?;
        self.preserve_source_verification_after_sync(
            &synchronized,
            source_verified,
            before.as_ref(),
            cache_started_at_zero,
        )?;
        Ok(synchronized)
    }

    /// Synchronize through a caller-captured event-spine high-water.
    pub fn synchronize_through(
        &self,
        event_store: &EventStore,
        high_water: i64,
    ) -> Result<ProjectionSync, ContextProjectionError> {
        let _gate = self.sync_gate()?;
        let source_verified = self.source_verification_matches_cache()?;
        let before = self.source_verification_checkpoint()?;
        let cache_started_at_zero = self.checkpoint()?.through_seq == 0;
        let synchronized = self.synchronize_through_locked(event_store, high_water, false)?;
        self.preserve_source_verification_after_sync(
            &synchronized,
            source_verified,
            before.as_ref(),
            cache_started_at_zero,
        )?;
        Ok(synchronized)
    }

    /// Synchronize through one exact canonical event. Both its sequence and
    /// identity are checked before replay and against the resulting anchor.
    pub fn synchronize_through_event(
        &self,
        event_store: &EventStore,
        event: &EventRecord,
    ) -> Result<ProjectionSync, ContextProjectionError> {
        let by_seq = event_store.get_by_seq(event.seq)?;
        let by_id = event_store.get_by_event_id(&event.event_id)?;
        let matches = by_seq
            .as_ref()
            .is_some_and(|stored| stored.seq == event.seq && stored.event_id == event.event_id)
            && by_id
                .as_ref()
                .is_some_and(|stored| stored.seq == event.seq && stored.event_id == event.event_id);
        if !matches {
            return Err(ContextProjectionError::TargetEventMismatch {
                event_id: event.event_id.clone(),
                seq: event.seq,
            });
        }
        let sync = self.synchronize_through(event_store, event.seq)?;
        if sync.checkpoint.through_seq != event.seq
            || sync.checkpoint.through_event_id.as_deref() != Some(&event.event_id)
        {
            return Err(ContextProjectionError::TargetEventMismatch {
                event_id: event.event_id.clone(),
                seq: event.seq,
            });
        }
        Ok(sync)
    }

    /// Explicitly discard only derived tables and replay the current event
    /// spine from sequence zero.
    pub fn rebuild(
        &self,
        event_store: &EventStore,
    ) -> Result<ProjectionSync, ContextProjectionError> {
        let _gate = self.sync_gate()?;
        let high_water = event_store.latest_seq()?;
        self.rebuild_verified_through_locked(event_store, high_water, false)
    }

    /// Synchronize and copy one immutable derived query scope under the
    /// projection's own gate.
    ///
    /// This low-level compatibility helper does not compare the cache with
    /// canonical scoped history and therefore conveys no source authority.
    pub fn synchronize_and_snapshot(
        &self,
        event_store: &EventStore,
        session_id: &str,
        task_id: Option<&str>,
    ) -> Result<DerivedContextSnapshot, ContextProjectionError> {
        let _gate = self.sync_gate()?;
        let high_water = event_store.latest_seq()?;
        self.synchronize_through_locked(event_store, high_water, false)?;
        self.capture_snapshot_locked(session_id, task_id)
    }

    /// Synchronize through an explicit canonical high-water and return one
    /// source-verified session-root plus exact-task snapshot.
    ///
    /// The canonical event spine defines the complete requested scope. The
    /// projection is compared with that bounded view inside the same SQLite
    /// read transaction that materializes the detached snapshot. A logical
    /// mismatch resets and replays the derived cache exactly once at the same
    /// high-water; a repeated mismatch fails without returning partial rows.
    pub fn synchronize_and_verified_snapshot_through(
        &self,
        event_store: &EventStore,
        high_water: i64,
        session_id: &str,
        task_id: Option<&str>,
    ) -> Result<VerifiedContextSnapshot, ContextProjectionError> {
        let mut budget = RetrievalWorkBudget::new();
        self.synchronize_and_verified_snapshot_through_at(
            event_store,
            high_water,
            session_id,
            task_id,
            Utc::now(),
            &mut budget,
        )
    }

    /// Return lifecycle-active rows through a source-verified generation while
    /// charging candidate bytes to the joint working-set envelope.
    pub fn synchronize_and_verified_snapshot_through_at(
        &self,
        event_store: &EventStore,
        high_water: i64,
        session_id: &str,
        task_id: Option<&str>,
        evaluated_at: DateTime<Utc>,
        budget: &mut RetrievalWorkBudget,
    ) -> Result<VerifiedContextSnapshot, ContextProjectionError> {
        let _gate = self.sync_gate()?;
        validate_query_scope(session_id, task_id)?;
        let available = event_store.latest_seq()?;
        if high_water < 0 || high_water > available {
            return Err(ContextProjectionError::HighWaterAhead {
                requested: high_water,
                available,
            });
        }

        let had_source_verification = self.has_source_verification()?;
        let mut rebuilt = false;
        let source_checkpoint = self.source_verification_checkpoint()?;
        let cache_checkpoint = self.checkpoint()?;
        if self.source_verification_matches_cache()? {
            let before = source_checkpoint;
            let synchronized = self.snapshot_sync_locked(event_store, high_water)?;
            if synchronized.rebuilt {
                self.record_source_verification(&synchronized.checkpoint, true, true)?;
                rebuilt = true;
            } else {
                let advanced = before
                    .as_ref()
                    .is_none_or(|checkpoint| checkpoint.through_seq < high_water);
                self.record_delta_verification(&synchronized.checkpoint, advanced)?;
            }
        } else if source_checkpoint.as_ref() == Some(&cache_checkpoint) {
            // Preserve canonical delta failure precedence before repairing an
            // externally changed cache generation.
            let synchronized = self.snapshot_sync_locked(event_store, high_water)?;
            if synchronized.rebuilt {
                self.record_source_verification(&synchronized.checkpoint, true, true)?;
            } else {
                self.rebuild_verified_through_locked(event_store, high_water, true)?;
            }
            rebuilt = true;
        } else {
            self.rebuild_verified_through_locked(event_store, high_water, had_source_verification)?;
            rebuilt = true;
        }
        #[cfg(test)]
        if rebuilt {
            self.run_post_rebuild_snapshot_hook()?;
        }
        if !self.source_verification_matches_cache()? {
            if rebuilt {
                return Err(
                    ContextProjectionError::ProjectionSnapshotIntegrityMismatch { high_water },
                );
            }
            self.rebuild_verified_through_locked(event_store, high_water, true)?;
            rebuilt = true;
            #[cfg(test)]
            self.run_post_rebuild_snapshot_hook()?;
        }

        let mut attempted_budget = budget.clone();
        let mut snapshot = self.capture_active_snapshot_locked(
            session_id,
            task_id,
            evaluated_at,
            &mut attempted_budget,
        )?;
        #[cfg(test)]
        self.run_post_active_snapshot_hook()?;
        if !self.source_verification_matches_cache()? {
            if rebuilt {
                return Err(
                    ContextProjectionError::ProjectionSnapshotIntegrityMismatch { high_water },
                );
            }
            self.rebuild_verified_through_locked(event_store, high_water, true)?;
            #[cfg(test)]
            self.run_post_rebuild_snapshot_hook()?;
            if !self.source_verification_matches_cache()? {
                return Err(
                    ContextProjectionError::ProjectionSnapshotIntegrityMismatch { high_water },
                );
            }
            snapshot = self.capture_active_snapshot_locked(
                session_id,
                task_id,
                evaluated_at,
                &mut attempted_budget,
            )?;
            #[cfg(test)]
            self.run_post_active_snapshot_hook()?;
        }
        if snapshot.checkpoint.through_seq != high_water {
            return Err(ContextProjectionError::ProjectionSnapshotIntegrityMismatch { high_water });
        }

        *budget = attempted_budget;
        self.increment_fast_snapshot()?;
        Ok(VerifiedContextSnapshot { snapshot })
    }

    /// Explicitly compare an entire selected scope with canonical history.
    /// Normal working-set retrieval never calls this O(session-history) audit.
    pub fn audit_source_consistency_through(
        &self,
        event_store: &EventStore,
        high_water: i64,
        session_id: &str,
        task_id: Option<&str>,
    ) -> Result<ProjectionCheckpoint, ContextProjectionError> {
        let _gate = self.sync_gate()?;
        validate_query_scope(session_id, task_id)?;
        let available = event_store.latest_seq()?;
        if high_water < 0 || high_water > available {
            return Err(ContextProjectionError::HighWaterAhead {
                requested: high_water,
                available,
            });
        }
        let synchronized = self.snapshot_sync_locked(event_store, high_water)?;
        let canonical =
            canonical_scope_snapshot_view(event_store, high_water, session_id, task_id)?;
        if self
            .verified_snapshot_attempt(&canonical, &synchronized.checkpoint, session_id, task_id)?
            .is_some()
        {
            if synchronized.rebuilt {
                self.record_source_verification(&synchronized.checkpoint, true, true)?;
            } else {
                self.record_delta_verification(&synchronized.checkpoint, false)?;
            }
            return Ok(synchronized.checkpoint);
        }
        if synchronized.rebuilt {
            return Err(ContextProjectionError::ProjectionSnapshotIntegrityMismatch { high_water });
        }

        let rebuilt = self.rebuild_verified_through_locked(event_store, high_water, true)?;
        #[cfg(test)]
        self.run_post_rebuild_snapshot_hook()?;
        if self
            .verified_snapshot_attempt(&canonical, &rebuilt.checkpoint, session_id, task_id)?
            .is_some()
        {
            return Ok(rebuilt.checkpoint);
        }
        Err(ContextProjectionError::ProjectionSnapshotIntegrityMismatch { high_water })
    }

    /// Copy the currently projected session-root plus exact-task namespace.
    /// Every selected row is counted before active/relevance filtering.
    ///
    /// This is a low-level derived-cache inspection helper. Its result has no
    /// source authority without the canonical comparison performed by
    /// [`Self::synchronize_and_verified_snapshot_through`].
    pub fn capture_snapshot(
        &self,
        session_id: &str,
        task_id: Option<&str>,
    ) -> Result<DerivedContextSnapshot, ContextProjectionError> {
        let _gate = self.sync_gate()?;
        self.capture_snapshot_locked(session_id, task_id)
    }

    /// Look up one committed identity in canonical session history through an
    /// explicit captured high-water.
    ///
    /// The projection database is deliberately not consulted for authority.
    pub fn lookup_committed_identity(
        &self,
        event_store: &EventStore,
        high_water: i64,
        session_id: &str,
        node_id: &str,
    ) -> Result<Option<CommittedContextIdentity>, ContextProjectionError> {
        if session_id.trim().is_empty() || node_id.trim().is_empty() {
            return Err(ContextProjectionError::InvalidScope {
                seq: 0,
                reason: "identity lookup session/node ID is empty".into(),
            });
        }
        bounded_bytes("context node id", node_id.len(), MAX_CONTEXT_NODE_ID_BYTES)?;
        let _gate = self.sync_gate()?;
        let available = event_store.latest_seq()?;
        if high_water < 0 || high_water > available {
            return Err(ContextProjectionError::HighWaterAhead {
                requested: high_water,
                available,
            });
        }
        let targets = HashSet::from([node_id.to_owned()]);
        let view = canonical_session_admission_view(event_store, high_water, session_id, &targets)?;
        Ok(view.rows.get(node_id).map(|row| CommittedContextIdentity {
            session_id: session_id.to_owned(),
            task_id: row.task_id.clone(),
            node_id: node_id.to_owned(),
            event_id: row.event_id.clone(),
            event_seq: row.event_seq,
        }))
    }

    /// Validate a trusted live draft against the same durable rules used by
    /// replay and return the deterministic greatest-source-sequence causation.
    /// The caller supplies its captured high-water and must have synchronized
    /// through that exact cutoff first; stale projections fail closed.
    pub fn validate_draft(
        &self,
        event_store: &EventStore,
        high_water: i64,
        draft: &ContextNodeDraft,
    ) -> Result<ValidatedContextNodeDraft, ContextProjectionError> {
        let _gate = self.sync_gate()?;
        SessionId::new(draft.session_id.clone())?;
        if let Some(task_id) = &draft.task_id {
            TaskId::new(task_id.clone())?;
        }
        ContextNodeId::new(draft.node.id.clone())?;
        validate_node_identity_shape(&draft.node)?;
        validate_new_validity_precision(&draft.node)?;
        let available = event_store.latest_seq()?;
        if high_water < 0 || high_water > available {
            return Err(ContextProjectionError::HighWaterAhead {
                requested: high_water,
                available,
            });
        }
        {
            let connection = self.connection()?;
            let checkpoint = read_checkpoint(&connection)?.ok_or_else(|| {
                ContextProjectionError::ProjectionNotSynchronized {
                    checkpoint: 0,
                    high_water,
                }
            })?;
            if checkpoint.through_seq != high_water
                || !checkpoint_anchor_matches(&checkpoint, event_store)?
            {
                return Err(ContextProjectionError::ProjectionNotSynchronized {
                    checkpoint: checkpoint.through_seq,
                    high_water,
                });
            }
        }

        if draft.session_id.trim().is_empty() {
            return Err(ContextProjectionError::InvalidScope {
                seq: 0,
                reason: "session_id is empty".into(),
            });
        }
        let mut rebuilt = false;
        let proposed_targets = HashSet::from([draft.node.id.clone()]);
        let proposed_view = self.admission_view_locked(
            event_store,
            high_water,
            &draft.session_id,
            &proposed_targets,
            &mut rebuilt,
        )?;
        if let Some(existing) = proposed_view.rows.get(&draft.node.id) {
            return Err(ContextProjectionError::DuplicateNodeIdentity {
                session_id: draft.session_id.clone(),
                node_id: draft.node.id.clone(),
                event_id: existing.event_id.clone(),
                seq: existing.event_seq,
            });
        }

        let scope = validate_requested_scope(
            0,
            &draft.session_id,
            draft.task_id.as_deref(),
            draft.node.scope,
        )?;
        validate_durable_node(&draft.node)?;

        let targets = std::iter::once(draft.node.id.clone())
            .chain(draft.node.supersedes.iter().cloned())
            .collect::<HashSet<_>>();
        let canonical_view = self.admission_view_locked(
            event_store,
            high_water,
            &draft.session_id,
            &targets,
            &mut rebuilt,
        )?;
        for superseded_id in &draft.node.supersedes {
            match canonical_view.rows.get(superseded_id) {
                None => {
                    return Err(ContextProjectionError::MissingSupersededNode {
                        node_id: draft.node.id.clone(),
                        superseded_id: superseded_id.clone(),
                    });
                }
                Some(existing) if existing.task_id.as_deref() != scope.task_id => {
                    return Err(ContextProjectionError::SupersessionScopeMismatch {
                        node_id: draft.node.id.clone(),
                        superseded_id: superseded_id.clone(),
                    });
                }
                Some(_) => {}
            }
        }
        let resolved = resolve_sources(
            event_store,
            &draft.node,
            &scope,
            high_water.saturating_add(1),
        )?;

        Ok(ValidatedContextNodeDraft {
            node: draft.node.clone(),
            session_id: draft.session_id.clone(),
            task_id: draft.task_id.clone(),
            causation_id: resolved.causation_id,
        })
    }

    fn admission_view_locked(
        &self,
        event_store: &EventStore,
        high_water: i64,
        session_id: &str,
        targets: &HashSet<String>,
        rebuilt: &mut bool,
    ) -> Result<CanonicalAdmissionView, ContextProjectionError> {
        let view = canonical_session_admission_view(event_store, high_water, session_id, targets)?;
        let matches = {
            let connection = self.connection()?;
            relevant_cache_matches(&connection, session_id, targets, &view)?
        };
        if matches {
            return Ok(view);
        }
        if *rebuilt {
            let checkpoint = self.checkpoint()?;
            return Err(ContextProjectionError::ProjectionNotSynchronized {
                checkpoint: checkpoint.through_seq,
                high_water,
            });
        }

        {
            let mut connection = self.connection()?;
            reset_schema(&mut connection)?;
        }
        self.synchronize_through_locked(event_store, high_water, true)?;
        *rebuilt = true;

        let repaired =
            canonical_session_admission_view(event_store, high_water, session_id, targets)?;
        let matches = {
            let connection = self.connection()?;
            relevant_cache_matches(&connection, session_id, targets, &repaired)?
        };
        if !matches {
            return Err(ContextProjectionError::ProjectionNotSynchronized {
                checkpoint: self.checkpoint()?.through_seq,
                high_water,
            });
        }
        Ok(repaired)
    }

    fn synchronize_through_locked(
        &self,
        event_store: &EventStore,
        high_water: i64,
        already_rebuilt: bool,
    ) -> Result<ProjectionSync, ContextProjectionError> {
        match self.synchronize_through_locked_internal(event_store, high_water, already_rebuilt) {
            Ok(synchronized) => Ok(synchronized),
            Err(SynchronizeThroughError::Public(error)) => Err(error),
            Err(SynchronizeThroughError::PersistentCacheDivergence { checkpoint }) => {
                Err(ContextProjectionError::ProjectionNotSynchronized {
                    checkpoint,
                    high_water,
                })
            }
        }
    }

    fn synchronize_through_locked_internal(
        &self,
        event_store: &EventStore,
        high_water: i64,
        already_rebuilt: bool,
    ) -> Result<ProjectionSync, SynchronizeThroughError> {
        let available = event_store
            .latest_seq()
            .map_err(ContextProjectionError::from)?;
        if high_water < 0 || high_water > available {
            return Err(ContextProjectionError::HighWaterAhead {
                requested: high_water,
                available,
            }
            .into());
        }

        let mut rebuilt = already_rebuilt;
        let mut checkpoint = {
            let mut connection = self.connection()?;
            let schema_current = schema_is_current(&connection)?;
            let stored_checkpoint = if schema_current {
                read_checkpoint(&connection)?
            } else {
                None
            };
            match stored_checkpoint {
                Some(checkpoint)
                    if checkpoint.schema_version == CONTEXT_PROJECTION_SCHEMA_VERSION
                        && checkpoint.through_seq <= available
                        && checkpoint_anchor_matches(&checkpoint, event_store)? =>
                {
                    checkpoint
                }
                _ => {
                    reset_schema(&mut connection)?;
                    rebuilt = true;
                    ProjectionCheckpoint::zero()
                }
            }
        };

        if high_water < checkpoint.through_seq {
            return Err(ContextProjectionError::HighWaterBehindCheckpoint {
                requested: high_water,
                checkpoint: checkpoint.through_seq,
            }
            .into());
        }

        if checkpoint.through_seq < high_water
            && !self.delta_dependencies_match_cache(
                event_store,
                checkpoint.through_seq,
                high_water,
            )?
        {
            if rebuilt {
                return Err(SynchronizeThroughError::PersistentCacheDivergence {
                    checkpoint: checkpoint.through_seq,
                });
            }
            {
                let mut connection = self.connection()?;
                reset_schema(&mut connection)?;
            }
            rebuilt = true;
            checkpoint = ProjectionCheckpoint::zero();
        }

        let checkpoint = match self.catch_up_from_checkpoint(event_store, high_water, checkpoint) {
            Ok(checkpoint) => checkpoint,
            Err(CatchUpError::Public(error)) => return Err(error.into()),
            Err(CatchUpError::RepairableCacheDivergence) if !rebuilt => {
                {
                    let mut connection = self.connection()?;
                    reset_schema(&mut connection)?;
                }
                rebuilt = true;
                match self.catch_up_from_checkpoint(
                    event_store,
                    high_water,
                    ProjectionCheckpoint::zero(),
                ) {
                    Ok(checkpoint) => checkpoint,
                    Err(CatchUpError::Public(error)) => return Err(error.into()),
                    Err(CatchUpError::RepairableCacheDivergence) => {
                        return Err(SynchronizeThroughError::PersistentCacheDivergence {
                            checkpoint: self.checkpoint()?.through_seq,
                        });
                    }
                }
            }
            Err(CatchUpError::RepairableCacheDivergence) => {
                return Err(SynchronizeThroughError::PersistentCacheDivergence {
                    checkpoint: self.checkpoint()?.through_seq,
                });
            }
        };

        Ok(ProjectionSync {
            captured_high_water: high_water,
            checkpoint,
            rebuilt,
        })
    }

    fn delta_dependencies_match_cache(
        &self,
        event_store: &EventStore,
        after_seq: i64,
        through_seq: i64,
    ) -> Result<bool, ContextProjectionError> {
        let mut connection = self.connection()?;
        ensure_delta_preflight_tables(&connection)?;
        let transaction = connection.transaction()?;
        clear_delta_preflight_tables(&transaction)?;
        collect_delta_dependencies(&transaction, event_store, after_seq, through_seq)?;
        seed_canonical_delta_dependencies(&transaction, event_store, after_seq)?;
        let matches = delta_dependency_cache_matches(&transaction)?;
        validate_delta_against_canonical_dependencies(
            &transaction,
            event_store,
            after_seq,
            through_seq,
        )?;
        clear_delta_preflight_tables(&transaction)?;
        transaction.commit()?;
        Ok(matches)
    }

    fn catch_up_from_checkpoint(
        &self,
        event_store: &EventStore,
        high_water: i64,
        mut checkpoint: ProjectionCheckpoint,
    ) -> Result<ProjectionCheckpoint, CatchUpError> {
        let mut cursor = checkpoint.through_seq;
        while cursor < high_water {
            let page = event_store
                .list_through(
                    &EventQuery {
                        after_seq: Some(cursor),
                        limit: Some(SYNC_PAGE_SIZE),
                        ..EventQuery::default()
                    },
                    high_water,
                )
                .map_err(ContextProjectionError::from)?;
            let Some(last) = page.last().cloned() else {
                return Err(
                    ContextProjectionError::HighWaterUnreachable { cursor, high_water }.into(),
                );
            };
            let mut previous = cursor;
            for event in &page {
                if event.seq <= previous || event.seq > high_water {
                    return Err(ContextProjectionError::NonMonotonicPage {
                        after: previous,
                        found: event.seq,
                    }
                    .into());
                }
                previous = event.seq;
            }

            {
                let mut connection = self.connection()?;
                let transaction = connection
                    .transaction()
                    .map_err(ContextProjectionError::from)?;
                for event in &page {
                    if event.kind == event_kind::CONTEXT_NODE_RECORDED {
                        apply_context_event(&transaction, event_store, event)?;
                    }
                }
                let updated = transaction.execute(
                    "UPDATE projection_checkpoint SET schema_version = ?1, through_seq = ?2, through_event_id = ?3 WHERE singleton = 1",
                    params![
                        CONTEXT_PROJECTION_SCHEMA_VERSION,
                        last.seq,
                        &last.event_id
                    ],
                )
                .map_err(ContextProjectionError::from)?;
                if updated != 1 {
                    return Err(ContextProjectionError::MissingCheckpointDuringPage.into());
                }
                transaction.commit().map_err(ContextProjectionError::from)?;
            }
            cursor = last.seq;
            checkpoint = ProjectionCheckpoint {
                schema_version: CONTEXT_PROJECTION_SCHEMA_VERSION,
                through_seq: last.seq,
                through_event_id: Some(last.event_id),
            };
        }
        Ok(checkpoint)
    }

    fn capture_snapshot_locked(
        &self,
        session_id: &str,
        task_id: Option<&str>,
    ) -> Result<DerivedContextSnapshot, ContextProjectionError> {
        validate_query_scope(session_id, task_id)?;
        let connection = self.connection()?;
        let checkpoint = read_checkpoint(&connection)?.ok_or_else(|| {
            ContextProjectionError::CorruptProjectionRow {
                seq: 0,
                reason: "singleton checkpoint is missing".into(),
            }
        })?;
        let mut statement = connection.prepare(
            r#"
            SELECT
                n.session_id,
                n.task_id,
                n.node_id,
                n.event_seq,
                n.event_id,
                n.node_json,
                EXISTS (
                    SELECT 1
                    FROM supersession_edges AS edge
                    WHERE edge.session_id = n.session_id
                      AND edge.task_key = COALESCE(n.task_id, '')
                      AND edge.superseded_node_id = n.node_id
                )
            FROM projected_nodes AS n
            WHERE n.session_id = ?1
              AND (
                  n.task_id IS NULL
                  OR (?2 IS NOT NULL AND n.task_id = ?2)
              )
            ORDER BY n.event_seq ASC, n.node_id ASC
            LIMIT 10001
            "#,
        )?;
        let mapped = statement.query_map(params![session_id, task_id], |row| {
            Ok(RawProjectedRow {
                session_id: row.get(0)?,
                task_id: row.get(1)?,
                node_id: row.get(2)?,
                event_seq: row.get(3)?,
                event_id: row.get(4)?,
                node_json: row.get(5)?,
                superseded: row.get(6)?,
            })
        })?;
        let mut raw_rows = Vec::new();
        for row in mapped {
            raw_rows.push(row?);
            CandidateCount::new(raw_rows.len())?;
        }
        let scanned_rows = raw_rows.len();
        let candidates = raw_rows
            .into_iter()
            .map(projected_row)
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .filter_map(|row| (!row.superseded).then_some(row.node))
            .collect();

        Ok(DerivedContextSnapshot {
            checkpoint,
            scanned_rows,
            candidates,
        })
    }

    fn capture_active_snapshot_locked(
        &self,
        session_id: &str,
        task_id: Option<&str>,
        evaluated_at: DateTime<Utc>,
        budget: &mut RetrievalWorkBudget,
    ) -> Result<DerivedContextSnapshot, ContextProjectionError> {
        validate_query_scope(session_id, task_id)?;
        let connection = self.connection()?;
        let checkpoint = read_checkpoint(&connection)?.ok_or_else(|| {
            ContextProjectionError::CorruptProjectionRow {
                seq: 0,
                reason: "singleton checkpoint is missing".into(),
            }
        })?;
        let evaluated_at_millis = evaluated_at.timestamp_millis();
        let evaluated_at_submillis_nanos =
            i64::from(evaluated_at.timestamp_subsec_nanos() % 1_000_000);
        let mut statement = connection.prepare(
            r#"
            SELECT
                n.session_id,
                n.task_id,
                n.node_id,
                n.event_seq,
                n.event_id,
                n.node_json,
                0
            FROM projected_nodes AS n
            WHERE n.session_id = ?1
              AND (
                  n.task_id IS NULL
                  OR (?2 IS NOT NULL AND n.task_id = ?2)
              )
              AND n.epistemic_status != 'disputed'
              AND (
                  n.valid_from_millis IS NULL
                  OR n.valid_from_millis < ?3
                  OR (
                      n.valid_from_millis = ?3
                      AND n.valid_from_submillis_nanos <= ?4
                  )
              )
              AND (
                  n.valid_until_millis IS NULL
                  OR n.valid_until_millis > ?3
                  OR (
                      n.valid_until_millis = ?3
                      AND n.valid_until_submillis_nanos > ?4
                  )
              )
              AND NOT EXISTS (
                  SELECT 1
                  FROM supersession_edges AS edge
                  WHERE edge.session_id = n.session_id
                    AND edge.task_key = COALESCE(n.task_id, '')
                    AND edge.superseded_node_id = n.node_id
              )
            ORDER BY n.node_id ASC
            LIMIT 10001
            "#,
        )?;
        let mapped = statement.query_map(
            params![
                session_id,
                task_id,
                evaluated_at_millis,
                evaluated_at_submillis_nanos
            ],
            |row| {
                Ok(RawProjectedRow {
                    session_id: row.get(0)?,
                    task_id: row.get(1)?,
                    node_id: row.get(2)?,
                    event_seq: row.get(3)?,
                    event_id: row.get(4)?,
                    node_json: row.get(5)?,
                    superseded: row.get(6)?,
                })
            },
        )?;
        let mut candidates = Vec::new();
        for raw in mapped {
            let raw = raw?;
            CandidateCount::new(candidates.len().saturating_add(1))?;
            budget.charge_candidate_bytes(raw.node_json.len())?;
            let projected = projected_row(raw)?;
            if projected.superseded || !projected.node.is_valid_at(evaluated_at) {
                return Err(ContextProjectionError::CorruptProjectionRow {
                    seq: checkpoint.through_seq,
                    reason: "active lifecycle columns disagree with serialized node".into(),
                });
            }
            candidates.push(projected.node);
        }
        let scanned_rows = candidates.len();
        Ok(DerivedContextSnapshot {
            checkpoint,
            scanned_rows,
            candidates,
        })
    }

    fn verified_snapshot_attempt(
        &self,
        canonical: &CanonicalAdmissionView,
        expected_checkpoint: &ProjectionCheckpoint,
        session_id: &str,
        task_id: Option<&str>,
    ) -> Result<Option<DerivedContextSnapshot>, ContextProjectionError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let snapshot = verified_snapshot_from_cache(
            &transaction,
            canonical,
            expected_checkpoint,
            session_id,
            task_id,
        )?;
        transaction.commit()?;
        Ok(snapshot)
    }

    #[cfg(test)]
    fn run_post_rebuild_snapshot_hook(&self) -> Result<(), ContextProjectionError> {
        let hook = self
            .post_rebuild_snapshot_hook
            .lock()
            .map_err(|_| ContextProjectionError::Poisoned)?
            .clone();
        if let Some(hook) = hook {
            hook(self.database_path());
        }
        Ok(())
    }

    #[cfg(test)]
    fn run_post_active_snapshot_hook(&self) -> Result<(), ContextProjectionError> {
        let hook = self
            .post_active_snapshot_hook
            .lock()
            .map_err(|_| ContextProjectionError::Poisoned)?
            .take();
        if let Some(hook) = hook {
            hook(self.database_path());
        }
        Ok(())
    }

    fn connection(&self) -> Result<MutexGuard<'_, Connection>, ContextProjectionError> {
        self.connection
            .lock()
            .map_err(|_| ContextProjectionError::Poisoned)
    }

    fn verification_state(
        &self,
    ) -> Result<MutexGuard<'_, ProjectionVerificationState>, ContextProjectionError> {
        self.verification
            .lock()
            .map_err(|_| ContextProjectionError::Poisoned)
    }

    fn sync_gate(&self) -> Result<MutexGuard<'_, ()>, ContextProjectionError> {
        self.sync_gate
            .lock()
            .map_err(|_| ContextProjectionError::Poisoned)
    }
}

#[derive(Debug, Clone)]
struct ProjectedRow {
    node: ContextNode,
    superseded: bool,
}

struct RawProjectedRow {
    session_id: String,
    task_id: Option<String>,
    node_id: String,
    event_seq: i64,
    event_id: String,
    node_json: String,
    superseded: bool,
}

struct NodeScope<'a> {
    session_id: &'a str,
    task_id: Option<&'a str>,
}

struct ResolvedNode {
    causation_id: String,
}

struct ValidatedRecordedNode {
    node: ContextNode,
    session_id: String,
    task_id: Option<String>,
}

enum ApplyContextEventError {
    Public(ContextProjectionError),
    RepairableCacheDivergence,
}

impl From<ContextProjectionError> for ApplyContextEventError {
    fn from(error: ContextProjectionError) -> Self {
        Self::Public(error)
    }
}

enum CatchUpError {
    Public(ContextProjectionError),
    RepairableCacheDivergence,
}

impl From<ContextProjectionError> for CatchUpError {
    fn from(error: ContextProjectionError) -> Self {
        Self::Public(error)
    }
}

impl From<ApplyContextEventError> for CatchUpError {
    fn from(error: ApplyContextEventError) -> Self {
        match error {
            ApplyContextEventError::Public(error) => Self::Public(error),
            ApplyContextEventError::RepairableCacheDivergence => Self::RepairableCacheDivergence,
        }
    }
}

enum SynchronizeThroughError {
    Public(ContextProjectionError),
    PersistentCacheDivergence { checkpoint: i64 },
}

impl From<ContextProjectionError> for SynchronizeThroughError {
    fn from(error: ContextProjectionError) -> Self {
        Self::Public(error)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CanonicalAdmissionRow {
    task_id: Option<String>,
    event_seq: i64,
    event_id: String,
    node_digest: [u8; 32],
    outgoing_edge_digest: [u8; 32],
    outgoing_edge_count: usize,
}

#[derive(Default)]
struct CanonicalAdmissionView {
    rows: HashMap<String, CanonicalAdmissionRow>,
    incoming_edges: HashMap<String, EdgeAccumulator>,
}

struct EdgeAccumulator {
    hasher: Sha256,
    count: usize,
}

impl EdgeAccumulator {
    fn update(
        &mut self,
        superseding_node_id: &str,
        task_key: &str,
        event_seq: i64,
        superseded_node_id: &str,
    ) {
        update_edge_digest(
            &mut self.hasher,
            superseding_node_id,
            task_key,
            event_seq,
            superseded_node_id,
        );
        self.count += 1;
    }

    fn shape(&self) -> ([u8; 32], usize) {
        (self.hasher.clone().finalize().into(), self.count)
    }
}

impl Default for EdgeAccumulator {
    fn default() -> Self {
        Self {
            hasher: Sha256::new(),
            count: 0,
        }
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

fn ensure_schema(connection: &mut Connection) -> Result<(), ContextProjectionError> {
    if !schema_is_current(connection)? {
        reset_schema(connection)?;
    }
    Ok(())
}

fn schema_is_current(connection: &Connection) -> Result<bool, ContextProjectionError> {
    let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version != CONTEXT_PROJECTION_SCHEMA_VERSION {
        return Ok(false);
    }
    let table_count: i64 = connection.query_row(
        r#"
        SELECT COUNT(*)
        FROM sqlite_master
        WHERE type = 'table'
          AND name IN (
              'projection_checkpoint',
              'projected_nodes',
              'supersession_edges'
          )
        "#,
        [],
        |row| row.get(0),
    )?;
    if table_count != 3 {
        return Ok(false);
    }
    let statements = [
        "SELECT schema_version, through_seq, through_event_id FROM projection_checkpoint LIMIT 0",
        "SELECT session_id, task_id, node_id, event_seq, event_id, node_json, epistemic_status, valid_from_millis, valid_from_submillis_nanos, valid_until_millis, valid_until_submillis_nanos FROM projected_nodes LIMIT 0",
        "SELECT session_id, task_key, superseding_node_id, superseded_node_id, event_seq FROM supersession_edges LIMIT 0",
    ];
    Ok(statements
        .into_iter()
        .all(|statement| connection.prepare(statement).is_ok()))
}

fn reset_schema(connection: &mut Connection) -> Result<(), ContextProjectionError> {
    let transaction = connection.transaction()?;
    transaction.execute_batch(
        r#"
        DROP TABLE IF EXISTS supersession_edges;
        DROP TABLE IF EXISTS projected_nodes;
        DROP TABLE IF EXISTS projection_checkpoint;
        "#,
    )?;
    transaction.execute_batch(SCHEMA_V3)?;
    transaction.pragma_update(None, "user_version", CONTEXT_PROJECTION_SCHEMA_VERSION)?;
    transaction.commit()?;
    Ok(())
}

fn read_checkpoint(
    connection: &Connection,
) -> Result<Option<ProjectionCheckpoint>, ContextProjectionError> {
    connection
        .query_row(
            "SELECT schema_version, through_seq, through_event_id FROM projection_checkpoint WHERE singleton = 1",
            [],
            |row| {
                Ok(ProjectionCheckpoint {
                    schema_version: row.get(0)?,
                    through_seq: row.get(1)?,
                    through_event_id: row.get(2)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
}

fn checkpoint_anchor_matches(
    checkpoint: &ProjectionCheckpoint,
    event_store: &EventStore,
) -> Result<bool, ContextProjectionError> {
    if checkpoint.through_seq == 0 {
        return Ok(checkpoint.through_event_id.is_none());
    }
    let Some(expected_id) = checkpoint.through_event_id.as_deref() else {
        return Ok(false);
    };
    let by_seq = event_store.get_by_seq(checkpoint.through_seq)?;
    let by_id = event_store.get_by_event_id(expected_id)?;
    Ok(by_seq
        .as_ref()
        .is_some_and(|event| event.event_id == expected_id && event.seq == checkpoint.through_seq)
        && by_id.as_ref().is_some_and(|event| {
            event.event_id == expected_id && event.seq == checkpoint.through_seq
        }))
}

fn ensure_delta_preflight_tables(connection: &Connection) -> Result<(), ContextProjectionError> {
    // Dependency sets can grow with the bounded delta, so keep their derived
    // spill storage off the Rust heap regardless of SQLite's build default.
    connection.pragma_update(None, "temp_store", "FILE")?;
    connection.execute_batch(
        r#"
        CREATE TEMP TABLE IF NOT EXISTS context_delta_targets (
            session_id TEXT NOT NULL,
            node_id TEXT NOT NULL,
            PRIMARY KEY (session_id, node_id)
        ) WITHOUT ROWID;
        CREATE TEMP TABLE IF NOT EXISTS context_delta_events (
            event_seq INTEGER PRIMARY KEY,
            event_id TEXT NOT NULL UNIQUE
        );
        CREATE TEMP TABLE IF NOT EXISTS context_delta_canonical_rows (
            session_id TEXT NOT NULL,
            node_id TEXT NOT NULL,
            task_id TEXT,
            event_seq INTEGER NOT NULL,
            event_id TEXT NOT NULL,
            node_json TEXT NOT NULL,
            PRIMARY KEY (session_id, node_id)
        ) WITHOUT ROWID;
        CREATE TEMP TABLE IF NOT EXISTS context_delta_canonical_edges (
            session_id TEXT NOT NULL,
            task_key TEXT NOT NULL,
            superseding_node_id TEXT NOT NULL,
            superseded_node_id TEXT NOT NULL,
            event_seq INTEGER NOT NULL
        );
        CREATE TEMP TABLE IF NOT EXISTS context_delta_working_rows (
            session_id TEXT NOT NULL,
            node_id TEXT NOT NULL,
            task_id TEXT,
            event_seq INTEGER NOT NULL,
            event_id TEXT NOT NULL,
            node_json TEXT NOT NULL,
            PRIMARY KEY (session_id, node_id)
        ) WITHOUT ROWID;
        "#,
    )?;
    Ok(())
}

fn clear_delta_preflight_tables(connection: &Connection) -> Result<(), ContextProjectionError> {
    connection.execute_batch(
        r#"
        DELETE FROM context_delta_targets;
        DELETE FROM context_delta_events;
        DELETE FROM context_delta_canonical_rows;
        DELETE FROM context_delta_canonical_edges;
        DELETE FROM context_delta_working_rows;
        "#,
    )?;
    Ok(())
}

fn visit_event_range(
    event_store: &EventStore,
    after_seq: i64,
    through_seq: i64,
    mut visit: impl FnMut(&EventRecord) -> Result<(), ContextProjectionError>,
) -> Result<(), ContextProjectionError> {
    visit_filtered_event_range(event_store, after_seq, through_seq, None, &mut visit)
}

fn visit_filtered_event_range(
    event_store: &EventStore,
    after_seq: i64,
    through_seq: i64,
    session_id: Option<&str>,
    visit: &mut impl FnMut(&EventRecord) -> Result<(), ContextProjectionError>,
) -> Result<(), ContextProjectionError> {
    let mut cursor = after_seq;
    while cursor < through_seq {
        let page = event_store.list_through(
            &EventQuery {
                after_seq: Some(cursor),
                limit: Some(SYNC_PAGE_SIZE),
                session_id: session_id.map(str::to_owned),
                ..EventQuery::default()
            },
            through_seq,
        )?;
        let Some(last) = page.last() else {
            if session_id.is_some() {
                break;
            }
            return Err(ContextProjectionError::HighWaterUnreachable {
                cursor,
                high_water: through_seq,
            });
        };
        let mut previous = cursor;
        for event in &page {
            if event.seq <= previous || event.seq > through_seq {
                return Err(ContextProjectionError::NonMonotonicPage {
                    after: previous,
                    found: event.seq,
                });
            }
            visit(event)?;
            previous = event.seq;
        }
        cursor = last.seq;
    }
    Ok(())
}

fn collect_delta_dependencies(
    transaction: &Transaction<'_>,
    event_store: &EventStore,
    after_seq: i64,
    through_seq: i64,
) -> Result<(), ContextProjectionError> {
    visit_event_range(event_store, after_seq, through_seq, |event| {
        if event.kind != event_kind::CONTEXT_NODE_RECORDED {
            return Ok(());
        }
        let validated = decode_recorded_node(event_store, event)?;
        transaction.execute(
            "INSERT INTO context_delta_events (event_seq, event_id) VALUES (?1, ?2)",
            params![event.seq, &event.event_id],
        )?;
        transaction.execute(
            "INSERT OR IGNORE INTO context_delta_targets (session_id, node_id) VALUES (?1, ?2)",
            params![&validated.session_id, &validated.node.id],
        )?;
        for superseded_id in &validated.node.supersedes {
            transaction.execute(
                "INSERT OR IGNORE INTO context_delta_targets (session_id, node_id) VALUES (?1, ?2)",
                params![&validated.session_id, superseded_id],
            )?;
        }
        Ok(())
    })
}

fn delta_target_contains(
    connection: &Connection,
    session_id: &str,
    node_id: &str,
) -> Result<bool, ContextProjectionError> {
    connection
        .query_row(
            r#"
            SELECT EXISTS (
                SELECT 1
                FROM context_delta_targets
                WHERE session_id = ?1 AND node_id = ?2
            )
            "#,
            params![session_id, node_id],
            |row| row.get(0),
        )
        .map_err(Into::into)
}

fn seed_canonical_delta_dependencies(
    transaction: &Transaction<'_>,
    event_store: &EventStore,
    through_seq: i64,
) -> Result<(), ContextProjectionError> {
    let mut after_session: Option<String> = None;
    loop {
        let sessions = {
            let mut statement = transaction.prepare(
                r#"
                SELECT DISTINCT session_id
                FROM context_delta_targets
                WHERE (?1 IS NULL OR session_id > ?1)
                ORDER BY session_id ASC
                LIMIT 128
                "#,
            )?;
            statement
                .query_map(params![after_session.as_deref()], |row| {
                    row.get::<_, String>(0)
                })?
                .collect::<Result<Vec<_>, _>>()?
        };
        if sessions.is_empty() {
            break;
        }
        for session_id in &sessions {
            visit_filtered_event_range(
                event_store,
                0,
                through_seq,
                Some(session_id),
                &mut |event| seed_canonical_delta_event(transaction, event_store, event),
            )?;
        }
        after_session = sessions.last().cloned();
    }
    Ok(())
}

fn seed_canonical_delta_event(
    transaction: &Transaction<'_>,
    event_store: &EventStore,
    event: &EventRecord,
) -> Result<(), ContextProjectionError> {
    if event.kind != event_kind::CONTEXT_NODE_RECORDED {
        return Ok(());
    }
    let validated = decode_recorded_node(event_store, event)?;
    let row_is_target =
        delta_target_contains(transaction, &validated.session_id, &validated.node.id)?;
    if row_is_target {
        if let Some((event_id, event_seq)) = transaction
                .query_row(
                    "SELECT event_id, event_seq FROM context_delta_canonical_rows WHERE session_id = ?1 AND node_id = ?2",
                    params![&validated.session_id, &validated.node.id],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
                )
                .optional()?
            {
                return Err(ContextProjectionError::DuplicateNodeIdentity {
                    session_id: validated.session_id,
                    node_id: validated.node.id,
                    event_id,
                    seq: event_seq,
                });
            }
        transaction.execute(
            r#"
                INSERT INTO context_delta_canonical_rows (
                    session_id, node_id, task_id, event_seq, event_id, node_json
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                "#,
            params![
                &validated.session_id,
                &validated.node.id,
                &validated.task_id,
                event.seq,
                &event.event_id,
                serde_json::to_string(&validated.node).map_err(|error| {
                    ContextProjectionError::InvalidNode {
                        node_id: validated.node.id.clone(),
                        reason: error.to_string(),
                    }
                })?,
            ],
        )?;
    }

    for superseded_id in &validated.node.supersedes {
        if row_is_target
            || delta_target_contains(transaction, &validated.session_id, superseded_id)?
        {
            transaction.execute(
                r#"
                    INSERT INTO context_delta_canonical_edges (
                        session_id, task_key, superseding_node_id,
                        superseded_node_id, event_seq
                    ) VALUES (?1, ?2, ?3, ?4, ?5)
                    "#,
                params![
                    &validated.session_id,
                    task_key(validated.task_id.as_deref()),
                    &validated.node.id,
                    superseded_id,
                    event.seq,
                ],
            )?;
        }
    }
    Ok(())
}

fn delta_dependency_cache_matches(
    transaction: &Transaction<'_>,
) -> Result<bool, ContextProjectionError> {
    let row_mismatch: bool = transaction.query_row(
        r#"
        SELECT EXISTS (
            SELECT 1
            FROM context_delta_targets AS target
            LEFT JOIN context_delta_canonical_rows AS canonical
              ON canonical.session_id = target.session_id
             AND canonical.node_id = target.node_id
            LEFT JOIN projected_nodes AS cached
              ON cached.session_id = target.session_id
             AND cached.node_id = target.node_id
            WHERE (canonical.node_id IS NULL) <> (cached.node_id IS NULL)
               OR (
                    canonical.node_id IS NOT NULL
                AND (
                       canonical.task_id IS NOT cached.task_id
                    OR canonical.event_seq != cached.event_seq
                    OR canonical.event_id != cached.event_id
                    OR canonical.node_json != cached.node_json
                )
               )
        )
        "#,
        [],
        |row| row.get(0),
    )?;
    let occupied_delta_event: bool = transaction.query_row(
        r#"
        SELECT EXISTS (
            SELECT 1
            FROM projected_nodes AS cached
            JOIN context_delta_events AS delta
              ON cached.event_seq = delta.event_seq
              OR cached.event_id = delta.event_id
        )
        "#,
        [],
        |row| row.get(0),
    )?;
    let missing_edge: bool = transaction.query_row(
        r#"
        SELECT EXISTS (
            SELECT 1
            FROM context_delta_canonical_edges AS canonical
            LEFT JOIN supersession_edges AS cached
              ON cached.session_id = canonical.session_id
             AND cached.task_key = canonical.task_key
             AND cached.superseding_node_id = canonical.superseding_node_id
             AND cached.superseded_node_id = canonical.superseded_node_id
             AND cached.event_seq = canonical.event_seq
            WHERE cached.session_id IS NULL
        )
        "#,
        [],
        |row| row.get(0),
    )?;
    let extra_edge: bool = transaction.query_row(
        r#"
        SELECT EXISTS (
            SELECT 1
            FROM supersession_edges AS cached
            JOIN context_delta_targets AS target
              ON target.session_id = cached.session_id
             AND (
                    target.node_id = cached.superseding_node_id
                 OR target.node_id = cached.superseded_node_id
             )
            LEFT JOIN context_delta_canonical_edges AS canonical
              ON canonical.session_id = cached.session_id
             AND canonical.task_key = cached.task_key
             AND canonical.superseding_node_id = cached.superseding_node_id
             AND canonical.superseded_node_id = cached.superseded_node_id
             AND canonical.event_seq = cached.event_seq
            WHERE canonical.session_id IS NULL
        )
        "#,
        [],
        |row| row.get(0),
    )?;
    Ok(!(row_mismatch || occupied_delta_event || missing_edge || extra_edge))
}

fn validate_delta_against_canonical_dependencies(
    transaction: &Transaction<'_>,
    event_store: &EventStore,
    after_seq: i64,
    through_seq: i64,
) -> Result<(), ContextProjectionError> {
    transaction.execute(
        r#"
        INSERT INTO context_delta_working_rows (
            session_id, node_id, task_id, event_seq, event_id, node_json
        )
        SELECT session_id, node_id, task_id, event_seq, event_id, node_json
        FROM context_delta_canonical_rows
        "#,
        [],
    )?;
    visit_event_range(event_store, after_seq, through_seq, |event| {
        if event.kind != event_kind::CONTEXT_NODE_RECORDED {
            return Ok(());
        }
        let validated = decode_recorded_node(event_store, event)?;
        if let Some((event_id, event_seq)) = transaction
            .query_row(
                "SELECT event_id, event_seq FROM context_delta_working_rows WHERE session_id = ?1 AND node_id = ?2",
                params![&validated.session_id, &validated.node.id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
            )
            .optional()?
        {
            return Err(ContextProjectionError::DuplicateNodeIdentity {
                session_id: validated.session_id,
                node_id: validated.node.id,
                event_id,
                seq: event_seq,
            });
        }
        for superseded_id in &validated.node.supersedes {
            match transaction
                .query_row(
                    "SELECT task_id FROM context_delta_working_rows WHERE session_id = ?1 AND node_id = ?2",
                    params![&validated.session_id, superseded_id],
                    |row| row.get::<_, Option<String>>(0),
                )
                .optional()?
            {
                None => {
                    return Err(ContextProjectionError::MissingSupersededNode {
                        node_id: validated.node.id,
                        superseded_id: superseded_id.clone(),
                    });
                }
                Some(task_id) if task_id != validated.task_id => {
                    return Err(ContextProjectionError::SupersessionScopeMismatch {
                        node_id: validated.node.id,
                        superseded_id: superseded_id.clone(),
                    });
                }
                Some(_) => {}
            }
        }
        transaction.execute(
            r#"
            INSERT INTO context_delta_working_rows (
                session_id, node_id, task_id, event_seq, event_id, node_json
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
            "#,
            params![
                &validated.session_id,
                &validated.node.id,
                &validated.task_id,
                event.seq,
                &event.event_id,
                serde_json::to_string(&validated.node).map_err(|error| {
                    ContextProjectionError::InvalidNode {
                        node_id: validated.node.id.clone(),
                        reason: error.to_string(),
                    }
                })?,
            ],
        )?;
        Ok(())
    })
}

fn canonical_session_admission_view(
    event_store: &EventStore,
    through_seq: i64,
    session_id: &str,
    targets: &HashSet<String>,
) -> Result<CanonicalAdmissionView, ContextProjectionError> {
    let mut view = CanonicalAdmissionView::default();
    let mut cursor = 0_i64;
    loop {
        let page = event_store.list_through(
            &EventQuery {
                after_seq: Some(cursor),
                limit: Some(1_000),
                session_id: Some(session_id.to_owned()),
                task_id: None,
            },
            through_seq,
        )?;
        if page.is_empty() {
            break;
        }
        for event in &page {
            if event.seq <= cursor || event.seq > through_seq {
                return Err(ContextProjectionError::NonMonotonicPage {
                    after: cursor,
                    found: event.seq,
                });
            }
            cursor = event.seq;
            if event.kind != event_kind::CONTEXT_NODE_RECORDED {
                continue;
            }
            let validated = decode_recorded_node(event_store, event)?;
            let mut targeted_supersedes = validated
                .node
                .supersedes
                .iter()
                .filter(|superseded_id| targets.contains(*superseded_id))
                .collect::<Vec<_>>();
            targeted_supersedes.sort_unstable();
            for superseded_id in targeted_supersedes {
                view.incoming_edges
                    .entry(superseded_id.clone())
                    .or_default()
                    .update(
                        &validated.node.id,
                        task_key(validated.task_id.as_deref()),
                        event.seq,
                        superseded_id,
                    );
            }
            if !targets.contains(&validated.node.id) {
                continue;
            }
            let row = canonical_cache_row(&validated, event)?;
            if let Some(existing) = view.rows.insert(validated.node.id.clone(), row) {
                return Err(ContextProjectionError::DuplicateNodeIdentity {
                    session_id: session_id.to_owned(),
                    node_id: validated.node.id,
                    event_id: existing.event_id,
                    seq: existing.event_seq,
                });
            }
        }
    }
    Ok(view)
}

fn canonical_scope_snapshot_view(
    event_store: &EventStore,
    through_seq: i64,
    session_id: &str,
    task_id: Option<&str>,
) -> Result<CanonicalAdmissionView, ContextProjectionError> {
    let mut view = CanonicalAdmissionView::default();
    let mut cursor = 0_i64;
    let mut selected_count = 0_usize;
    loop {
        let page = event_store.list_through(
            &EventQuery {
                after_seq: Some(cursor),
                limit: Some(1_000),
                session_id: Some(session_id.to_owned()),
                task_id: None,
            },
            through_seq,
        )?;
        if page.is_empty() {
            break;
        }
        for event in &page {
            if event.seq <= cursor || event.seq > through_seq {
                return Err(ContextProjectionError::NonMonotonicPage {
                    after: cursor,
                    found: event.seq,
                });
            }
            cursor = event.seq;
            if event.kind != event_kind::CONTEXT_NODE_RECORDED {
                continue;
            }
            let envelope_selected = event.task_id.is_none()
                || task_id.is_some_and(|requested| event.task_id.as_deref() == Some(requested));
            if !envelope_selected {
                continue;
            }
            let validated = decode_recorded_node(event_store, event)?;

            selected_count += 1;
            CandidateCount::new(selected_count)?;
            for superseded_id in &validated.node.supersedes {
                view.incoming_edges
                    .entry(superseded_id.clone())
                    .or_default()
                    .update(
                        &validated.node.id,
                        task_key(validated.task_id.as_deref()),
                        event.seq,
                        superseded_id,
                    );
            }
            let row = canonical_cache_row(&validated, event)?;
            if let Some(existing) = view.rows.insert(validated.node.id.clone(), row) {
                return Err(ContextProjectionError::DuplicateNodeIdentity {
                    session_id: session_id.to_owned(),
                    node_id: validated.node.id,
                    event_id: existing.event_id,
                    seq: existing.event_seq,
                });
            }
        }
    }
    Ok(view)
}

fn canonical_cache_row(
    validated: &ValidatedRecordedNode,
    event: &EventRecord,
) -> Result<CanonicalAdmissionRow, ContextProjectionError> {
    let node_json = serde_json::to_vec(&validated.node).map_err(|error| {
        ContextProjectionError::InvalidNode {
            node_id: validated.node.id.clone(),
            reason: error.to_string(),
        }
    })?;
    Ok(CanonicalAdmissionRow {
        task_id: validated.task_id.clone(),
        event_seq: event.seq,
        event_id: event.event_id.clone(),
        node_digest: digest_bytes(&node_json),
        outgoing_edge_digest: canonical_edge_digest(
            &validated.node.id,
            task_key(validated.task_id.as_deref()),
            event.seq,
            &validated.node.supersedes,
        ),
        outgoing_edge_count: validated.node.supersedes.len(),
    })
}

fn verified_snapshot_from_cache(
    transaction: &Transaction<'_>,
    canonical: &CanonicalAdmissionView,
    expected_checkpoint: &ProjectionCheckpoint,
    session_id: &str,
    task_id: Option<&str>,
) -> Result<Option<DerivedContextSnapshot>, ContextProjectionError> {
    let Some(checkpoint) = read_checkpoint(transaction)? else {
        return Ok(None);
    };
    if checkpoint != *expected_checkpoint {
        return Ok(None);
    }
    let Some(scoped_cache_count) =
        cached_scope_row_count_with_sentinel(transaction, session_id, task_id)?
    else {
        return Ok(None);
    };
    if scoped_cache_count != canonical.rows.len() {
        return Ok(None);
    }
    let Some(cached_edges) = cached_scope_edge_view(transaction, canonical, session_id, task_id)?
    else {
        return Ok(None);
    };

    let mut candidates = Vec::new();
    let mut cached_row_count = 0_usize;
    {
        let mut statement = transaction.prepare(
            r#"
            SELECT
                n.session_id,
                n.task_id,
                n.node_id,
                n.event_seq,
                n.event_id,
                n.node_json,
                EXISTS (
                    SELECT 1
                    FROM supersession_edges AS edge
                    WHERE edge.session_id = n.session_id
                      AND edge.task_key = COALESCE(n.task_id, '')
                      AND edge.superseded_node_id = n.node_id
                )
            FROM projected_nodes AS n
            WHERE n.session_id = ?1
              AND (
                  n.task_id IS NULL
                  OR (?2 IS NOT NULL AND n.task_id = ?2)
              )
            ORDER BY n.event_seq ASC, n.node_id ASC
            LIMIT 10001
            "#,
        )?;
        let mapped = statement.query_map(params![session_id, task_id], |row| {
            Ok(RawProjectedRow {
                session_id: row.get(0)?,
                task_id: row.get(1)?,
                node_id: row.get(2)?,
                event_seq: row.get(3)?,
                event_id: row.get(4)?,
                node_json: row.get(5)?,
                superseded: row.get(6)?,
            })
        })?;
        for raw in mapped {
            let raw = raw?;
            cached_row_count += 1;
            let Some(expected) = canonical.rows.get(&raw.node_id) else {
                return Ok(None);
            };
            if raw.task_id != expected.task_id
                || raw.event_seq != expected.event_seq
                || raw.event_id != expected.event_id
                || digest_bytes(raw.node_json.as_bytes()) != expected.node_digest
            {
                return Ok(None);
            }

            let outgoing = cached_edges
                .outgoing
                .get(&raw.node_id)
                .map_or_else(empty_edge_shape, EdgeAccumulator::shape);
            if outgoing != (expected.outgoing_edge_digest, expected.outgoing_edge_count) {
                return Ok(None);
            }
            let incoming = cached_edges
                .incoming
                .get(&raw.node_id)
                .map_or_else(empty_edge_shape, EdgeAccumulator::shape);
            let expected_incoming = canonical
                .incoming_edges
                .get(&raw.node_id)
                .map_or_else(empty_edge_shape, EdgeAccumulator::shape);
            if incoming != expected_incoming {
                return Ok(None);
            }

            let projected = projected_row(raw)?;
            if !projected.superseded {
                candidates.push(projected.node);
            }
        }
    }
    if cached_row_count != canonical.rows.len() {
        return Ok(None);
    }

    Ok(Some(DerivedContextSnapshot {
        checkpoint,
        scanned_rows: cached_row_count,
        candidates,
    }))
}

fn cached_scope_row_count_with_sentinel(
    transaction: &Transaction<'_>,
    session_id: &str,
    task_id: Option<&str>,
) -> Result<Option<usize>, ContextProjectionError> {
    let mut statement = transaction.prepare(
        r#"
        SELECT 1
        FROM projected_nodes
        WHERE session_id = ?1
          AND (
              task_id IS NULL
              OR (?2 IS NOT NULL AND task_id = ?2)
          )
        ORDER BY event_seq ASC, node_id ASC
        LIMIT 10001
        "#,
    )?;
    let rows = statement.query_map(params![session_id, task_id], |_| Ok(()))?;
    let mut count = 0_usize;
    for row in rows {
        row?;
        count += 1;
        if count > MAX_CANDIDATE_COUNT {
            return Ok(None);
        }
    }
    Ok(Some(count))
}

#[derive(Default)]
struct CachedScopeEdgeView {
    outgoing: HashMap<String, EdgeAccumulator>,
    incoming: HashMap<String, EdgeAccumulator>,
}

fn cached_scope_edge_view(
    transaction: &Transaction<'_>,
    canonical: &CanonicalAdmissionView,
    session_id: &str,
    task_id: Option<&str>,
) -> Result<Option<CachedScopeEdgeView>, ContextProjectionError> {
    let expected_edge_count = canonical
        .rows
        .values()
        .map(|row| row.outgoing_edge_count)
        .sum::<usize>();
    let mut statement = transaction.prepare(
        r#"
        SELECT superseding_node_id, task_key, event_seq, superseded_node_id
        FROM supersession_edges
        WHERE session_id = ?1
        ORDER BY event_seq ASC, superseding_node_id ASC,
                 task_key ASC, superseded_node_id ASC
        "#,
    )?;
    let rows = statement.query_map(params![session_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, String>(3)?,
        ))
    })?;
    let mut result = CachedScopeEdgeView::default();
    let mut endpoints = HashSet::new();
    let mut relevant_edge_count = 0_usize;
    for row in rows {
        let (superseding_node_id, edge_task_key, event_seq, superseded_node_id) = row?;
        let scope_selected =
            edge_task_key.is_empty() || task_id.is_some_and(|requested| requested == edge_task_key);
        let touches_selected_row = canonical.rows.contains_key(&superseding_node_id)
            || canonical.rows.contains_key(&superseded_node_id);
        if !scope_selected && !touches_selected_row {
            continue;
        }

        relevant_edge_count += 1;
        if relevant_edge_count > expected_edge_count {
            return Ok(None);
        }
        endpoints.insert(superseding_node_id.clone());
        endpoints.insert(superseded_node_id.clone());
        if endpoints.len() > MAX_CANDIDATE_COUNT
            || !canonical.rows.contains_key(&superseding_node_id)
            || !canonical.rows.contains_key(&superseded_node_id)
        {
            return Ok(None);
        }
        result
            .outgoing
            .entry(superseding_node_id.clone())
            .or_default()
            .update(
                &superseding_node_id,
                &edge_task_key,
                event_seq,
                &superseded_node_id,
            );
        result
            .incoming
            .entry(superseded_node_id.clone())
            .or_default()
            .update(
                &superseding_node_id,
                &edge_task_key,
                event_seq,
                &superseded_node_id,
            );
    }
    if relevant_edge_count != expected_edge_count {
        return Ok(None);
    }
    Ok(Some(result))
}

fn relevant_cache_matches(
    connection: &Connection,
    session_id: &str,
    targets: &HashSet<String>,
    canonical: &CanonicalAdmissionView,
) -> Result<bool, ContextProjectionError> {
    for node_id in targets {
        let cached = connection
            .query_row(
                r#"
                SELECT task_id, event_seq, event_id, node_json
                FROM projected_nodes
                WHERE session_id = ?1 AND node_id = ?2
                "#,
                params![session_id, node_id],
                |row| {
                    let node_json = row.get::<_, String>(3)?;
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                        digest_bytes(node_json.as_bytes()),
                    ))
                },
            )
            .optional()?;
        let Some(expected) = canonical.rows.get(node_id) else {
            if cached.is_some() || cache_has_edges_for_node(connection, session_id, node_id)? {
                return Ok(false);
            }
            continue;
        };
        let Some((task_id, event_seq, event_id, node_digest)) = cached else {
            return Ok(false);
        };
        if task_id != expected.task_id
            || event_seq != expected.event_seq
            || event_id != expected.event_id
            || node_digest != expected.node_digest
        {
            return Ok(false);
        }

        let (outgoing_edge_digest, outgoing_edge_count) =
            cached_outgoing_edge_digest(connection, session_id, node_id)?;
        if outgoing_edge_digest != expected.outgoing_edge_digest
            || outgoing_edge_count != expected.outgoing_edge_count
        {
            return Ok(false);
        }

        let incoming_edge_shape = cached_incoming_edge_digest(connection, session_id, node_id)?;
        let expected_incoming_edge_shape = canonical
            .incoming_edges
            .get(node_id)
            .map_or_else(empty_edge_shape, EdgeAccumulator::shape);
        if incoming_edge_shape != expected_incoming_edge_shape {
            return Ok(false);
        }
    }
    Ok(true)
}

fn digest_bytes(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

fn update_edge_digest(
    hasher: &mut Sha256,
    superseding_node_id: &str,
    task_key: &str,
    event_seq: i64,
    superseded_node_id: &str,
) {
    for bytes in [
        superseding_node_id.as_bytes(),
        task_key.as_bytes(),
        superseded_node_id.as_bytes(),
    ] {
        hasher.update(u64::try_from(bytes.len()).unwrap_or(u64::MAX).to_be_bytes());
        hasher.update(bytes);
    }
    hasher.update(event_seq.to_be_bytes());
}

fn canonical_edge_digest(
    superseding_node_id: &str,
    task_key: &str,
    event_seq: i64,
    targets: &[String],
) -> [u8; 32] {
    let mut ordered = targets.iter().map(String::as_str).collect::<Vec<_>>();
    ordered.sort_unstable();
    let mut hasher = Sha256::new();
    for target in ordered {
        update_edge_digest(
            &mut hasher,
            superseding_node_id,
            task_key,
            event_seq,
            target,
        );
    }
    hasher.finalize().into()
}

fn empty_edge_shape() -> ([u8; 32], usize) {
    (Sha256::new().finalize().into(), 0)
}

fn cached_outgoing_edge_digest(
    connection: &Connection,
    session_id: &str,
    node_id: &str,
) -> Result<([u8; 32], usize), ContextProjectionError> {
    let mut statement = connection.prepare(
        r#"
        SELECT superseding_node_id, task_key, event_seq, superseded_node_id
        FROM supersession_edges
        WHERE session_id = ?1 AND superseding_node_id = ?2
        ORDER BY task_key ASC, event_seq ASC, superseded_node_id ASC
        "#,
    )?;
    let rows = statement.query_map(params![session_id, node_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, String>(3)?,
        ))
    })?;
    let mut hasher = Sha256::new();
    let mut count = 0_usize;
    for row in rows {
        let (superseding_node_id, edge_task_key, event_seq, target) = row?;
        update_edge_digest(
            &mut hasher,
            &superseding_node_id,
            &edge_task_key,
            event_seq,
            &target,
        );
        count += 1;
    }
    Ok((hasher.finalize().into(), count))
}

fn cache_has_edges_for_node(
    connection: &Connection,
    session_id: &str,
    node_id: &str,
) -> Result<bool, ContextProjectionError> {
    connection
        .query_row(
            r#"
            SELECT EXISTS (
                SELECT 1
                FROM supersession_edges
                WHERE session_id = ?1
                  AND (
                      superseding_node_id = ?2
                      OR superseded_node_id = ?2
                  )
            )
            "#,
            params![session_id, node_id],
            |row| row.get(0),
        )
        .map_err(Into::into)
}

fn cached_incoming_edge_digest(
    connection: &Connection,
    session_id: &str,
    node_id: &str,
) -> Result<([u8; 32], usize), ContextProjectionError> {
    let mut statement = connection.prepare(
        r#"
        SELECT superseding_node_id, task_key, event_seq, superseded_node_id
        FROM supersession_edges
        WHERE session_id = ?1 AND superseded_node_id = ?2
        ORDER BY event_seq ASC, superseding_node_id ASC, task_key ASC, superseded_node_id ASC
        "#,
    )?;
    let rows = statement.query_map(params![session_id, node_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, String>(3)?,
        ))
    })?;
    let mut hasher = Sha256::new();
    let mut count = 0_usize;
    for row in rows {
        let (superseding_node_id, edge_task_key, event_seq, target) = row?;
        update_edge_digest(
            &mut hasher,
            &superseding_node_id,
            &edge_task_key,
            event_seq,
            &target,
        );
        count += 1;
    }
    Ok((hasher.finalize().into(), count))
}

fn apply_context_event(
    transaction: &Transaction<'_>,
    event_store: &EventStore,
    event: &EventRecord,
) -> Result<(), ApplyContextEventError> {
    let validated = decode_recorded_node(event_store, event)?;
    let scope = NodeScope {
        session_id: &validated.session_id,
        task_id: validated.task_id.as_deref(),
    };
    let preflight_conflict = cache_preflight_conflicts_with_event(transaction, event, &validated)?;
    let validation_conflict = projection_cache_conflicts(transaction, &validated.node, &scope)?;
    if preflight_conflict || validation_conflict {
        return Err(ApplyContextEventError::RepairableCacheDivergence);
    }

    let node_json = serde_json::to_string(&validated.node).map_err(|error| {
        ContextProjectionError::InvalidNode {
            node_id: validated.node.id.clone(),
            reason: error.to_string(),
        }
    })?;
    let valid_from = validated
        .node
        .valid_from
        .as_ref()
        .map(projection_timestamp_parts);
    let valid_until = validated
        .node
        .valid_until
        .as_ref()
        .map(projection_timestamp_parts);
    transaction
        .execute(
            r#"
        INSERT INTO projected_nodes (
            session_id, task_id, node_id, event_seq, event_id, node_json,
            epistemic_status,
            valid_from_millis, valid_from_submillis_nanos,
            valid_until_millis, valid_until_submillis_nanos
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
        "#,
            params![
                scope.session_id,
                scope.task_id,
                &validated.node.id,
                event.seq,
                &event.event_id,
                node_json,
                epistemic_status(validated.node.epistemic),
                valid_from.map(|value| value.0),
                valid_from.map(|value| value.1),
                valid_until.map(|value| value.0),
                valid_until.map(|value| value.1),
            ],
        )
        .map_err(ContextProjectionError::from)?;
    for superseded_id in &validated.node.supersedes {
        transaction
            .execute(
                r#"
            INSERT INTO supersession_edges (
                session_id, task_key, superseding_node_id,
                superseded_node_id, event_seq
            ) VALUES (?1, ?2, ?3, ?4, ?5)
            "#,
                params![
                    scope.session_id,
                    task_key(scope.task_id),
                    &validated.node.id,
                    superseded_id,
                    event.seq,
                ],
            )
            .map_err(ContextProjectionError::from)?;
    }
    Ok(())
}

fn projection_timestamp_parts(value: &DateTime<Utc>) -> (i64, i64) {
    (
        value.timestamp_millis(),
        i64::from(value.timestamp_subsec_nanos() % 1_000_000),
    )
}

fn cache_preflight_conflicts_with_event(
    connection: &Connection,
    event: &EventRecord,
    validated: &ValidatedRecordedNode,
) -> Result<bool, ContextProjectionError> {
    let occupied_event_identity: bool = connection.query_row(
        r#"
        SELECT EXISTS (
            SELECT 1
            FROM projected_nodes
            WHERE event_seq = ?1 OR event_id = ?2
        )
        "#,
        params![event.seq, &event.event_id],
        |row| row.get(0),
    )?;
    if occupied_event_identity {
        return Ok(true);
    }

    cache_has_edges_for_node(connection, &validated.session_id, &validated.node.id)
}

fn decode_recorded_node(
    event_store: &EventStore,
    event: &EventRecord,
) -> Result<ValidatedRecordedNode, ContextProjectionError> {
    if event.actor != EventActor::System {
        return Err(ContextProjectionError::InvalidActor {
            seq: event.seq,
            found: event.actor,
        });
    }
    if event.span_id.is_some() {
        return Err(ContextProjectionError::UnexpectedSpan { seq: event.seq });
    }

    let serialized_payload = serde_json::to_vec(&event.payload).map_err(|error| {
        ContextProjectionError::MalformedPayload {
            seq: event.seq,
            reason: error.to_string(),
        }
    })?;
    bounded_bytes(
        "serialized context payload",
        serialized_payload.len(),
        MAX_SERIALIZED_CONTEXT_PAYLOAD_BYTES,
    )?;

    let version = event
        .payload
        .get("event_version")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| ContextProjectionError::MalformedPayload {
            seq: event.seq,
            reason: "event_version is missing or is not an unsigned integer".into(),
        })?;
    if version != u64::from(CONTEXT_NODE_EVENT_VERSION) {
        return Err(ContextProjectionError::UnsupportedEventVersion {
            seq: event.seq,
            found: version,
        });
    }
    let payload = serde_json::from_value::<ContextNodeRecordedPayloadV1>(event.payload.clone())
        .map_err(|error| ContextProjectionError::MalformedPayload {
            seq: event.seq,
            reason: error.to_string(),
        })?;
    let session_id =
        event
            .session_id
            .as_deref()
            .ok_or_else(|| ContextProjectionError::InvalidScope {
                seq: event.seq,
                reason: "session_id is missing".into(),
            })?;
    let scope = validate_requested_scope(
        event.seq,
        session_id,
        event.task_id.as_deref(),
        payload.node.scope,
    )?;
    let expected_correlation = scope.task_id.unwrap_or(scope.session_id);
    if event.correlation_id.as_deref() != Some(expected_correlation) {
        return Err(ContextProjectionError::CorrelationMismatch {
            seq: event.seq,
            expected: expected_correlation.to_owned(),
            actual: event.correlation_id.clone(),
        });
    }

    validate_durable_node(&payload.node)?;
    let resolved = resolve_sources(event_store, &payload.node, &scope, event.seq)?;
    if event.causation_id.as_deref() != Some(&resolved.causation_id) {
        return Err(ContextProjectionError::CausationMismatch {
            seq: event.seq,
            expected: resolved.causation_id,
            actual: event.causation_id.clone(),
        });
    }
    Ok(ValidatedRecordedNode {
        node: payload.node,
        session_id: scope.session_id.to_owned(),
        task_id: scope.task_id.map(str::to_owned),
    })
}

fn validate_durable_node(node: &ContextNode) -> Result<(), ContextProjectionError> {
    validate_node_identity_shape(node)?;
    bounded_bytes(
        "context node summary",
        node.summary.len(),
        MAX_CONTEXT_SUMMARY_BYTES,
    )?;
    bounded_list(
        "source_event_ids",
        node.source_event_ids.len(),
        1,
        MAX_CONTEXT_SOURCE_EVENT_IDS,
    )?;
    bounded_list(
        "supersedes",
        node.supersedes.len(),
        0,
        MAX_CONTEXT_SUPERSEDES,
    )?;
    validate_references("source_event_ids", &node.source_event_ids, None)?;
    validate_references("supersedes", &node.supersedes, Some(&node.id))?;
    node.validate().map_err(|error: ContextValidationError| {
        ContextProjectionError::InvalidNode {
            node_id: node.id.clone(),
            reason: error.to_string(),
        }
    })?;

    let serialized =
        serde_json::to_vec(node).map_err(|error| ContextProjectionError::InvalidNode {
            node_id: node.id.clone(),
            reason: error.to_string(),
        })?;
    bounded_bytes(
        "serialized context node",
        serialized.len(),
        MAX_SERIALIZED_CONTEXT_NODE_BYTES,
    )?;
    let payload = ContextNodeRecordedPayloadV1::new(node.clone());
    let serialized_payload =
        serde_json::to_vec(&payload).map_err(|error| ContextProjectionError::InvalidNode {
            node_id: node.id.clone(),
            reason: error.to_string(),
        })?;
    bounded_bytes(
        "serialized context payload",
        serialized_payload.len(),
        MAX_SERIALIZED_CONTEXT_PAYLOAD_BYTES,
    )?;
    Ok(())
}

fn validate_new_validity_precision(node: &ContextNode) -> Result<(), ContextProjectionError> {
    for (field, value) in [
        ("valid_from", node.valid_from.as_ref()),
        ("valid_until", node.valid_until.as_ref()),
    ] {
        if value.is_some_and(|value| value.timestamp_subsec_nanos() % 1_000_000 != 0) {
            return Err(ContextProjectionError::NonCanonicalValidityPrecision {
                node_id: node.id.clone(),
                field,
            });
        }
    }
    Ok(())
}

fn validate_node_identity_shape(node: &ContextNode) -> Result<(), ContextProjectionError> {
    bounded_bytes("context node id", node.id.len(), MAX_CONTEXT_NODE_ID_BYTES)?;
    if node.id.trim().is_empty() {
        return Err(ContextProjectionError::InvalidNode {
            node_id: node.id.clone(),
            reason: ContextValidationError::EmptyId.to_string(),
        });
    }
    Ok(())
}

fn validate_references(
    field: &'static str,
    values: &[String],
    self_id: Option<&str>,
) -> Result<(), ContextProjectionError> {
    let mut seen = HashSet::with_capacity(values.len());
    for (index, value) in values.iter().enumerate() {
        if value.trim().is_empty() {
            return Err(ContextProjectionError::EmptyReference { field, index });
        }
        bounded_bytes(field, value.len(), MAX_CONTEXT_REFERENCE_ID_BYTES)?;
        if !seen.insert(value.as_str()) {
            return Err(ContextProjectionError::DuplicateReference {
                field,
                id: value.clone(),
            });
        }
        if self_id == Some(value.as_str()) {
            return Err(ContextProjectionError::SelfSupersession {
                node_id: value.clone(),
            });
        }
    }
    Ok(())
}

fn projection_cache_conflicts(
    connection: &Connection,
    node: &ContextNode,
    scope: &NodeScope<'_>,
) -> Result<bool, ContextProjectionError> {
    if find_identity(connection, scope.session_id, &node.id)?.is_some() {
        return Ok(true);
    }

    for superseded_id in &node.supersedes {
        match find_node_scope(connection, scope.session_id, superseded_id)? {
            None => return Ok(true),
            Some(existing_task) if existing_task.as_deref() != scope.task_id => {
                return Ok(true);
            }
            Some(_) => {}
        }
    }
    Ok(false)
}

fn resolve_sources(
    event_store: &EventStore,
    node: &ContextNode,
    scope: &NodeScope<'_>,
    recording_seq: i64,
) -> Result<ResolvedNode, ContextProjectionError> {
    let mut sources = Vec::with_capacity(node.source_event_ids.len());
    for source_event_id in &node.source_event_ids {
        let source = event_store
            .get_by_event_id(source_event_id)?
            .ok_or_else(|| ContextProjectionError::MissingSource {
                node_id: node.id.clone(),
                source_event_id: source_event_id.clone(),
            })?;
        if source.seq >= recording_seq {
            return Err(ContextProjectionError::SourceNotPrior {
                node_id: node.id.clone(),
                source_event_id: source_event_id.clone(),
                source_seq: source.seq,
            });
        }
        if source.session_id.as_deref() != Some(scope.session_id) {
            return Err(ContextProjectionError::SourceSessionMismatch {
                node_id: node.id.clone(),
                source_event_id: source_event_id.clone(),
            });
        }
        if scope.task_id.is_some()
            && source.task_id.as_deref().is_some()
            && source.task_id.as_deref() != scope.task_id
        {
            return Err(ContextProjectionError::SourceTaskMismatch {
                node_id: node.id.clone(),
                source_event_id: source_event_id.clone(),
            });
        }
        sources.push(source);
    }

    let expected_actor = origin_actor(node.origin);
    if !sources.iter().any(|source| source.actor == expected_actor) {
        return Err(ContextProjectionError::OriginNotAttested {
            node_id: node.id.clone(),
            origin: node.origin,
        });
    }
    let causation_id = sources
        .iter()
        .max_by_key(|source| source.seq)
        .ok_or(ContextProjectionError::DurableListOutOfRange {
            field: "source_event_ids",
            actual: 0,
            minimum: 1,
            maximum: MAX_CONTEXT_SOURCE_EVENT_IDS,
        })?
        .event_id
        .clone();
    Ok(ResolvedNode { causation_id })
}

fn validate_requested_scope<'a>(
    seq: i64,
    session_id: &'a str,
    task_id: Option<&'a str>,
    node_scope: ContextScope,
) -> Result<NodeScope<'a>, ContextProjectionError> {
    if session_id.trim().is_empty() {
        return Err(ContextProjectionError::InvalidScope {
            seq,
            reason: "session_id is empty".into(),
        });
    }
    if task_id.is_some_and(|task| task.trim().is_empty()) {
        return Err(ContextProjectionError::InvalidScope {
            seq,
            reason: "task_id is empty".into(),
        });
    }
    match (node_scope, task_id) {
        (ContextScope::Session, None) | (ContextScope::Task, Some(_)) => Ok(NodeScope {
            session_id,
            task_id,
        }),
        (ContextScope::Session, Some(_)) => Err(ContextProjectionError::InvalidScope {
            seq,
            reason: "session-scoped node has a task_id".into(),
        }),
        (ContextScope::Task, None) => Err(ContextProjectionError::InvalidScope {
            seq,
            reason: "task-scoped node has no task_id".into(),
        }),
        _ => Err(ContextProjectionError::InvalidScope {
            seq,
            reason: format!("unsupported durable node scope {node_scope:?}"),
        }),
    }
}

fn validate_query_scope(
    session_id: &str,
    task_id: Option<&str>,
) -> Result<(), ContextProjectionError> {
    SessionId::new(session_id)?;
    if let Some(task_id) = task_id {
        TaskId::new(task_id)?;
    }
    Ok(())
}

fn find_identity(
    connection: &Connection,
    session_id: &str,
    node_id: &str,
) -> Result<Option<(String, i64)>, ContextProjectionError> {
    connection
        .query_row(
            "SELECT event_id, event_seq FROM projected_nodes WHERE session_id = ?1 AND node_id = ?2",
            params![session_id, node_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(Into::into)
}

fn find_node_scope(
    connection: &Connection,
    session_id: &str,
    node_id: &str,
) -> Result<Option<Option<String>>, ContextProjectionError> {
    connection
        .query_row(
            "SELECT task_id FROM projected_nodes WHERE session_id = ?1 AND node_id = ?2",
            params![session_id, node_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(Into::into)
}

fn projected_row(raw: RawProjectedRow) -> Result<ProjectedRow, ContextProjectionError> {
    let node = serde_json::from_str::<ContextNode>(&raw.node_json).map_err(|error| {
        ContextProjectionError::CorruptProjectionRow {
            seq: raw.event_seq,
            reason: error.to_string(),
        }
    })?;
    if node.id != raw.node_id {
        return Err(ContextProjectionError::CorruptProjectionRow {
            seq: raw.event_seq,
            reason: "stored node ID differs from serialized node".into(),
        });
    }
    validate_durable_node(&node).map_err(|error| ContextProjectionError::CorruptProjectionRow {
        seq: raw.event_seq,
        reason: error.to_string(),
    })?;
    validate_requested_scope(
        raw.event_seq,
        &raw.session_id,
        raw.task_id.as_deref(),
        node.scope,
    )?;
    Ok(ProjectedRow {
        node,
        superseded: raw.superseded,
    })
}

const fn epistemic_status(status: EpistemicStatus) -> &'static str {
    match status {
        EpistemicStatus::Asserted => "asserted",
        EpistemicStatus::Inferred => "inferred",
        EpistemicStatus::Verified => "verified",
        EpistemicStatus::Disputed => "disputed",
    }
}

const fn origin_actor(origin: ContextOrigin) -> EventActor {
    match origin {
        ContextOrigin::User => EventActor::User,
        ContextOrigin::Model => EventActor::Model,
        ContextOrigin::Capability => EventActor::Capability,
        ContextOrigin::Policy => EventActor::Policy,
        ContextOrigin::System => EventActor::System,
    }
}

fn task_key(task_id: Option<&str>) -> &str {
    task_id.unwrap_or(ZERO_TASK_KEY)
}

fn bounded_bytes(
    field: &'static str,
    actual: usize,
    maximum: usize,
) -> Result<(), ContextProjectionError> {
    if actual > maximum {
        Err(ContextProjectionError::DurableBytesExceeded {
            field,
            actual,
            maximum,
        })
    } else {
        Ok(())
    }
}

fn bounded_list(
    field: &'static str,
    actual: usize,
    minimum: usize,
    maximum: usize,
) -> Result<(), ContextProjectionError> {
    if !(minimum..=maximum).contains(&actual) {
        Err(ContextProjectionError::DurableListOutOfRange {
            field,
            actual,
            minimum,
            maximum,
        })
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ditto_context::{ContextLens, ContextNodeKind, EpistemicStatus};
    use ditto_protocol::NewEvent;
    use ditto_retrieval::{MAX_TOTAL_CANDIDATE_BYTES, RetrievalWorkKind};
    use serde_json::json;

    #[test]
    fn persistent_post_rebuild_corruption_returns_typed_snapshot_integrity_failure() {
        let directory = tempfile::tempdir().expect("snapshot integrity fixture");
        let store = EventStore::open(directory.path().join("state.db")).expect("event store");
        let projection = ContextProjection::open_in(directory.path()).expect("projection");
        let source = store
            .append(NewEvent {
                session_id: Some("session-integrity".into()),
                task_id: None,
                actor: EventActor::User,
                kind: "fixture.source".into(),
                payload: json!({"source": true}),
                causation_id: None,
                correlation_id: Some("session-integrity".into()),
                span_id: None,
            })
            .expect("source event");
        let node = ContextNode {
            id: "persistent-node".into(),
            kind: ContextNodeKind::Claim,
            summary: "canonical summary".into(),
            origin: ContextOrigin::User,
            epistemic: EpistemicStatus::Verified,
            scope: ContextScope::Session,
            lens: ContextLens::Task,
            confidence: 1.0,
            source_event_ids: vec![source.event_id.clone()],
            supersedes: Vec::new(),
            valid_from: None,
            valid_until: None,
        };
        let recorded = store
            .append(NewEvent {
                session_id: Some("session-integrity".into()),
                task_id: None,
                actor: EventActor::System,
                kind: event_kind::CONTEXT_NODE_RECORDED.into(),
                payload: serde_json::to_value(ContextNodeRecordedPayloadV1::new(node))
                    .expect("context payload"),
                causation_id: Some(source.event_id),
                correlation_id: Some("session-integrity".into()),
                span_id: None,
            })
            .expect("context event");
        projection.synchronize(&store).expect("initial sync");
        let connection = Connection::open(projection.database_path()).expect("corrupt cache");
        connection
            .execute(
                "UPDATE projected_nodes SET node_json = '{}' WHERE node_id = 'persistent-node'",
                [],
            )
            .expect("initial logical corruption");
        drop(connection);

        *projection
            .post_rebuild_snapshot_hook
            .lock()
            .expect("snapshot hook") = Some(Arc::new(|path| {
            let connection = Connection::open(path).expect("post-rebuild trigger connection");
            connection
                .execute_batch(
                    r#"
                    CREATE TRIGGER corrupt_verified_snapshot_after_rebuild
                    AFTER UPDATE OF through_seq ON projection_checkpoint
                    BEGIN
                        UPDATE projected_nodes
                        SET node_json = '{}'
                        WHERE node_id = 'persistent-node';
                    END;
                    UPDATE projection_checkpoint
                    SET through_seq = through_seq
                    WHERE singleton = 1;
                    "#,
                )
                .expect("post-rebuild corruption trigger");
        }));

        let error = projection
            .synchronize_and_verified_snapshot_through(
                &store,
                recorded.seq,
                "session-integrity",
                None,
            )
            .expect_err("persistent mismatch must not return a partial snapshot");
        assert!(matches!(
            error,
            ContextProjectionError::ProjectionSnapshotIntegrityMismatch { high_water }
                if high_water == recorded.seq
        ));
        assert_eq!(store.count().expect("source count"), 2);
    }

    #[test]
    fn catch_up_repair_consumes_the_verified_snapshot_rebuild_budget() {
        let directory = tempfile::tempdir().expect("catch-up repair fixture");
        let store = EventStore::open(directory.path().join("state.db")).expect("event store");
        let projection = ContextProjection::open_in(directory.path()).expect("projection");
        let source = store
            .append(NewEvent {
                session_id: Some("session-catch-up-budget".into()),
                task_id: None,
                actor: EventActor::User,
                kind: "fixture.source".into(),
                payload: json!({"source": true}),
                causation_id: None,
                correlation_id: Some("session-catch-up-budget".into()),
                span_id: None,
            })
            .expect("source event");
        projection
            .synchronize_through(&store, source.seq)
            .expect("prefix checkpoint");
        let node = ContextNode {
            id: "catch-up-budget-node".into(),
            kind: ContextNodeKind::Claim,
            summary: "canonical summary".into(),
            origin: ContextOrigin::User,
            epistemic: EpistemicStatus::Verified,
            scope: ContextScope::Session,
            lens: ContextLens::Task,
            confidence: 1.0,
            source_event_ids: vec![source.event_id.clone()],
            supersedes: Vec::new(),
            valid_from: None,
            valid_until: None,
        };
        let recorded = store
            .append(NewEvent {
                session_id: Some("session-catch-up-budget".into()),
                task_id: None,
                actor: EventActor::System,
                kind: event_kind::CONTEXT_NODE_RECORDED.into(),
                payload: serde_json::to_value(ContextNodeRecordedPayloadV1::new(node.clone()))
                    .expect("context payload"),
                causation_id: Some(source.event_id),
                correlation_id: Some("session-catch-up-budget".into()),
                span_id: None,
            })
            .expect("context event");
        let connection = Connection::open(projection.database_path()).expect("collision cache");
        connection
            .execute(
                "INSERT INTO projected_nodes (session_id, task_id, node_id, event_seq, event_id, node_json) VALUES (?1, NULL, ?2, ?3, ?4, ?5)",
                params![
                    "session-catch-up-budget",
                    "catch-up-budget-node",
                    recorded.seq + 10_000,
                    "cache-only-collision",
                    serde_json::to_string(&node).expect("cache node JSON"),
                ],
            )
            .expect("insert cache-only identity collision");
        drop(connection);

        *projection
            .post_rebuild_snapshot_hook
            .lock()
            .expect("snapshot hook") = Some(Arc::new(|path| {
            let connection = Connection::open(path).expect("post-catch-up repair connection");
            connection
                .execute(
                    "UPDATE projected_nodes SET node_json = '{}' WHERE node_id = 'catch-up-budget-node'",
                    [],
                )
                .expect("corrupt cache after catch-up repair");
        }));

        let error = projection
            .synchronize_and_verified_snapshot_through(
                &store,
                recorded.seq,
                "session-catch-up-budget",
                None,
            )
            .expect_err("catch-up repair already consumed the one rebuild budget");
        assert!(matches!(
            error,
            ContextProjectionError::ProjectionSnapshotIntegrityMismatch { high_water }
                if high_water == recorded.seq
        ));
        let connection = Connection::open(projection.database_path()).expect("inspect cache");
        let cached_json: String = connection
            .query_row(
                "SELECT node_json FROM projected_nodes WHERE node_id = 'catch-up-budget-node'",
                [],
                |row| row.get(0),
            )
            .expect("persistently corrupted row");
        assert_eq!(cached_json, "{}", "a second rebuild must not have occurred");
        assert_eq!(store.count().expect("source count"), 2);
    }

    #[test]
    fn cache_repair_accumulates_candidate_work_across_both_snapshot_attempts() {
        let directory = tempfile::tempdir().expect("cumulative repair budget fixture");
        let store = EventStore::open(directory.path().join("state.db")).expect("event store");
        let projection = ContextProjection::open_in(directory.path()).expect("projection");
        let source = store
            .append(NewEvent {
                session_id: Some("session-cumulative-repair".into()),
                task_id: None,
                actor: EventActor::User,
                kind: "fixture.source".into(),
                payload: serde_json::json!({"source": true}),
                causation_id: None,
                correlation_id: Some("session-cumulative-repair".into()),
                span_id: None,
            })
            .expect("source event");
        let node = ContextNode {
            id: "cumulative-repair-node".into(),
            kind: ContextNodeKind::Claim,
            summary: "x".repeat(16 * 1024),
            origin: ContextOrigin::User,
            epistemic: EpistemicStatus::Verified,
            scope: ContextScope::Session,
            lens: ContextLens::Task,
            confidence: 1.0,
            source_event_ids: vec![source.event_id.clone()],
            supersedes: Vec::new(),
            valid_from: None,
            valid_until: None,
        };
        let candidate_bytes = serde_json::to_string(&node).expect("candidate JSON").len();
        let recorded = store
            .append(NewEvent {
                session_id: Some("session-cumulative-repair".into()),
                task_id: None,
                actor: EventActor::System,
                kind: event_kind::CONTEXT_NODE_RECORDED.into(),
                payload: serde_json::to_value(ContextNodeRecordedPayloadV1::new(node))
                    .expect("context payload"),
                causation_id: Some(source.event_id),
                correlation_id: Some("session-cumulative-repair".into()),
                span_id: None,
            })
            .expect("context event");
        projection.rebuild(&store).expect("source-verified rebuild");

        *projection
            .post_active_snapshot_hook
            .lock()
            .expect("active snapshot hook") = Some(Box::new(|path| {
            let connection = Connection::open(path).expect("cache drift connection");
            connection
                .execute(
                    r#"
                    INSERT INTO projected_nodes (
                        session_id, task_id, node_id, event_seq, event_id, node_json
                    ) VALUES (?1, NULL, ?2, ?3, ?4, ?5)
                    "#,
                    params![
                        "cache-drift-session",
                        "cache-drift-node",
                        9_999_999_i64,
                        "cache-drift-event",
                        "{}"
                    ],
                )
                .expect("change SQLite data version after first snapshot");
        }));
        let retry_headroom = candidate_bytes + candidate_bytes / 2;
        let mut budget = RetrievalWorkBudget::new();
        budget
            .charge_candidate_bytes(MAX_TOTAL_CANDIDATE_BYTES - retry_headroom)
            .expect("precharge within candidate budget");

        let error = projection
            .synchronize_and_verified_snapshot_through_at(
                &store,
                recorded.seq,
                "session-cumulative-repair",
                None,
                Utc::now(),
                &mut budget,
            )
            .expect_err("combined first and repaired snapshots must exceed the budget");
        assert!(matches!(
            error,
            ContextProjectionError::Retrieval(RetrievalError::WorkBudgetExceeded {
                kind: RetrievalWorkKind::CandidateBytes,
                attempted,
                maximum: MAX_TOTAL_CANDIDATE_BYTES,
            }) if attempted > MAX_TOTAL_CANDIDATE_BYTES
        ));
        assert_eq!(
            projection
                .verification_metrics()
                .expect("repair metrics")
                .cache_repairs,
            1
        );
    }

    #[cfg(unix)]
    #[test]
    fn projection_sqlite_family_is_private_regular_and_current_user_owned() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        let directory = tempfile::tempdir().expect("projection privacy fixture");
        let private = directory.path().join("private");
        fs::create_dir(&private).expect("create projection directory");
        fs::set_permissions(&private, fs::Permissions::from_mode(0o777))
            .expect("loosen projection directory");
        let path = private.join(CONTEXT_PROJECTION_DATABASE_FILENAME);
        let projection = ContextProjection::open(&path).expect("open private projection");
        let metadata = fs::symlink_metadata(&private).expect("projection directory metadata");
        assert_eq!(metadata.permissions().mode() & 0o777, 0o700);
        // SAFETY: `geteuid` reads process identity and has no preconditions.
        let effective_uid = unsafe { libc::geteuid() };
        assert_eq!(metadata.uid(), effective_uid);
        for member in sqlite_family_paths(projection.database_path()) {
            let Ok(metadata) = fs::symlink_metadata(&member) else {
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
    fn projection_open_rejects_database_and_parent_symlinks() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().expect("projection symlink fixture");
        let private = directory.path().join("private");
        fs::create_dir(&private).expect("create private directory");
        let target = directory.path().join("target.db");
        fs::File::create(&target).expect("create symlink target");
        let database_link = private.join(CONTEXT_PROJECTION_DATABASE_FILENAME);
        symlink(&target, &database_link).expect("create database symlink");
        assert!(matches!(
            ContextProjection::open(&database_link),
            Err(ContextProjectionError::Io(error))
                if error.kind() == std::io::ErrorKind::PermissionDenied
        ));

        let real_parent = directory.path().join("real-parent");
        fs::create_dir(&real_parent).expect("create real parent");
        let parent_link = directory.path().join("parent-link");
        symlink(&real_parent, &parent_link).expect("create parent symlink");
        assert!(matches!(
            ContextProjection::open(parent_link.join(CONTEXT_PROJECTION_DATABASE_FILENAME)),
            Err(ContextProjectionError::Io(error))
                if error.kind() == std::io::ErrorKind::PermissionDenied
        ));
    }
}
