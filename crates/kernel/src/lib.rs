use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

pub use ditto_artifact_store::{ArtifactMetadata, ArtifactRef};
use ditto_artifact_store::{ArtifactStore, ArtifactStoreError, DEFAULT_MAX_OBJECT_BYTES};
use ditto_capability::{CapabilityCard, CapabilityCatalog, CapabilityError};
pub use ditto_capability::{ExecutionEpoch, SearchContext};
use ditto_context_projection::{ContextProjection, ContextProjectionError};
use ditto_event_store::{EventStore, EventStoreError};
use ditto_protocol::{
    EventActor, EventQuery, EventRecord, NewEvent, SubmitInputCommand, event_kind,
};
use ditto_retrieval::EmbeddingProvider;
use serde_json::json;
use thiserror::Error;
use tokio::sync::broadcast;
use ulid::Ulid;

mod context_admission;
mod context_retrieval;
pub mod turn;

pub use context_admission::{
    COMMITTED_BUT_PROJECTION_UNAVAILABLE, ContextProjectionUnavailable, TrustedContextNodeDraft,
};
pub use context_retrieval::{
    WorkingSet, WorkingSetError, WorkingSetRequest, WorkingSetRetrievalSummary,
};

pub use turn::{
    ArtifactReadTurnOutcome, ArtifactReadTurnReplay, ArtifactReadTurnStatus,
    CapabilitiesSelectedPayload, CapabilityRequestedPayload, ContextCompiledPayload,
    ExecutionOutputPayload, ExecutionStartedPayload, ModelOutputPayload, ModelRequestedPayload,
    ReadOnlyTurnControl, ReplayError, ReplayedArtifactReadCall, ReplayedReadOnlyTurn,
    TurnFailedPayload, TurnFailure, TurnFailureCode, TurnFinishedPayload, TurnRunError,
    TurnSequenceSpan, replay_artifact_read_turn,
};

const MAX_INPUT_BYTES: usize = 64 * 1024;
const MAX_IDENTIFIER_BYTES: usize = 256;

#[derive(Debug, Clone)]
pub struct KernelConfig {
    pub data_dir: PathBuf,
    pub capabilities_dir: PathBuf,
    pub event_buffer: usize,
    pub artifact_max_object_bytes: u64,
}

impl KernelConfig {
    pub fn new(data_dir: impl Into<PathBuf>, capabilities_dir: impl Into<PathBuf>) -> Self {
        Self {
            data_dir: data_dir.into(),
            capabilities_dir: capabilities_dir.into(),
            event_buffer: 1_024,
            artifact_max_object_bytes: DEFAULT_MAX_OBJECT_BYTES,
        }
    }
}

#[derive(Debug, Error)]
pub enum KernelError {
    #[error(transparent)]
    EventStore(#[from] EventStoreError),
    #[error(transparent)]
    Capability(#[from] CapabilityError),
    #[error(transparent)]
    ArtifactStore(#[from] ArtifactStoreError),
    #[error(transparent)]
    ContextProjection(#[from] ContextProjectionError),
    #[error("context admission gate mutex was poisoned")]
    ContextAdmissionGatePoisoned,
    #[error(
        "duplicate_context_node_identity: context node identity ({session_id}, {node_id}) was already committed in event {event_id} at sequence {event_seq}"
    )]
    DuplicateContextNodeIdentity {
        session_id: String,
        node_id: String,
        event_id: String,
        event_seq: i64,
    },
    #[error(
        "committed_but_projection_unavailable: the durable context event was committed but its projection is unavailable: {source}"
    )]
    CommittedButProjectionUnavailable {
        event: Box<EventRecord>,
        #[source]
        source: ContextProjectionUnavailable,
    },
    #[error("trusted context payload serialization failed: {0}")]
    ContextPayloadSerialization(#[from] serde_json::Error),
    #[error("invalid command: {0}")]
    InvalidCommand(String),
}

impl KernelError {
    /// Stable machine outcome for the one accepted-but-not-yet-projected case.
    pub const fn outcome_code(&self) -> Option<&'static str> {
        match self {
            Self::CommittedButProjectionUnavailable { .. } => {
                Some(COMMITTED_BUT_PROJECTION_UNAVAILABLE)
            }
            _ => None,
        }
    }
}

#[derive(Clone)]
pub struct DittoKernel {
    inner: Arc<KernelInner>,
}

struct KernelInner {
    events: EventStore,
    artifacts: ArtifactStore,
    capabilities: CapabilityCatalog,
    context_projection: ContextProjection,
    context_admission_gate: Mutex<()>,
    embedding_provider: Option<Arc<dyn EmbeddingProvider>>,
    event_sender: broadcast::Sender<EventRecord>,
}

#[derive(Debug, Clone, Default)]
pub struct ArtifactWriteContext {
    pub session_id: Option<String>,
    pub task_id: Option<String>,
    pub producer_event_id: Option<String>,
    pub mime: Option<String>,
    pub purpose: Option<String>,
}

