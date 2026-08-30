use std::{path::PathBuf, sync::Arc};

pub use ditto_artifact_store::{ArtifactMetadata, ArtifactRef, PutOptions};
use ditto_artifact_store::{ArtifactStore, ArtifactStoreError, DEFAULT_MAX_OBJECT_BYTES};
use ditto_capability::{CapabilityCard, CapabilityCatalog, CapabilityError};
pub use ditto_capability::{ExecutionEpoch, SearchContext};
use ditto_event_store::{EventStore, EventStoreError};
use ditto_protocol::{EventActor, EventQuery, EventRecord, NewEvent, event_kind};
use thiserror::Error;
use tokio::sync::broadcast;

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
}

#[derive(Clone)]
pub struct DittoKernel {
    inner: Arc<KernelInner>,
}

struct KernelInner {
    events: EventStore,
    artifacts: ArtifactStore,
    capabilities: CapabilityCatalog,
    event_sender: broadcast::Sender<EventRecord>,
}

impl DittoKernel {
    pub fn open(config: KernelConfig) -> Result<Self, KernelError> {
        let events = EventStore::open(config.data_dir.join("state.db"))?;
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
                event_sender,
            }),
        })
    }

    pub fn append_event(&self, event: NewEvent) -> Result<EventRecord, KernelError> {
        let event = self.inner.events.append(event)?;
        let _ = self.inner.event_sender.send(event.clone());
        Ok(event)
    }

    pub fn record_runtime_started(&self, bind: &str) -> Result<EventRecord, KernelError> {
        self.append_event(NewEvent {
            session_id: None,
            task_id: None,
            actor: EventActor::System,
            kind: event_kind::RUNTIME_STARTED.into(),
            payload: serde_json::json!({
                "version": env!("CARGO_PKG_VERSION"),
                "bind": bind,
                "capability_count": self.inner.capabilities.len(),
            }),
            causation_id: None,
            correlation_id: None,
            span_id: None,
        })
    }

    pub fn list_events(&self, query: &EventQuery) -> Result<Vec<EventRecord>, KernelError> {
        Ok(self.inner.events.list(query)?)
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
                .search_with_context(query, context, epoch.max_working_set());
        epoch.page_in(cards)
    }

    pub fn store_artifact(
        &self,
        bytes: &[u8],
        options: PutOptions,
    ) -> Result<ArtifactMetadata, KernelError> {
        Ok(self.inner.artifacts.put(bytes, options)?)
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
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::{DittoKernel, KernelConfig, PutOptions};

    #[test]
    fn stores_content_addressed_artifacts_through_the_kernel() {
        let directory = tempdir().expect("temporary directory");
        let kernel = DittoKernel::open(KernelConfig::new(
            directory.path().join("data"),
            directory.path().join("capabilities"),
        ))
        .expect("open kernel");
        let metadata = kernel
            .store_artifact(b"verified output", PutOptions::default())
            .expect("store artifact");

        assert_eq!(
            kernel
                .read_artifact_range(&metadata.reference, 9, 6)
                .expect("read artifact range"),
            b"output"
        );
    }
}
