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
pub const CONTEXT_PROJECTION_SCHEMA_VERSION: i64 = 4;
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
/// Maximum immutable context identities retained for one session index.
pub const MAX_SESSION_INDEX_IDENTITIES: u64 = 65_536;
/// Maximum accounted bytes retained for one session index.
pub const MAX_SESSION_INDEX_BYTES: u64 = 256 * 1024 * 1024;
/// Maximum canonical events inspected by one normal delta synchronization.
pub const MAX_NORMAL_DELTA_EVENTS: u64 = 65_536;
/// Maximum serialized context payload bytes inspected by one normal delta.
pub const MAX_NORMAL_DELTA_CONTEXT_BYTES: u64 = 64 * 1024 * 1024;
/// Maximum deterministic verification work charged by one normal delta.
pub const MAX_NORMAL_DELTA_VERIFICATION_WORK: u64 = 2_000_000;

const SYNC_PAGE_SIZE: usize = 500;
const ZERO_TASK_KEY: &str = "";
const EVENT_STORE_DATABASE_FILENAME: &str = "state.db";
const INDEX_ENTRY_FIXED_BYTES: u64 = 8 * 5 + 32 * 3;
const GLOBAL_INDEX_DIGEST_DOMAIN: &[u8] = b"ditto.context-index.global.v1";
const SESSION_INDEX_DIGEST_DOMAIN: &[u8] = b"ditto.context-index.session.v1";
const PROVENANCE_DIGEST_DOMAIN: &[u8] = b"ditto.context-index.provenance.v1";

const SCHEMA_V4: &str = r#"
CREATE TABLE projection_checkpoint (
    singleton       INTEGER PRIMARY KEY CHECK (singleton = 1),
    schema_version  INTEGER NOT NULL,
    through_seq     INTEGER NOT NULL CHECK (through_seq >= 0),
    through_event_id TEXT,
    canonical_state_digest BLOB NOT NULL CHECK (length(canonical_state_digest) = 32),
    CHECK (
        (through_seq = 0 AND through_event_id IS NULL)
        OR (through_seq > 0 AND through_event_id IS NOT NULL)
    )
);

INSERT INTO projection_checkpoint (
    singleton, schema_version, through_seq, through_event_id,
    canonical_state_digest
) VALUES (1, 4, 0, NULL, zeroblob(32));

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

CREATE TABLE session_index_nodes (
    session_id          TEXT NOT NULL,
    node_id             TEXT NOT NULL,
    task_id             TEXT,
    event_seq           INTEGER NOT NULL UNIQUE,
    event_id            TEXT NOT NULL UNIQUE,
    node_digest         BLOB NOT NULL CHECK (length(node_digest) = 32),
    provenance_digest   BLOB NOT NULL CHECK (length(provenance_digest) = 32),
    source_count        INTEGER NOT NULL CHECK (source_count BETWEEN 1 AND 64),
    causation_seq       INTEGER NOT NULL CHECK (causation_seq > 0),
    causation_event_id  TEXT NOT NULL,
    supersession_digest BLOB NOT NULL CHECK (length(supersession_digest) = 32),
    supersession_count  INTEGER NOT NULL CHECK (supersession_count BETWEEN 0 AND 64),
    accounted_bytes     INTEGER NOT NULL CHECK (accounted_bytes > 0),
    PRIMARY KEY (session_id, node_id)
);

CREATE INDEX session_index_nodes_scope
    ON session_index_nodes(session_id, task_id, node_id);

CREATE TABLE session_index_state (
    session_id       TEXT PRIMARY KEY,
    through_seq      INTEGER NOT NULL CHECK (through_seq > 0),
    through_event_id TEXT NOT NULL,
    state_digest     BLOB NOT NULL CHECK (length(state_digest) = 32),
    entry_count      INTEGER NOT NULL CHECK (entry_count BETWEEN 1 AND 65536),
    accounted_bytes  INTEGER NOT NULL CHECK (
        accounted_bytes > 0 AND accounted_bytes <= 268435456
    )
);
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
    pub canonical_state_digest: [u8; 32],
}

impl ProjectionCheckpoint {
    fn zero() -> Self {
        Self {
            schema_version: CONTEXT_PROJECTION_SCHEMA_VERSION,
            through_seq: 0,
            through_event_id: None,
            canonical_state_digest: empty_state_digest(GLOBAL_INDEX_DIGEST_DOMAIN),
        }
    }
}

/// Observable outcome of one bounded-high-water synchronization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectionSync {
    pub captured_high_water: i64,
    pub checkpoint: ProjectionCheckpoint,
    pub rebuilt: bool,
    source_index_recovery: bool,
}

/// Canonical recording identity for one session-wide node ID.
///
/// Admission retries obtain this only from the process-local source-verified
/// compact index, never from an unverified projection row, and never compare or
/// rewrite the committed payload.
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
    pub full_replay_events: u64,
    pub delta_events: u64,
    pub delta_context_payload_bytes: u64,
    pub delta_verification_work: u64,
    pub admission_index_lookups: u64,
}

/// Inspectable derived checkpoint for one compact session index.
///
/// This value carries no admission authority; only the process-local source
/// verification proof can authorize index lookups.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionIndexCheckpoint {
    pub through_seq: i64,
    pub through_event_id: String,
    pub canonical_state_digest: [u8; 32],
    pub identity_count: u64,
    pub accounted_bytes: u64,
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
    #[error(
        "compact session index remains inconsistent with canonical history through sequence {high_water} after one rebuild"
    )]
    SessionIndexIntegrityMismatch { high_water: i64 },
    #[error(
        "{dimension} attempted {attempted}, exceeding the compact session-index limit {maximum}"
    )]
    SessionIndexLimitExceeded {
        dimension: &'static str,
        attempted: u64,
        maximum: u64,
    },
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
    sessions: HashMap<String, HashMap<String, SessionIndexIdentity>>,
}

