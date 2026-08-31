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

use ditto_context::{ContextNode, ContextOrigin, ContextScope, ContextValidationError};
use ditto_event_store::{EventStore, EventStoreError};
use ditto_protocol::{EventActor, EventQuery, EventRecord, event_kind};
use ditto_retrieval::{CandidateCount, RetrievalError};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

/// Version of the durable context-node event payload.
pub const CONTEXT_NODE_EVENT_VERSION: u16 = 1;
/// Version of the independently rebuildable projection schema.
pub const CONTEXT_PROJECTION_SCHEMA_VERSION: i64 = 1;
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

const SCHEMA_V1: &str = r#"
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
) VALUES (1, 1, 0, NULL);

CREATE TABLE projected_nodes (
    session_id   TEXT NOT NULL,
    task_id      TEXT,
    node_id      TEXT NOT NULL,
    event_seq    INTEGER NOT NULL UNIQUE,
    event_id     TEXT NOT NULL UNIQUE,
    node_json    TEXT NOT NULL,
    PRIMARY KEY (session_id, node_id)
);

CREATE INDEX projected_nodes_scope_seq
    ON projected_nodes(session_id, task_id, event_seq);

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
pub struct ContextProjectionSnapshot {
    checkpoint: ProjectionCheckpoint,
    scanned_rows: usize,
    candidates: Vec<ContextNode>,
}

impl ContextProjectionSnapshot {
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
    /// and the requested result limit.
    pub fn candidates(&self) -> &[ContextNode] {
        &self.candidates
    }

    /// Consume the detached snapshot and transfer its candidates without
    /// cloning their summaries or provenance lists.
    pub fn into_candidates(self) -> Vec<ContextNode> {
        self.candidates
    }
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
    #[error("projection singleton checkpoint disappeared during page application")]
    MissingCheckpointDuringPage,
}

