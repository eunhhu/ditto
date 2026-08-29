use std::{path::PathBuf, sync::Arc};

use ditto_capability::{CapabilityCard, CapabilityCatalog, CapabilityError};
use ditto_event_store::{EventStore, EventStoreError};
use ditto_protocol::{
    EventActor, EventQuery, EventRecord, NewEvent, event_kind,
};
use thiserror::Error;
use tokio::sync::broadcast;

#[derive(Debug, Clone)]
pub struct KernelConfig {
    pub data_dir: PathBuf,
    pub capabilities_dir: PathBuf,
    pub event_buffer: usize,
}

impl KernelConfig {
    pub fn new(data_dir: impl Into<PathBuf>, capabilities_dir: impl Into<PathBuf>) -> Self {
        Self {
            data_dir: data_dir.into(),
            capabilities_dir: capabilities_dir.into(),
            event_buffer: 1_024,
        }
    }
}

#[derive(Debug, Error)]
pub enum KernelError {
    #[error(transparent)]
    EventStore(#[from] EventStoreError),
    #[error(transparent)]
    Capability(#[from] CapabilityError),
}

#[derive(Clone)]
pub struct DittoKernel {
    inner: Arc<KernelInner>,
}

struct KernelInner {
    events: EventStore,
    capabilities: CapabilityCatalog,
    event_sender: broadcast::Sender<EventRecord>,
}

impl DittoKernel {
    pub fn open(config: KernelConfig) -> Result<Self, KernelError> {
        let events = EventStore::open(config.data_dir.join("state.db"))?;
        let capabilities = CapabilityCatalog::load(config.capabilities_dir)?;
        let (event_sender, _) = broadcast::channel(config.event_buffer.max(16));

        Ok(Self {
            inner: Arc::new(KernelInner {
                events,
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
}