#[derive(Debug, Default)]
struct ProjectionVerificationState {
    source: Option<SourceVerification>,
    metrics: ProjectionVerificationMetrics,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CatchUpMode {
    FullReplay,
    VerifiedDelta,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct CatchUpWork {
    events: u64,
    context_payload_bytes: u64,
    verification_work: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct DeltaVerificationBudget {
    work: CatchUpWork,
}

impl DeltaVerificationBudget {
    fn charge_event(&mut self) -> Result<(), ContextProjectionError> {
        charge_bounded_counter(
            &mut self.work.events,
            1,
            "normal delta events",
            MAX_NORMAL_DELTA_EVENTS,
        )?;
        self.charge_work(1)
    }

    fn charge_context_payload(&mut self, bytes: usize) -> Result<(), ContextProjectionError> {
        let bytes = u64::try_from(bytes).map_err(|_| {
            ContextProjectionError::SessionIndexLimitExceeded {
                dimension: "normal delta context payload bytes",
                attempted: u64::MAX,
                maximum: MAX_NORMAL_DELTA_CONTEXT_BYTES,
            }
        })?;
        charge_bounded_counter(
            &mut self.work.context_payload_bytes,
            bytes,
            "normal delta context payload bytes",
            MAX_NORMAL_DELTA_CONTEXT_BYTES,
        )?;
        self.charge_work(1)
    }

    fn charge_lookup(&mut self) -> Result<(), ContextProjectionError> {
        self.charge_work(1)
    }

    fn charge_work(&mut self, amount: u64) -> Result<(), ContextProjectionError> {
        charge_bounded_counter(
            &mut self.work.verification_work,
            amount,
            "normal delta verification work",
            MAX_NORMAL_DELTA_VERIFICATION_WORK,
        )
    }

    fn remaining_events(&self) -> u64 {
        MAX_NORMAL_DELTA_EVENTS.saturating_sub(self.work.events)
    }
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

    pub fn session_index_checkpoint(
        &self,
        session_id: &str,
    ) -> Result<Option<SessionIndexCheckpoint>, ContextProjectionError> {
        SessionId::new(session_id)?;
        let _gate = self.sync_gate()?;
        let connection = self.connection()?;
        Ok(
            read_session_index_state(&connection, session_id)?.map(|state| {
                SessionIndexCheckpoint {
                    through_seq: state.through_seq,
                    through_event_id: state.through_event_id,
                    canonical_state_digest: state.state_digest,
                    identity_count: state.entry_count,
                    accounted_bytes: state.accounted_bytes,
                }
            }),
        )
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
        let (sqlite_data_version, sessions) = {
            let connection = self.connection()?;
            let sqlite_data_version =
                connection.pragma_query_value(None, "data_version", |row| row.get(0))?;
            let sessions = load_verified_session_identities(&connection, checkpoint)?;
            (sqlite_data_version, sessions)
        };
        let mut state = self.verification_state()?;
        state.source = Some(SourceVerification {
            checkpoint: checkpoint.clone(),
            sqlite_data_version,
            sessions,
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
        let after_seq = self
            .source_verification_checkpoint()?
            .map_or(0, |checkpoint| checkpoint.through_seq);
        let (sqlite_data_version, updates) = {
            let connection = self.connection()?;
            let sqlite_data_version =
                connection.pragma_query_value(None, "data_version", |row| row.get(0))?;
            let updates =
                load_session_identity_updates(&connection, after_seq, checkpoint.through_seq)?;
            (sqlite_data_version, updates)
        };
        let mut state = self.verification_state()?;
        let source =
            state
                .source
                .as_mut()
                .ok_or(ContextProjectionError::SessionIndexIntegrityMismatch {
                    high_water: checkpoint.through_seq,
                })?;
        for (session_id, node_id, identity) in updates {
            if source
                .sessions
                .entry(session_id.clone())
                .or_default()
                .insert(node_id.clone(), identity)
                .is_some()
            {
                return Err(ContextProjectionError::CorruptProjectionRow {
                    seq: checkpoint.through_seq,
                    reason: format!(
                        "session index delta repeats identity ({session_id}, {node_id})"
                    ),
                });
            }
        }
        source.checkpoint = checkpoint.clone();
        source.sqlite_data_version = sqlite_data_version;
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

    fn record_catch_up_work(
        &self,
        mode: CatchUpMode,
        work: CatchUpWork,
    ) -> Result<(), ContextProjectionError> {
        let mut state = self.verification_state()?;
        match mode {
            CatchUpMode::FullReplay => {
                state.metrics.full_replay_events =
                    state.metrics.full_replay_events.saturating_add(work.events);
            }
            CatchUpMode::VerifiedDelta => {
                state.metrics.delta_events = state.metrics.delta_events.saturating_add(work.events);
                state.metrics.delta_context_payload_bytes = state
                    .metrics
                    .delta_context_payload_bytes
                    .saturating_add(work.context_payload_bytes);
                state.metrics.delta_verification_work = state
                    .metrics
                    .delta_verification_work
                    .saturating_add(work.verification_work);
            }
        }
        Ok(())
    }

    fn record_admission_index_lookups(&self, lookups: u64) -> Result<(), ContextProjectionError> {
        let mut state = self.verification_state()?;
        state.metrics.admission_index_lookups = state
            .metrics
            .admission_index_lookups
            .saturating_add(lookups);
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
        } else if synchronized.source_index_recovery
            && self.recover_source_verification_from_cache(&synchronized.checkpoint)?
        {
            let mut state = self.verification_state()?;
            state.metrics.delta_synchronizations =
                state.metrics.delta_synchronizations.saturating_add(1);
        }
        Ok(())
    }

    fn recover_source_verification_from_cache(
        &self,
        checkpoint: &ProjectionCheckpoint,
    ) -> Result<bool, ContextProjectionError> {
        let Some(prior) = self.verification_state()?.source.clone() else {
            return Ok(false);
        };
        let (sqlite_data_version, actual, updates) = {
            let connection = self.connection()?;
            let actual = match load_verified_session_identities(&connection, checkpoint) {
                Ok(actual) => actual,
                Err(
                    ContextProjectionError::SessionIndexIntegrityMismatch { .. }
                    | ContextProjectionError::CorruptProjectionRow { .. }
                    | ContextProjectionError::SessionIndexLimitExceeded { .. },
                ) => return Ok(false),
                Err(error) => return Err(error),
            };
            let updates = load_session_identity_updates(
                &connection,
                prior.checkpoint.through_seq,
                checkpoint.through_seq,
            )?;
            let sqlite_data_version =
                connection.pragma_query_value(None, "data_version", |row| row.get(0))?;
            (sqlite_data_version, actual, updates)
        };
        let mut expected = prior.sessions;
        for (session_id, node_id, identity) in updates {
            if expected
                .entry(session_id)
                .or_default()
                .insert(node_id, identity)
                .is_some()
            {
                return Ok(false);
            }
        }
        if expected != actual {
            return Ok(false);
        }
        self.verification_state()?.source = Some(SourceVerification {
            checkpoint: checkpoint.clone(),
            sqlite_data_version,
            sessions: actual,
        });
        Ok(true)
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
                rebuilt = true;
            } else if self.recover_source_verification_from_cache(&synchronized.checkpoint)? {
                if source_checkpoint
                    .as_ref()
                    .is_some_and(|checkpoint| checkpoint.through_seq < high_water)
                {
                    let mut state = self.verification_state()?;
                    state.metrics.delta_synchronizations =
                        state.metrics.delta_synchronizations.saturating_add(1);
                }
            } else {
                self.rebuild_verified_through_locked(event_store, high_water, true)?;
                rebuilt = true;
            }
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

    /// Look up one committed identity through the source-verified compact
    /// session index at an explicit captured high-water.
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
        self.ensure_verified_index_locked(event_store, high_water)?;
        let identity = self.source_index_identity(session_id, node_id)?;
        self.record_admission_index_lookups(1)?;
        Ok(identity.map(|identity| CommittedContextIdentity {
            session_id: session_id.to_owned(),
            task_id: identity.task_id,
            node_id: node_id.to_owned(),
            event_id: identity.event_id,
            event_seq: identity.event_seq,
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
        self.ensure_verified_index_locked(event_store, high_water)?;

        if draft.session_id.trim().is_empty() {
            return Err(ContextProjectionError::InvalidScope {
                seq: 0,
                reason: "session_id is empty".into(),
            });
        }
        let proposed_identity = self.source_index_identity(&draft.session_id, &draft.node.id)?;
        self.record_admission_index_lookups(1)?;
        if let Some(existing) = proposed_identity {
            return Err(ContextProjectionError::DuplicateNodeIdentity {
                session_id: draft.session_id.clone(),
                node_id: draft.node.id.clone(),
                event_id: existing.event_id,
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

        for superseded_id in &draft.node.supersedes {
            let existing = self.source_index_identity(&draft.session_id, superseded_id)?;
            self.record_admission_index_lookups(1)?;
            match existing {
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
            None,
        )?;

        Ok(ValidatedContextNodeDraft {
            node: draft.node.clone(),
            session_id: draft.session_id.clone(),
            task_id: draft.task_id.clone(),
            causation_id: resolved.causation_id,
        })
    }

    fn ensure_verified_index_locked(
        &self,
        event_store: &EventStore,
        high_water: i64,
    ) -> Result<ProjectionCheckpoint, ContextProjectionError> {
        let checkpoint = self.checkpoint()?;
        if checkpoint.through_seq != high_water
            || !checkpoint_anchor_matches(&checkpoint, event_store)?
        {
            return Err(ContextProjectionError::ProjectionNotSynchronized {
                checkpoint: checkpoint.through_seq,
                high_water,
            });
        }
        if high_water == 0
            && checkpoint.canonical_state_digest == empty_state_digest(GLOBAL_INDEX_DIGEST_DOMAIN)
        {
            return Ok(checkpoint);
        }
        if self.source_verification_matches_cache()? {
            return Ok(checkpoint);
        }

        let rebuilt = self.rebuild_verified_through_locked(event_store, high_water, true)?;
        if !self.source_verification_matches_cache()? {
            return Err(ContextProjectionError::SessionIndexIntegrityMismatch { high_water });
        }
        Ok(rebuilt.checkpoint)
    }

    fn source_index_identity(
        &self,
        session_id: &str,
        node_id: &str,
    ) -> Result<Option<SessionIndexIdentity>, ContextProjectionError> {
        Ok(self
            .verification_state()?
            .source
            .as_ref()
            .and_then(|source| source.sessions.get(session_id))
            .and_then(|session| session.get(node_id))
            .cloned())
    }

    fn prevalidate_delta_against_verified_index(
        &self,
        event_store: &EventStore,
        after_seq: i64,
        through_seq: i64,
    ) -> Result<(), ContextProjectionError> {
        let mut overlay = HashMap::<(String, String), SessionIndexIdentity>::new();
        let mut budget = DeltaVerificationBudget::default();
        let mut cursor = after_seq;
        while cursor < through_seq {
            if budget.remaining_events() == 0 {
                return Err(ContextProjectionError::SessionIndexLimitExceeded {
                    dimension: "normal delta events",
                    attempted: MAX_NORMAL_DELTA_EVENTS + 1,
                    maximum: MAX_NORMAL_DELTA_EVENTS,
                });
            }
            let limit = usize::try_from(
                budget
                    .remaining_events()
                    .min(u64::try_from(SYNC_PAGE_SIZE).unwrap_or(u64::MAX)),
            )
            .unwrap_or(SYNC_PAGE_SIZE);
            let page = event_store.list_through(
                &EventQuery {
                    after_seq: Some(cursor),
                    limit: Some(limit),
                    ..EventQuery::default()
                },
                through_seq,
            )?;
            let Some(last) = page.last() else {
                return Err(ContextProjectionError::HighWaterUnreachable {
                    cursor,
                    high_water: through_seq,
                });
            };
            let mut previous = cursor;
            for event in &page {
                budget.charge_event()?;
                if event.seq <= previous || event.seq > through_seq {
                    return Err(ContextProjectionError::NonMonotonicPage {
                        after: previous,
                        found: event.seq,
                    });
                }
                previous = event.seq;
                if event.kind != event_kind::CONTEXT_NODE_RECORDED {
                    continue;
                }
                let validated =
                    decode_recorded_node_with_budget(event_store, event, Some(&mut budget))?;
                budget.charge_lookup()?;
                let key = (validated.session_id.clone(), validated.node.id.clone());
                let existing = overlay
                    .get(&key)
                    .cloned()
                    .or(self.source_index_identity(&validated.session_id, &validated.node.id)?);
                if let Some(existing) = existing {
                    return Err(ContextProjectionError::DuplicateNodeIdentity {
                        session_id: validated.session_id,
                        node_id: validated.node.id,
                        event_id: existing.event_id,
                        seq: existing.event_seq,
                    });
                }
                for superseded_id in &validated.node.supersedes {
                    budget.charge_lookup()?;
                    let superseded_key = (validated.session_id.clone(), superseded_id.clone());
                    let existing = overlay
                        .get(&superseded_key)
                        .cloned()
                        .or(self.source_index_identity(&validated.session_id, superseded_id)?);
                    match existing {
                        None => {
                            return Err(ContextProjectionError::MissingSupersededNode {
                                node_id: validated.node.id,
                                superseded_id: superseded_id.clone(),
                            });
                        }
                        Some(existing) if existing.task_id != validated.task_id => {
                            return Err(ContextProjectionError::SupersessionScopeMismatch {
                                node_id: validated.node.id,
                                superseded_id: superseded_id.clone(),
                            });
                        }
                        Some(_) => {}
                    }
                }
                overlay.insert(
                    key,
                    SessionIndexIdentity {
                        task_id: validated.task_id,
                        event_seq: event.seq,
                        event_id: event.event_id.clone(),
                    },
                );
            }
            cursor = last.seq;
        }
        Ok(())
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

        let source_verified = self.source_verification_matches_cache()?;
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

        let mut source_index_recovery = checkpoint.through_seq < high_water
            && !source_verified
            && self.source_verification_checkpoint()?.as_ref() == Some(&checkpoint);
        if source_index_recovery {
            self.prevalidate_delta_against_verified_index(
                event_store,
                checkpoint.through_seq,
                high_water,
            )?;
            if !self.recover_source_verification_from_cache(&checkpoint)? {
                let mut connection = self.connection()?;
                reset_schema(&mut connection)?;
                checkpoint = ProjectionCheckpoint::zero();
                rebuilt = true;
                source_index_recovery = false;
            }
        }

        let mode = if checkpoint.through_seq == 0 && (rebuilt || !source_verified) {
            CatchUpMode::FullReplay
        } else {
            CatchUpMode::VerifiedDelta
        };
        let mut completed_mode = mode;
        let caught_up =
            match self.catch_up_from_checkpoint(event_store, high_water, checkpoint, mode) {
                Ok(caught_up) => caught_up,
                Err(CatchUpError::Public(error)) => return Err(error.into()),
                Err(CatchUpError::RepairableCacheDivergence) if !rebuilt => {
                    {
                        let mut connection = self.connection()?;
                        reset_schema(&mut connection)?;
                    }
                    rebuilt = true;
                    completed_mode = CatchUpMode::FullReplay;
                    match self.catch_up_from_checkpoint(
                        event_store,
                        high_water,
                        ProjectionCheckpoint::zero(),
                        CatchUpMode::FullReplay,
                    ) {
                        Ok(caught_up) => caught_up,
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

        self.record_catch_up_work(completed_mode, caught_up.work)?;

        Ok(ProjectionSync {
            captured_high_water: high_water,
            checkpoint: caught_up.checkpoint,
            rebuilt,
            source_index_recovery: source_index_recovery && !rebuilt,
        })
    }

    fn catch_up_from_checkpoint(
        &self,
        event_store: &EventStore,
        high_water: i64,
        mut checkpoint: ProjectionCheckpoint,
        mode: CatchUpMode,
    ) -> Result<CatchUpResult, CatchUpError> {
        let mut delta_budget =
            (mode == CatchUpMode::VerifiedDelta).then(DeltaVerificationBudget::default);
        let mut work = CatchUpWork::default();
        let mut cursor = checkpoint.through_seq;
        while cursor < high_water {
            let page_limit = match delta_budget.as_ref() {
                Some(budget) if budget.remaining_events() == 0 => {
                    return Err(ContextProjectionError::SessionIndexLimitExceeded {
                        dimension: "normal delta events",
                        attempted: MAX_NORMAL_DELTA_EVENTS + 1,
                        maximum: MAX_NORMAL_DELTA_EVENTS,
                    }
                    .into());
                }
                Some(budget) => usize::try_from(
                    budget
                        .remaining_events()
                        .min(u64::try_from(SYNC_PAGE_SIZE).unwrap_or(u64::MAX)),
                )
                .unwrap_or(SYNC_PAGE_SIZE),
                None => SYNC_PAGE_SIZE,
            };
            let page = event_store
                .list_through(
                    &EventQuery {
                        after_seq: Some(cursor),
                        limit: Some(page_limit),
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
                if let Some(budget) = delta_budget.as_mut() {
                    budget.charge_event()?;
                } else {
                    work.events = work.events.saturating_add(1);
                }
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
                let mut page_digest = checkpoint.canonical_state_digest;
                for event in &page {
                    if event.kind == event_kind::CONTEXT_NODE_RECORDED {
                        page_digest = apply_context_event(
                            &transaction,
                            event_store,
                            event,
                            page_digest,
                            delta_budget.as_mut(),
                        )?;
                    }
                }
                let updated = transaction.execute(
                    "UPDATE projection_checkpoint SET schema_version = ?1, through_seq = ?2, through_event_id = ?3, canonical_state_digest = ?4 WHERE singleton = 1",
                    params![
                        CONTEXT_PROJECTION_SCHEMA_VERSION,
                        last.seq,
                        &last.event_id,
                        page_digest.as_slice(),
                    ],
                )
                .map_err(ContextProjectionError::from)?;
                if updated != 1 {
                    return Err(ContextProjectionError::MissingCheckpointDuringPage.into());
                }
                transaction.commit().map_err(ContextProjectionError::from)?;
                checkpoint.canonical_state_digest = page_digest;
            }
            cursor = last.seq;
            checkpoint = ProjectionCheckpoint {
                schema_version: CONTEXT_PROJECTION_SCHEMA_VERSION,
                through_seq: last.seq,
                through_event_id: Some(last.event_id),
                canonical_state_digest: checkpoint.canonical_state_digest,
            };
        }
        if let Some(budget) = delta_budget {
            work = budget.work;
        }
        Ok(CatchUpResult { checkpoint, work })
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
    causation_seq: i64,
    provenance_digest: [u8; 32],
    source_count: usize,
}

struct ValidatedRecordedNode {
    node: ContextNode,
    session_id: String,
    task_id: Option<String>,
    resolved: ResolvedNode,
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

struct CatchUpResult {
    checkpoint: ProjectionCheckpoint,
    work: CatchUpWork,
}

#[derive(Debug, Clone)]
struct SessionIndexEntry {
    session_id: String,
    node_id: String,
    task_id: Option<String>,
    event_seq: i64,
    event_id: String,
    node_digest: [u8; 32],
    provenance_digest: [u8; 32],
    source_count: usize,
    causation_seq: i64,
    causation_event_id: String,
    supersession_digest: [u8; 32],
    supersession_count: usize,
    accounted_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SessionIndexIdentity {
    task_id: Option<String>,
    event_seq: i64,
    event_id: String,
}

struct RawSessionIndexEntry {
    session_id: String,
    node_id: String,
    task_id: Option<String>,
    event_seq: i64,
    event_id: String,
    node_digest: Vec<u8>,
    provenance_digest: Vec<u8>,
    source_count: i64,
    causation_seq: i64,
    causation_event_id: String,
    supersession_digest: Vec<u8>,
    supersession_count: i64,
    accounted_bytes: i64,
    projected_node_id: Option<String>,
    projected_task_id: Option<String>,
    projected_event_seq: Option<i64>,
    projected_event_id: Option<String>,
    projected_node_json: Option<String>,
    projected_epistemic_status: Option<String>,
    projected_valid_from_millis: Option<i64>,
    projected_valid_from_submillis_nanos: Option<i64>,
    projected_valid_until_millis: Option<i64>,
    projected_valid_until_submillis_nanos: Option<i64>,
}

#[derive(Debug, Clone)]
struct SessionIndexState {
    through_seq: i64,
    through_event_id: String,
    state_digest: [u8; 32],
    entry_count: u64,
    accounted_bytes: u64,
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
              'supersession_edges',
              'session_index_nodes',
              'session_index_state'
          )
        "#,
        [],
        |row| row.get(0),
    )?;
    if table_count != 5 {
        return Ok(false);
    }
    let statements = [
        "SELECT schema_version, through_seq, through_event_id, canonical_state_digest FROM projection_checkpoint LIMIT 0",
        "SELECT session_id, task_id, node_id, event_seq, event_id, node_json, epistemic_status, valid_from_millis, valid_from_submillis_nanos, valid_until_millis, valid_until_submillis_nanos FROM projected_nodes LIMIT 0",
        "SELECT session_id, task_key, superseding_node_id, superseded_node_id, event_seq FROM supersession_edges LIMIT 0",
        "SELECT session_id, node_id, task_id, event_seq, event_id, node_digest, provenance_digest, source_count, causation_seq, causation_event_id, supersession_digest, supersession_count, accounted_bytes FROM session_index_nodes LIMIT 0",
        "SELECT session_id, through_seq, through_event_id, state_digest, entry_count, accounted_bytes FROM session_index_state LIMIT 0",
    ];
    Ok(statements
        .into_iter()
        .all(|statement| connection.prepare(statement).is_ok()))
}

fn reset_schema(connection: &mut Connection) -> Result<(), ContextProjectionError> {
    let transaction = connection.transaction()?;
    transaction.execute_batch(
        r#"
        DROP TABLE IF EXISTS session_index_state;
        DROP TABLE IF EXISTS session_index_nodes;
        DROP TABLE IF EXISTS supersession_edges;
        DROP TABLE IF EXISTS projected_nodes;
        DROP TABLE IF EXISTS projection_checkpoint;
        "#,
    )?;
    transaction.execute_batch(SCHEMA_V4)?;
    let empty_digest = empty_state_digest(GLOBAL_INDEX_DIGEST_DOMAIN);
    transaction.execute(
        "UPDATE projection_checkpoint SET canonical_state_digest = ?1 WHERE singleton = 1",
        params![empty_digest.as_slice()],
    )?;
    transaction.pragma_update(None, "user_version", CONTEXT_PROJECTION_SCHEMA_VERSION)?;
    transaction.commit()?;
    Ok(())
}

fn read_checkpoint(
    connection: &Connection,
) -> Result<Option<ProjectionCheckpoint>, ContextProjectionError> {
    let raw = connection
        .query_row(
            "SELECT schema_version, through_seq, through_event_id, canonical_state_digest FROM projection_checkpoint WHERE singleton = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get::<_, Vec<u8>>(3)?)),
        )
        .optional()
        .map_err(ContextProjectionError::from)?;
    raw.map(|(schema_version, through_seq, through_event_id, digest)| {
        Ok(ProjectionCheckpoint {
            schema_version,
            through_seq,
            through_event_id,
            canonical_state_digest: digest_from_blob(
                &digest,
                through_seq,
                "projection checkpoint canonical state digest",
            )?,
        })
    })
    .transpose()
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

fn read_session_index_identity(
    connection: &Connection,
    session_id: &str,
    node_id: &str,
) -> Result<Option<SessionIndexIdentity>, ContextProjectionError> {
    connection
        .query_row(
            r#"
            SELECT task_id, event_seq, event_id
            FROM session_index_nodes
            WHERE session_id = ?1 AND node_id = ?2
            "#,
            params![session_id, node_id],
            |row| {
                Ok(SessionIndexIdentity {
                    task_id: row.get(0)?,
                    event_seq: row.get(1)?,
                    event_id: row.get(2)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
}

fn load_verified_session_identities(
    connection: &Connection,
    checkpoint: &ProjectionCheckpoint,
) -> Result<HashMap<String, HashMap<String, SessionIndexIdentity>>, ContextProjectionError> {
    let mut statement = connection.prepare(
        r#"
        SELECT
            i.session_id, i.node_id, i.task_id, i.event_seq, i.event_id,
            i.node_digest, i.provenance_digest, i.source_count,
            i.causation_seq, i.causation_event_id,
            i.supersession_digest, i.supersession_count, i.accounted_bytes,
            p.node_id, p.task_id, p.event_seq, p.event_id, p.node_json,
            p.epistemic_status,
            p.valid_from_millis, p.valid_from_submillis_nanos,
            p.valid_until_millis, p.valid_until_submillis_nanos
        FROM session_index_nodes AS i
        LEFT JOIN projected_nodes AS p
          ON p.session_id = i.session_id AND p.node_id = i.node_id
        ORDER BY i.event_seq ASC, i.session_id ASC, i.node_id ASC
        "#,
    )?;
    let rows = statement.query_map([], |row| {
        Ok(RawSessionIndexEntry {
            session_id: row.get(0)?,
            node_id: row.get(1)?,
            task_id: row.get(2)?,
            event_seq: row.get(3)?,
            event_id: row.get(4)?,
            node_digest: row.get(5)?,
            provenance_digest: row.get(6)?,
            source_count: row.get(7)?,
            causation_seq: row.get(8)?,
            causation_event_id: row.get(9)?,
            supersession_digest: row.get(10)?,
            supersession_count: row.get(11)?,
            accounted_bytes: row.get(12)?,
            projected_node_id: row.get(13)?,
            projected_task_id: row.get(14)?,
            projected_event_seq: row.get(15)?,
            projected_event_id: row.get(16)?,
            projected_node_json: row.get(17)?,
            projected_epistemic_status: row.get(18)?,
            projected_valid_from_millis: row.get(19)?,
            projected_valid_from_submillis_nanos: row.get(20)?,
            projected_valid_until_millis: row.get(21)?,
            projected_valid_until_submillis_nanos: row.get(22)?,
        })
    })?;

    let mut sessions = HashMap::<String, HashMap<String, SessionIndexIdentity>>::new();
    let mut session_states = HashMap::<String, SessionIndexState>::new();
    let mut global_digest = empty_state_digest(GLOBAL_INDEX_DIGEST_DOMAIN);
    let mut row_count = 0_u64;
    for raw in rows {
        let raw = raw?;
        let entry = validated_session_index_entry(connection, raw)?;
        row_count = row_count.saturating_add(1);
        let prior = session_states.remove(&entry.session_id);
        let state = advance_session_index_state(prior, &entry)?;
        session_states.insert(entry.session_id.clone(), state);
        global_digest = advance_state_digest(GLOBAL_INDEX_DIGEST_DOMAIN, global_digest, &entry);
        if sessions
            .entry(entry.session_id.clone())
            .or_default()
            .insert(
                entry.node_id.clone(),
                SessionIndexIdentity {
                    task_id: entry.task_id,
                    event_seq: entry.event_seq,
                    event_id: entry.event_id,
                },
            )
            .is_some()
        {
            return Err(ContextProjectionError::CorruptProjectionRow {
                seq: entry.event_seq,
                reason: "compact session index contains a duplicate identity".into(),
            });
        }
    }

    if global_digest != checkpoint.canonical_state_digest {
        return Err(ContextProjectionError::SessionIndexIntegrityMismatch {
            high_water: checkpoint.through_seq,
        });
    }
    let projected_count: i64 =
        connection.query_row("SELECT COUNT(*) FROM projected_nodes", [], |row| row.get(0))?;
    if u64::try_from(projected_count).ok() != Some(row_count) {
        return Err(ContextProjectionError::SessionIndexIntegrityMismatch {
            high_water: checkpoint.through_seq,
        });
    }
    let state_count: i64 =
        connection.query_row("SELECT COUNT(*) FROM session_index_state", [], |row| {
            row.get(0)
        })?;
    if usize::try_from(state_count).ok() != Some(session_states.len()) {
        return Err(ContextProjectionError::SessionIndexIntegrityMismatch {
            high_water: checkpoint.through_seq,
        });
    }
    for (session_id, expected) in session_states {
        let actual = read_session_index_state(connection, &session_id)?.ok_or(
            ContextProjectionError::SessionIndexIntegrityMismatch {
                high_water: checkpoint.through_seq,
            },
        )?;
        if actual.through_seq != expected.through_seq
            || actual.through_event_id != expected.through_event_id
            || actual.state_digest != expected.state_digest
            || actual.entry_count != expected.entry_count
            || actual.accounted_bytes != expected.accounted_bytes
        {
            return Err(ContextProjectionError::SessionIndexIntegrityMismatch {
                high_water: checkpoint.through_seq,
            });
        }
    }
    Ok(sessions)
}

fn validated_session_index_entry(
    connection: &Connection,
    raw: RawSessionIndexEntry,
) -> Result<SessionIndexEntry, ContextProjectionError> {
    let projected_matches = raw.projected_node_id.as_deref() == Some(raw.node_id.as_str())
        && raw.projected_task_id == raw.task_id
        && raw.projected_event_seq == Some(raw.event_seq)
        && raw.projected_event_id.as_deref() == Some(raw.event_id.as_str());
    let Some(projected_node_json) = raw.projected_node_json.as_deref() else {
        return Err(ContextProjectionError::SessionIndexIntegrityMismatch {
            high_water: raw.event_seq,
        });
    };
    let node_digest = digest_from_blob(&raw.node_digest, raw.event_seq, "indexed node digest")?;
    let provenance_digest = digest_from_blob(
        &raw.provenance_digest,
        raw.event_seq,
        "indexed provenance digest",
    )?;
    let supersession_digest = digest_from_blob(
        &raw.supersession_digest,
        raw.event_seq,
        "indexed supersession digest",
    )?;
    let source_count = usize::try_from(raw.source_count).map_err(|_| {
        ContextProjectionError::CorruptProjectionRow {
            seq: raw.event_seq,
            reason: "indexed source count is negative".into(),
        }
    })?;
    let supersession_count = usize::try_from(raw.supersession_count).map_err(|_| {
        ContextProjectionError::CorruptProjectionRow {
            seq: raw.event_seq,
            reason: "indexed supersession count is negative".into(),
        }
    })?;
    let accounted_bytes = u64::try_from(raw.accounted_bytes).map_err(|_| {
        ContextProjectionError::CorruptProjectionRow {
            seq: raw.event_seq,
            reason: "indexed accounted bytes are negative".into(),
        }
    })?;
    let node = serde_json::from_str::<ContextNode>(projected_node_json).map_err(|error| {
        ContextProjectionError::CorruptProjectionRow {
            seq: raw.event_seq,
            reason: error.to_string(),
        }
    })?;
    let expected_accounted_bytes = session_index_entry_bytes(
        &raw.session_id,
        raw.task_id.as_deref(),
        &raw.node_id,
        &raw.event_id,
        &raw.causation_event_id,
    )?;
    let expected_valid_from = node.valid_from.as_ref().map(projection_timestamp_parts);
    let expected_valid_until = node.valid_until.as_ref().map(projection_timestamp_parts);
    let cached_edge_shape = cached_outgoing_edge_digest(connection, &raw.session_id, &raw.node_id)?;
    if !projected_matches
        || digest_bytes(projected_node_json.as_bytes()) != node_digest
        || raw.projected_epistemic_status.as_deref() != Some(epistemic_status(node.epistemic))
        || raw.projected_valid_from_millis != expected_valid_from.map(|value| value.0)
        || raw.projected_valid_from_submillis_nanos != expected_valid_from.map(|value| value.1)
        || raw.projected_valid_until_millis != expected_valid_until.map(|value| value.0)
        || raw.projected_valid_until_submillis_nanos != expected_valid_until.map(|value| value.1)
        || cached_edge_shape != (supersession_digest, supersession_count)
        || !(1..=MAX_CONTEXT_SOURCE_EVENT_IDS).contains(&source_count)
        || supersession_count > MAX_CONTEXT_SUPERSEDES
        || node.id != raw.node_id
        || node.source_event_ids.len() != source_count
        || node.supersedes.len() != supersession_count
        || !node
            .source_event_ids
            .iter()
            .any(|source| source == &raw.causation_event_id)
        || raw.causation_seq <= 0
        || raw.causation_seq >= raw.event_seq
        || accounted_bytes != expected_accounted_bytes
    {
        return Err(ContextProjectionError::SessionIndexIntegrityMismatch {
            high_water: raw.event_seq,
        });
    }
    Ok(SessionIndexEntry {
        session_id: raw.session_id,
        node_id: raw.node_id,
        task_id: raw.task_id,
        event_seq: raw.event_seq,
        event_id: raw.event_id,
        node_digest,
        provenance_digest,
        source_count,
        causation_seq: raw.causation_seq,
        causation_event_id: raw.causation_event_id,
        supersession_digest,
        supersession_count,
        accounted_bytes,
    })
}

fn load_session_identity_updates(
    connection: &Connection,
    after_seq: i64,
    through_seq: i64,
) -> Result<Vec<(String, String, SessionIndexIdentity)>, ContextProjectionError> {
    let mut statement = connection.prepare(
        r#"
        SELECT session_id, node_id, task_id, event_seq, event_id
        FROM session_index_nodes
        WHERE event_seq > ?1 AND event_seq <= ?2
        ORDER BY event_seq ASC
        "#,
    )?;
    statement
        .query_map(params![after_seq, through_seq], |row| {
            let session_id = row.get::<_, String>(0)?;
            let node_id = row.get::<_, String>(1)?;
            Ok((
                session_id,
                node_id,
                SessionIndexIdentity {
                    task_id: row.get(2)?,
                    event_seq: row.get(3)?,
                    event_id: row.get(4)?,
                },
            ))
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(Into::into)
}

fn read_session_index_state(
    connection: &Connection,
    session_id: &str,
) -> Result<Option<SessionIndexState>, ContextProjectionError> {
    let raw = connection
        .query_row(
            r#"
            SELECT through_seq, through_event_id, state_digest,
                   entry_count, accounted_bytes
            FROM session_index_state
            WHERE session_id = ?1
            "#,
            [session_id],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Vec<u8>>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                ))
            },
        )
        .optional()?;
    raw.map(
        |(through_seq, through_event_id, digest, entry_count, accounted_bytes)| {
            let entry_count = u64::try_from(entry_count).map_err(|_| {
                ContextProjectionError::CorruptProjectionRow {
                    seq: through_seq,
                    reason: "session index entry count is negative".into(),
                }
            })?;
            let accounted_bytes = u64::try_from(accounted_bytes).map_err(|_| {
                ContextProjectionError::CorruptProjectionRow {
                    seq: through_seq,
                    reason: "session index accounted bytes are negative".into(),
                }
            })?;
            Ok(SessionIndexState {
                through_seq,
                through_event_id,
                state_digest: digest_from_blob(&digest, through_seq, "session index state digest")?,
                entry_count,
                accounted_bytes,
            })
        },
    )
    .transpose()
}

fn insert_session_index_entry(
    transaction: &Transaction<'_>,
    entry: &SessionIndexEntry,
) -> Result<(), ContextProjectionError> {
    transaction.execute(
        r#"
        INSERT INTO session_index_nodes (
            session_id, node_id, task_id, event_seq, event_id,
            node_digest, provenance_digest, source_count,
            causation_seq, causation_event_id,
            supersession_digest, supersession_count, accounted_bytes
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
        "#,
        params![
            &entry.session_id,
            &entry.node_id,
            entry.task_id.as_deref(),
            entry.event_seq,
            &entry.event_id,
            entry.node_digest.as_slice(),
            entry.provenance_digest.as_slice(),
            i64::try_from(entry.source_count).unwrap_or(i64::MAX),
            entry.causation_seq,
            &entry.causation_event_id,
            entry.supersession_digest.as_slice(),
            i64::try_from(entry.supersession_count).unwrap_or(i64::MAX),
            i64::try_from(entry.accounted_bytes).unwrap_or(i64::MAX),
        ],
    )?;
    Ok(())
}

fn write_session_index_state(
    transaction: &Transaction<'_>,
    session_id: &str,
    state: &SessionIndexState,
) -> Result<(), ContextProjectionError> {
    transaction.execute(
        r#"
        INSERT INTO session_index_state (
            session_id, through_seq, through_event_id, state_digest,
            entry_count, accounted_bytes
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
        ON CONFLICT(session_id) DO UPDATE SET
            through_seq = excluded.through_seq,
            through_event_id = excluded.through_event_id,
            state_digest = excluded.state_digest,
            entry_count = excluded.entry_count,
            accounted_bytes = excluded.accounted_bytes
        "#,
        params![
            session_id,
            state.through_seq,
            &state.through_event_id,
            state.state_digest.as_slice(),
            i64::try_from(state.entry_count).unwrap_or(i64::MAX),
            i64::try_from(state.accounted_bytes).unwrap_or(i64::MAX),
        ],
    )?;
    Ok(())
}

fn advance_session_index_state(
    prior: Option<SessionIndexState>,
    entry: &SessionIndexEntry,
) -> Result<SessionIndexState, ContextProjectionError> {
    let (prior_digest, prior_count, prior_bytes) = match prior {
        Some(prior) => {
            if prior.through_seq >= entry.event_seq {
                return Err(ContextProjectionError::CorruptProjectionRow {
                    seq: entry.event_seq,
                    reason: "session index event sequence did not advance".into(),
                });
            }
            (prior.state_digest, prior.entry_count, prior.accounted_bytes)
        }
        None => (
            empty_state_digest(SESSION_INDEX_DIGEST_DOMAIN),
            0_u64,
            0_u64,
        ),
    };
    let entry_count = checked_bounded_sum(
        prior_count,
        1,
        "session index identities",
        MAX_SESSION_INDEX_IDENTITIES,
    )?;
    let accounted_bytes = checked_bounded_sum(
        prior_bytes,
        entry.accounted_bytes,
        "session index accounted bytes",
        MAX_SESSION_INDEX_BYTES,
    )?;
    Ok(SessionIndexState {
        through_seq: entry.event_seq,
        through_event_id: entry.event_id.clone(),
        state_digest: advance_state_digest(SESSION_INDEX_DIGEST_DOMAIN, prior_digest, entry),
        entry_count,
        accounted_bytes,
    })
}

fn session_index_entry_bytes(
    session_id: &str,
    task_id: Option<&str>,
    node_id: &str,
    event_id: &str,
    causation_event_id: &str,
) -> Result<u64, ContextProjectionError> {
    [
        session_id.len(),
        task_id.map_or(0, str::len),
        node_id.len(),
        event_id.len(),
        causation_event_id.len(),
    ]
    .into_iter()
    .try_fold(INDEX_ENTRY_FIXED_BYTES, |total, bytes| {
        total
            .checked_add(u64::try_from(bytes).unwrap_or(u64::MAX))
            .ok_or(ContextProjectionError::SessionIndexLimitExceeded {
                dimension: "session index accounted bytes",
                attempted: u64::MAX,
                maximum: MAX_SESSION_INDEX_BYTES,
            })
    })
}

fn canonical_provenance_digest(sources: &[EventRecord]) -> [u8; 32] {
    let mut ordered = sources.iter().collect::<Vec<_>>();
    ordered.sort_unstable_by(|left, right| {
        left.seq
            .cmp(&right.seq)
            .then_with(|| left.event_id.cmp(&right.event_id))
    });
    let mut hasher = Sha256::new();
    update_framed_bytes(&mut hasher, PROVENANCE_DIGEST_DOMAIN);
    hasher.update(
        u64::try_from(ordered.len())
            .unwrap_or(u64::MAX)
            .to_be_bytes(),
    );
    for source in ordered {
        hasher.update(source.seq.to_be_bytes());
        update_framed_bytes(&mut hasher, source.event_id.as_bytes());
        update_framed_bytes(
            &mut hasher,
            source.session_id.as_deref().unwrap_or("").as_bytes(),
        );
        update_framed_bytes(
            &mut hasher,
            source.task_id.as_deref().unwrap_or("").as_bytes(),
        );
        update_framed_bytes(&mut hasher, source.actor.as_str().as_bytes());
    }
    hasher.finalize().into()
}

fn empty_state_digest(domain: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    update_framed_bytes(&mut hasher, domain);
    hasher.finalize().into()
}

fn advance_state_digest(domain: &[u8], prior: [u8; 32], entry: &SessionIndexEntry) -> [u8; 32] {
    let mut hasher = Sha256::new();
    update_framed_bytes(&mut hasher, domain);
    hasher.update(prior);
    update_framed_bytes(&mut hasher, entry.session_id.as_bytes());
    update_framed_bytes(&mut hasher, entry.node_id.as_bytes());
    update_framed_bytes(
        &mut hasher,
        entry.task_id.as_deref().unwrap_or("").as_bytes(),
    );
    hasher.update(entry.event_seq.to_be_bytes());
    update_framed_bytes(&mut hasher, entry.event_id.as_bytes());
    hasher.update(entry.node_digest);
    hasher.update(entry.provenance_digest);
    hasher.update(
        u64::try_from(entry.source_count)
            .unwrap_or(u64::MAX)
            .to_be_bytes(),
    );
    hasher.update(entry.causation_seq.to_be_bytes());
    update_framed_bytes(&mut hasher, entry.causation_event_id.as_bytes());
    hasher.update(entry.supersession_digest);
    hasher.update(
        u64::try_from(entry.supersession_count)
            .unwrap_or(u64::MAX)
            .to_be_bytes(),
    );
    hasher.update(entry.accounted_bytes.to_be_bytes());
    hasher.finalize().into()
}

fn update_framed_bytes(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update(u64::try_from(bytes.len()).unwrap_or(u64::MAX).to_be_bytes());
    hasher.update(bytes);
}

fn digest_from_blob(
    bytes: &[u8],
    seq: i64,
    label: &str,
) -> Result<[u8; 32], ContextProjectionError> {
    bytes
        .try_into()
        .map_err(|_| ContextProjectionError::CorruptProjectionRow {
            seq,
            reason: format!("{label} is not exactly 32 bytes"),
        })
}

fn checked_bounded_sum(
    current: u64,
    amount: u64,
    dimension: &'static str,
    maximum: u64,
) -> Result<u64, ContextProjectionError> {
    let attempted = current.checked_add(amount).unwrap_or(u64::MAX);
    if attempted > maximum {
        Err(ContextProjectionError::SessionIndexLimitExceeded {
            dimension,
            attempted,
            maximum,
        })
    } else {
        Ok(attempted)
    }
}

fn charge_bounded_counter(
    counter: &mut u64,
    amount: u64,
    dimension: &'static str,
    maximum: u64,
) -> Result<(), ContextProjectionError> {
    *counter = checked_bounded_sum(*counter, amount, dimension, maximum)?;
    Ok(())
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

fn apply_context_event(
    transaction: &Transaction<'_>,
    event_store: &EventStore,
    event: &EventRecord,
    prior_global_digest: [u8; 32],
    mut delta_budget: Option<&mut DeltaVerificationBudget>,
) -> Result<[u8; 32], ApplyContextEventError> {
    let validated =
        decode_recorded_node_with_budget(event_store, event, delta_budget.as_deref_mut())?;
    let scope = NodeScope {
        session_id: &validated.session_id,
        task_id: validated.task_id.as_deref(),
    };
    if let Some(budget) = delta_budget.as_deref_mut() {
        budget.charge_lookup()?;
    }
    if let Some(existing) =
        read_session_index_identity(transaction, &validated.session_id, &validated.node.id)?
    {
        return Err(ContextProjectionError::DuplicateNodeIdentity {
            session_id: validated.session_id,
            node_id: validated.node.id,
            event_id: existing.event_id,
            seq: existing.event_seq,
        }
        .into());
    }
    for superseded_id in &validated.node.supersedes {
        if let Some(budget) = delta_budget.as_deref_mut() {
            budget.charge_lookup()?;
        }
        match read_session_index_identity(transaction, &validated.session_id, superseded_id)? {
            None => {
                return Err(ContextProjectionError::MissingSupersededNode {
                    node_id: validated.node.id,
                    superseded_id: superseded_id.clone(),
                }
                .into());
            }
            Some(existing) if existing.task_id != validated.task_id => {
                return Err(ContextProjectionError::SupersessionScopeMismatch {
                    node_id: validated.node.id,
                    superseded_id: superseded_id.clone(),
                }
                .into());
            }
            Some(_) => {}
        }
    }
    if let Some(budget) = delta_budget.as_deref_mut() {
        budget.charge_lookup()?;
        budget.charge_lookup()?;
    }
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
    let node_digest = digest_bytes(node_json.as_bytes());
    let supersession_digest = canonical_edge_digest(
        &validated.node.id,
        task_key(scope.task_id),
        event.seq,
        &validated.node.supersedes,
    );
    let accounted_bytes = session_index_entry_bytes(
        &validated.session_id,
        validated.task_id.as_deref(),
        &validated.node.id,
        &event.event_id,
        &validated.resolved.causation_id,
    )?;
    let entry = SessionIndexEntry {
        session_id: validated.session_id.clone(),
        node_id: validated.node.id.clone(),
        task_id: validated.task_id.clone(),
        event_seq: event.seq,
        event_id: event.event_id.clone(),
        node_digest,
        provenance_digest: validated.resolved.provenance_digest,
        source_count: validated.resolved.source_count,
        causation_seq: validated.resolved.causation_seq,
        causation_event_id: validated.resolved.causation_id.clone(),
        supersession_digest,
        supersession_count: validated.node.supersedes.len(),
        accounted_bytes,
    };
    if let Some(budget) = delta_budget {
        budget.charge_lookup()?;
    }
    let prior_session_state = read_session_index_state(transaction, &validated.session_id)?;
    let session_state = advance_session_index_state(prior_session_state, &entry)?;

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
    insert_session_index_entry(transaction, &entry)?;
    write_session_index_state(transaction, &entry.session_id, &session_state)?;
    Ok(advance_state_digest(
        GLOBAL_INDEX_DIGEST_DOMAIN,
        prior_global_digest,
        &entry,
    ))
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
    decode_recorded_node_with_budget(event_store, event, None)
}

fn decode_recorded_node_with_budget(
    event_store: &EventStore,
    event: &EventRecord,
    mut delta_budget: Option<&mut DeltaVerificationBudget>,
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
    if let Some(budget) = delta_budget.as_deref_mut() {
        budget.charge_context_payload(serialized_payload.len())?;
    }

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
    let resolved = resolve_sources(event_store, &payload.node, &scope, event.seq, delta_budget)?;
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
        resolved,
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
    mut delta_budget: Option<&mut DeltaVerificationBudget>,
) -> Result<ResolvedNode, ContextProjectionError> {
    let mut sources = Vec::with_capacity(node.source_event_ids.len());
    for source_event_id in &node.source_event_ids {
        if let Some(budget) = delta_budget.as_deref_mut() {
            budget.charge_lookup()?;
        }
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
    let causation = sources.iter().max_by_key(|source| source.seq).ok_or(
        ContextProjectionError::DurableListOutOfRange {
            field: "source_event_ids",
            actual: 0,
            minimum: 1,
            maximum: MAX_CONTEXT_SOURCE_EVENT_IDS,
        },
    )?;
    Ok(ResolvedNode {
        causation_id: causation.event_id.clone(),
        causation_seq: causation.seq,
        provenance_digest: canonical_provenance_digest(&sources),
        source_count: sources.len(),
    })
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
    fn persistent_post_rebuild_session_index_corruption_returns_no_snapshot() {
        let directory = tempfile::tempdir().expect("session index integrity fixture");
        let store = EventStore::open(directory.path().join("state.db")).expect("event store");
        let projection = ContextProjection::open_in(directory.path()).expect("projection");
        let source = store
            .append(NewEvent {
                session_id: Some("session-index-integrity".into()),
                task_id: None,
                actor: EventActor::User,
                kind: "fixture.source".into(),
                payload: json!({"source": true}),
                causation_id: None,
                correlation_id: Some("session-index-integrity".into()),
                span_id: None,
            })
            .expect("source event");
        let node = ContextNode {
            id: "persistent-index-node".into(),
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
                session_id: Some("session-index-integrity".into()),
                task_id: None,
                actor: EventActor::System,
                kind: event_kind::CONTEXT_NODE_RECORDED.into(),
                payload: serde_json::to_value(ContextNodeRecordedPayloadV1::new(node))
                    .expect("context payload"),
                causation_id: Some(source.event_id),
                correlation_id: Some("session-index-integrity".into()),
                span_id: None,
            })
            .expect("context event");
        projection.rebuild(&store).expect("source-verified replay");
        let connection = Connection::open(projection.database_path()).expect("corrupt cache");
        connection
            .execute(
                "UPDATE projected_nodes SET node_json = '{}' WHERE node_id = 'persistent-index-node'",
                [],
            )
            .expect("force one repair attempt");
        drop(connection);

        *projection
            .post_rebuild_snapshot_hook
            .lock()
            .expect("snapshot hook") = Some(Arc::new(|path| {
            let connection = Connection::open(path).expect("post-rebuild index connection");
            connection
                .execute(
                    "UPDATE session_index_state SET state_digest = zeroblob(32) WHERE session_id = 'session-index-integrity'",
                    [],
                )
                .expect("persistently corrupt rebuilt session index");
        }));

        let error = projection
            .synchronize_and_verified_snapshot_through(
                &store,
                recorded.seq,
                "session-index-integrity",
                None,
            )
            .expect_err("persistent index mismatch must not return a snapshot");
        assert!(matches!(
            error,
            ContextProjectionError::ProjectionSnapshotIntegrityMismatch { high_water }
                if high_water == recorded.seq
        ));
        let connection = Connection::open(projection.database_path()).expect("inspect index");
        let digest: Vec<u8> = connection
            .query_row(
                "SELECT state_digest FROM session_index_state WHERE session_id = 'session-index-integrity'",
                [],
                |row| row.get(0),
            )
            .expect("persistently corrupted index digest");
        assert_eq!(
            digest,
            vec![0; 32],
            "a second rebuild must not have occurred"
        );
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

    fn bounded_index_entry(accounted_bytes: u64, event_seq: i64) -> SessionIndexEntry {
        SessionIndexEntry {
            session_id: "session-bounds".into(),
            node_id: format!("node-{event_seq}"),
            task_id: None,
            event_seq,
            event_id: format!("event-{event_seq}"),
            node_digest: [1; 32],
            provenance_digest: [2; 32],
            source_count: 1,
            causation_seq: event_seq - 1,
            causation_event_id: format!("source-{}", event_seq - 1),
            supersession_digest: [3; 32],
            supersession_count: 0,
            accounted_bytes,
        }
    }

    #[test]
    fn compact_index_and_delta_counters_accept_exact_bounds_and_reject_n_plus_one() {
        let entry = bounded_index_entry(1, 2);
        let count_at_n = advance_session_index_state(
            Some(SessionIndexState {
                through_seq: 1,
                through_event_id: "event-1".into(),
                state_digest: [4; 32],
                entry_count: MAX_SESSION_INDEX_IDENTITIES - 1,
                accounted_bytes: 1,
            }),
            &entry,
        )
        .expect("identity count N is accepted");
        assert_eq!(count_at_n.entry_count, MAX_SESSION_INDEX_IDENTITIES);
        let count_error = advance_session_index_state(Some(count_at_n), &bounded_index_entry(1, 3))
            .expect_err("identity count N+1 is rejected");
        assert!(matches!(
            count_error,
            ContextProjectionError::SessionIndexLimitExceeded {
                dimension: "session index identities",
                attempted,
                maximum: MAX_SESSION_INDEX_IDENTITIES,
            } if attempted == MAX_SESSION_INDEX_IDENTITIES + 1
        ));

        let bytes_at_n = advance_session_index_state(
            Some(SessionIndexState {
                through_seq: 1,
                through_event_id: "event-1".into(),
                state_digest: [5; 32],
                entry_count: 1,
                accounted_bytes: MAX_SESSION_INDEX_BYTES - 7,
            }),
            &bounded_index_entry(7, 2),
        )
        .expect("accounted byte N is accepted");
        assert_eq!(bytes_at_n.accounted_bytes, MAX_SESSION_INDEX_BYTES);
        let byte_error = advance_session_index_state(Some(bytes_at_n), &bounded_index_entry(1, 3))
            .expect_err("accounted byte N+1 is rejected");
        assert!(matches!(
            byte_error,
            ContextProjectionError::SessionIndexLimitExceeded {
                dimension: "session index accounted bytes",
                attempted,
                maximum: MAX_SESSION_INDEX_BYTES,
            } if attempted == MAX_SESSION_INDEX_BYTES + 1
        ));

        let mut delta = DeltaVerificationBudget::default();
        delta.work.events = MAX_NORMAL_DELTA_EVENTS - 1;
        delta.charge_event().expect("delta event N is accepted");
        let event_error = delta
            .charge_event()
            .expect_err("delta event N+1 is rejected");
        assert_eq!(delta.work.events, MAX_NORMAL_DELTA_EVENTS);
        assert!(matches!(
            event_error,
            ContextProjectionError::SessionIndexLimitExceeded {
                dimension: "normal delta events",
                attempted,
                maximum: MAX_NORMAL_DELTA_EVENTS,
            } if attempted == MAX_NORMAL_DELTA_EVENTS + 1
        ));

        let mut delta = DeltaVerificationBudget::default();
        delta.work.context_payload_bytes = MAX_NORMAL_DELTA_CONTEXT_BYTES - 1;
        delta
            .charge_context_payload(1)
            .expect("delta context byte N is accepted");
        let payload_error = delta
            .charge_context_payload(1)
            .expect_err("delta context byte N+1 is rejected");
        assert_eq!(
            delta.work.context_payload_bytes,
            MAX_NORMAL_DELTA_CONTEXT_BYTES
        );
        assert!(matches!(
            payload_error,
            ContextProjectionError::SessionIndexLimitExceeded {
                dimension: "normal delta context payload bytes",
                attempted,
                maximum: MAX_NORMAL_DELTA_CONTEXT_BYTES,
            } if attempted == MAX_NORMAL_DELTA_CONTEXT_BYTES + 1
        ));

        let mut delta = DeltaVerificationBudget::default();
        delta.work.verification_work = MAX_NORMAL_DELTA_VERIFICATION_WORK - 1;
        delta.charge_work(1).expect("delta work N is accepted");
        let work_error = delta
            .charge_work(1)
            .expect_err("delta work N+1 is rejected");
        assert_eq!(
            delta.work.verification_work,
            MAX_NORMAL_DELTA_VERIFICATION_WORK
        );
        assert!(matches!(
            work_error,
            ContextProjectionError::SessionIndexLimitExceeded {
                dimension: "normal delta verification work",
                attempted,
                maximum: MAX_NORMAL_DELTA_VERIFICATION_WORK,
            } if attempted == MAX_NORMAL_DELTA_VERIFICATION_WORK + 1
        ));
    }

    #[test]
    fn canonical_index_digest_is_domain_separated_ordered_and_field_sensitive() {
        let first = bounded_index_entry(128, 2);
        let second = bounded_index_entry(128, 3);
        let empty_global = empty_state_digest(GLOBAL_INDEX_DIGEST_DOMAIN);
        let empty_session = empty_state_digest(SESSION_INDEX_DIGEST_DOMAIN);
        assert_ne!(empty_global, empty_session);

        let first_global = advance_state_digest(GLOBAL_INDEX_DIGEST_DOMAIN, empty_global, &first);
        let ordered = advance_state_digest(GLOBAL_INDEX_DIGEST_DOMAIN, first_global, &second);
        let second_first = advance_state_digest(GLOBAL_INDEX_DIGEST_DOMAIN, empty_global, &second);
        let reversed = advance_state_digest(GLOBAL_INDEX_DIGEST_DOMAIN, second_first, &first);
        assert_ne!(ordered, reversed);

        let mut changed = second.clone();
        changed.provenance_digest[0] ^= 0xff;
        assert_ne!(
            ordered,
            advance_state_digest(GLOBAL_INDEX_DIGEST_DOMAIN, first_global, &changed)
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