#[derive(Debug, Clone)]
pub struct StoredArtifact {
    pub metadata: ArtifactMetadata,
    pub event: EventRecord,
}

impl DittoKernel {
    pub fn open(config: KernelConfig) -> Result<Self, KernelError> {
        Self::open_with_provider(config, None)
    }

    /// Open a kernel with an explicitly injected embedding provider.
    ///
    /// Production callers should use [`DittoKernel::open`], which is
    /// intentionally lexical-only. This constructor is for tests and local
    /// composition that have a provider they own.
    pub fn open_with_embedding_provider(
        config: KernelConfig,
        provider: Arc<dyn EmbeddingProvider>,
    ) -> Result<Self, KernelError> {
        Self::open_with_provider(config, Some(provider))
    }

    fn open_with_provider(
        config: KernelConfig,
        embedding_provider: Option<Arc<dyn EmbeddingProvider>>,
    ) -> Result<Self, KernelError> {
        let events = EventStore::open(config.data_dir.join("state.db"))?;
        let context_projection = ContextProjection::open_in(&config.data_dir)?;
        context_projection.synchronize(&events)?;
        let artifacts = ArtifactStore::with_max_object_bytes(
            config.data_dir.join("artifacts"),
            config.artifact_max_object_bytes,
        )?;
        let capabilities = CapabilityCatalog::load(config.capabilities_dir)?;
        let (event_sender, _) = broadcast::channel(config.event_buffer.max(16));

        Ok(Self {
            inner: Arc::new(KernelInner {
                events,
                artifacts,
                capabilities,
                context_projection,
                context_admission_gate: Mutex::new(()),
                embedding_provider,
                event_sender,
            }),
        })
    }

    pub fn record_runtime_started(&self, bind: &str) -> Result<EventRecord, KernelError> {
        self.append_and_publish(NewEvent {
            session_id: None,
            task_id: None,
            actor: EventActor::System,
            kind: event_kind::RUNTIME_STARTED.into(),
            payload: json!({
                "version": env!("CARGO_PKG_VERSION"),
                "bind": bind,
                "capability_count": self.inner.capabilities.len(),
            }),
            causation_id: None,
            correlation_id: None,
            span_id: None,
        })
    }

    /// Converts a narrow public command into a trusted user event.
    pub fn record_user_input(
        &self,
        command: SubmitInputCommand,
    ) -> Result<EventRecord, KernelError> {
        let text = normalize_input_text(&command.text)?;

        let session_id = normalize_identifier(command.session_id, "session")?;
        let task_id = command
            .task_id
            .map(|task_id| validate_identifier(task_id, "task"))
            .transpose()?;
        let mut event = NewEvent::user_input(session_id, task_id, text);
        event.correlation_id = event.task_id.clone();
        self.append_and_publish(event)
    }

    pub fn list_events(&self, query: &EventQuery) -> Result<Vec<EventRecord>, KernelError> {
        Ok(self.inner.events.list(query)?)
    }

    pub fn list_events_through(
        &self,
        query: &EventQuery,
        through_seq: i64,
    ) -> Result<Vec<EventRecord>, KernelError> {
        Ok(self.inner.events.list_through(query, through_seq)?)
    }

    pub fn latest_event_seq(&self) -> Result<i64, KernelError> {
        Ok(self.inner.events.latest_seq()?)
    }