/// Separately persisted, rebuildable projection cache.
#[derive(Clone)]
pub struct ContextProjection {
    connection: Arc<Mutex<Connection>>,
    sync_gate: Arc<Mutex<()>>,
    path: Arc<PathBuf>,
}

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
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent)?;
        }

        let mut connection = Connection::open(path)?;
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

        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
            sync_gate: Arc::new(Mutex::new(())),
            path: Arc::new(path.to_path_buf()),
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

    /// Capture the event-spine high-water once and synchronize only through
    /// that stable cutoff.
    pub fn synchronize(
        &self,
        event_store: &EventStore,
    ) -> Result<ProjectionSync, ContextProjectionError> {
        let _gate = self.sync_gate()?;
        let high_water = event_store.latest_seq()?;
        self.synchronize_through_locked(event_store, high_water, false)
    }

    /// Synchronize through a caller-captured event-spine high-water.
    pub fn synchronize_through(
        &self,
        event_store: &EventStore,
        high_water: i64,
    ) -> Result<ProjectionSync, ContextProjectionError> {
        let _gate = self.sync_gate()?;
        self.synchronize_through_locked(event_store, high_water, false)
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
        {
            let mut connection = self.connection()?;
            reset_schema(&mut connection)?;
        }
        self.synchronize_through_locked(event_store, high_water, true)
    }

    /// Synchronize and copy one immutable query scope under the projection's
    /// own gate. A kernel may additionally hold its clone-shared admission gate
    /// across this call to establish its stronger visibility guarantee.
    pub fn synchronize_and_snapshot(
        &self,
        event_store: &EventStore,
        session_id: &str,
        task_id: Option<&str>,
    ) -> Result<ContextProjectionSnapshot, ContextProjectionError> {
        let _gate = self.sync_gate()?;
        let high_water = event_store.latest_seq()?;
        self.synchronize_through_locked(event_store, high_water, false)?;
        self.capture_snapshot_locked(session_id, task_id)
    }

    /// Copy the currently projected session-root plus exact-task namespace.
    /// Every selected row is counted before active/relevance filtering.
    pub fn capture_snapshot(
        &self,
        session_id: &str,
        task_id: Option<&str>,
    ) -> Result<ContextProjectionSnapshot, ContextProjectionError> {
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
        validate_node_identity_shape(&draft.node)?;
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
        let available = event_store.latest_seq()?;
        if high_water < 0 || high_water > available {
            return Err(ContextProjectionError::HighWaterAhead {
                requested: high_water,
                available,
            });
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
            });
        }

        let mut cursor = checkpoint.through_seq;
        while cursor < high_water {
            let page = event_store.list_through(
                &EventQuery {
                    after_seq: Some(cursor),
                    limit: Some(SYNC_PAGE_SIZE),
                    ..EventQuery::default()
                },
                high_water,
            )?;
            let Some(last) = page.last().cloned() else {
                return Err(ContextProjectionError::HighWaterUnreachable { cursor, high_water });
            };
            let mut previous = cursor;
            for event in &page {
                if event.seq <= previous || event.seq > high_water {
                    return Err(ContextProjectionError::NonMonotonicPage {
                        after: previous,
                        found: event.seq,
                    });
                }
                previous = event.seq;
            }

            {
                let mut connection = self.connection()?;
                let transaction = connection.transaction()?;
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
                )?;
                if updated != 1 {
                    return Err(ContextProjectionError::MissingCheckpointDuringPage);
                }
                transaction.commit()?;
            }
            cursor = last.seq;
            checkpoint = ProjectionCheckpoint {
                schema_version: CONTEXT_PROJECTION_SCHEMA_VERSION,
                through_seq: last.seq,
                through_event_id: Some(last.event_id),
            };
        }

        Ok(ProjectionSync {
            captured_high_water: high_water,
            checkpoint,
            rebuilt,
        })
    }

    fn capture_snapshot_locked(
        &self,
        session_id: &str,
        task_id: Option<&str>,
    ) -> Result<ContextProjectionSnapshot, ContextProjectionError> {
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
                node_json: row.get(4)?,
                superseded: row.get(5)?,
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

        Ok(ContextProjectionSnapshot {
            checkpoint,
            scanned_rows,
            candidates,
        })
    }

    fn connection(&self) -> Result<MutexGuard<'_, Connection>, ContextProjectionError> {
        self.connection
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
        "SELECT session_id, task_id, node_id, event_seq, event_id, node_json FROM projected_nodes LIMIT 0",
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
    transaction.execute_batch(SCHEMA_V1)?;
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
            let node_json = serde_json::to_vec(&validated.node).map_err(|error| {
                ContextProjectionError::InvalidNode {
                    node_id: validated.node.id.clone(),
                    reason: error.to_string(),
                }
            })?;
            let outgoing_edge_count = validated.node.supersedes.len();
            let outgoing_edge_digest = canonical_edge_digest(
                &validated.node.id,
                task_key(validated.task_id.as_deref()),
                event.seq,
                &validated.node.supersedes,
            );
            let row = CanonicalAdmissionRow {
                task_id: validated.task_id,
                event_seq: event.seq,
                event_id: event.event_id.clone(),
                node_digest: digest_bytes(&node_json),
                outgoing_edge_digest,
                outgoing_edge_count,
            };
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
) -> Result<(), ContextProjectionError> {
    let validated = decode_recorded_node(event_store, event)?;
    let scope = NodeScope {
        session_id: &validated.session_id,
        task_id: validated.task_id.as_deref(),
    };
    validate_projection_identity_and_supersession(transaction, &validated.node, &scope)?;

    let node_json = serde_json::to_string(&validated.node).map_err(|error| {
        ContextProjectionError::InvalidNode {
            node_id: validated.node.id.clone(),
            reason: error.to_string(),
        }
    })?;
    transaction.execute(
        r#"
        INSERT INTO projected_nodes (
            session_id, task_id, node_id, event_seq, event_id, node_json
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
        "#,
        params![
            scope.session_id,
            scope.task_id,
            &validated.node.id,
            event.seq,
            &event.event_id,
            node_json,
        ],
    )?;
    for superseded_id in &validated.node.supersedes {
        transaction.execute(
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
        )?;
    }
    Ok(())
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

fn validate_projection_identity_and_supersession(
    connection: &Connection,
    node: &ContextNode,
    scope: &NodeScope<'_>,
) -> Result<(), ContextProjectionError> {
    if let Some((event_id, seq)) = find_identity(connection, scope.session_id, &node.id)? {
        return Err(ContextProjectionError::DuplicateNodeIdentity {
            session_id: scope.session_id.to_owned(),
            node_id: node.id.clone(),
            event_id,
            seq,
        });
    }

    for superseded_id in &node.supersedes {
        match find_node_scope(connection, scope.session_id, superseded_id)? {
            None => {
                return Err(ContextProjectionError::MissingSupersededNode {
                    node_id: node.id.clone(),
                    superseded_id: superseded_id.clone(),
                });
            }
            Some(existing_task) if existing_task.as_deref() != scope.task_id => {
                return Err(ContextProjectionError::SupersessionScopeMismatch {
                    node_id: node.id.clone(),
                    superseded_id: superseded_id.clone(),
                });
            }
            Some(_) => {}
        }
    }
    Ok(())
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
    if session_id.trim().is_empty() || task_id.is_some_and(|task| task.trim().is_empty()) {
        return Err(ContextProjectionError::InvalidScope {
            seq: 0,
            reason: "query session/task scope is empty".into(),
        });
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