    pub fn event_count(&self) -> Result<u64, KernelError> {
        Ok(self.inner.events.count()?)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<EventRecord> {
        self.inner.event_sender.subscribe()
    }

    pub fn capability_cards(&self) -> Vec<CapabilityCard> {
        self.inner.capabilities.cards()
    }

    pub fn search_capabilities(&self, query: &str, limit: usize) -> Vec<CapabilityCard> {
        self.inner.capabilities.search(query, limit.clamp(1, 100))
    }

    pub fn build_execution_epoch(
        &self,
        query: &str,
        context: &SearchContext,
        max_working_set: usize,
    ) -> ExecutionEpoch {
        let mut epoch = ExecutionEpoch::new(max_working_set);
        let cards = self
            .inner
            .capabilities
            .search_with_context(query, context, max_working_set);
        epoch.page_in(cards);
        epoch
    }

    pub fn page_execution_epoch(
        &self,
        epoch: &mut ExecutionEpoch,
        query: &str,
        context: &SearchContext,
    ) -> usize {
        let cards =
            self.inner
                .capabilities
                .search_with_context(query, context, epoch.remaining_capacity());
        epoch.page_in(cards)
    }

    /// Stores content, then roots its task-specific meaning in the event spine.
    pub fn store_artifact(
        &self,
        bytes: &[u8],
        context: ArtifactWriteContext,
    ) -> Result<StoredArtifact, KernelError> {
        let metadata = self.inner.artifacts.put(bytes)?;
        let event = self.append_and_publish(NewEvent {
            session_id: context.session_id,
            task_id: context.task_id,
            actor: EventActor::System,
            kind: event_kind::ARTIFACT_CREATED.into(),
            payload: json!({
                "reference": metadata.reference.clone(),
                "bytes": metadata.bytes,
                "first_seen_at": metadata.first_seen_at,
                "mime": context.mime,
                "purpose": context.purpose,
            }),
            causation_id: context.producer_event_id,
            correlation_id: None,
            span_id: None,
        })?;
        Ok(StoredArtifact { metadata, event })
    }

    pub fn read_artifact(&self, reference: &ArtifactRef) -> Result<Vec<u8>, KernelError> {
        Ok(self.inner.artifacts.get(reference)?)
    }

    pub fn read_artifact_range(
        &self,
        reference: &ArtifactRef,
        offset: u64,
        length: usize,
    ) -> Result<Vec<u8>, KernelError> {
        Ok(self.inner.artifacts.read_range(reference, offset, length)?)
    }

    pub fn artifact_metadata(
        &self,
        reference: &ArtifactRef,
    ) -> Result<ArtifactMetadata, KernelError> {
        Ok(self.inner.artifacts.metadata(reference)?)
    }

    fn append_and_publish(&self, event: NewEvent) -> Result<EventRecord, KernelError> {
        let event = self.append_without_publish(event)?;
        self.publish(&event);
        Ok(event)
    }

    fn append_without_publish(&self, event: NewEvent) -> Result<EventRecord, KernelError> {
        Ok(self.inner.events.append(event)?)
    }

    fn publish(&self, event: &EventRecord) {
        let _ = self.inner.event_sender.send(event.clone());
    }
}

fn normalize_input_text(value: &str) -> Result<String, KernelError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(KernelError::InvalidCommand("input text is empty".into()));
    }
    if value.len() > MAX_INPUT_BYTES {
        return Err(KernelError::InvalidCommand(format!(
            "input exceeds {MAX_INPUT_BYTES} bytes"
        )));
    }
    Ok(value.to_owned())
}

fn normalize_identifier(value: Option<String>, prefix: &str) -> Result<String, KernelError> {
    match value {
        Some(value) => validate_identifier(value, prefix),
        None => Ok(format!("{prefix}_{}", Ulid::new())),
    }
}

fn validate_identifier(value: String, label: &str) -> Result<String, KernelError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(KernelError::InvalidCommand(format!("{label} id is empty")));
    }
    if value.len() > MAX_IDENTIFIER_BYTES {
        return Err(KernelError::InvalidCommand(format!(
            "{label} id exceeds {MAX_IDENTIFIER_BYTES} bytes"
        )));
    }
    if value.chars().any(char::is_control) {
        return Err(KernelError::InvalidCommand(format!(
            "{label} id contains control characters"
        )));
    }
    Ok(value.to_owned())
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::{ArtifactWriteContext, DittoKernel, KernelConfig};
    use ditto_protocol::{EventActor, EventQuery, SubmitInputCommand, event_kind};

    fn kernel() -> (tempfile::TempDir, DittoKernel) {
        let directory = tempdir().expect("temporary directory");
        let kernel = DittoKernel::open(KernelConfig::new(
            directory.path().join("data"),
            directory.path().join("capabilities"),
        ))
        .expect("open kernel");
        (directory, kernel)
    }

    #[test]
    fn public_input_cannot_choose_an_actor_or_event_kind() {
        let (_directory, kernel) = kernel();
        let event = kernel
            .record_user_input(SubmitInputCommand {
                text: "hello".into(),
                session_id: Some("local".into()),
                task_id: Some("task-1".into()),
            })
            .expect("record input");
        assert_eq!(event.actor, EventActor::User);
        assert_eq!(event.kind, event_kind::INPUT_RECEIVED);
    }

    #[test]
    fn artifact_occurrence_is_rooted_in_the_event_spine() {
        let (_directory, kernel) = kernel();
        let producer = kernel
            .record_user_input(SubmitInputCommand {
                text: "produce output".into(),
                session_id: Some("local".into()),
                task_id: Some("task-1".into()),
            })
            .expect("record input");
        let stored = kernel
            .store_artifact(
                b"verified output",
                ArtifactWriteContext {
                    session_id: producer.session_id.clone(),
                    task_id: producer.task_id.clone(),
                    producer_event_id: Some(producer.event_id.clone()),
                    mime: Some("text/plain".into()),
                    purpose: Some("test output".into()),
                },
            )
            .expect("store artifact");

        assert_eq!(stored.event.kind, event_kind::ARTIFACT_CREATED);
        assert_eq!(stored.event.causation_id, Some(producer.event_id));
        assert_eq!(
            kernel
                .read_artifact_range(&stored.metadata.reference, 9, 6)
                .expect("read artifact range"),
            b"output"
        );
        assert_eq!(
            kernel
                .list_events(&EventQuery {
                    task_id: Some("task-1".into()),
                    ..EventQuery::default()
                })
                .expect("list events")
                .len(),
            2
        );
    }
}
