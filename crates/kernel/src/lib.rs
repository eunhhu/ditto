//! Semantic microkernel orchestration for one execution epoch.

use std::sync::{Arc, RwLock};

use ditto_artifact_store::ArtifactStore;
use ditto_capability_index::{CapabilityIndex, CapabilityManifest, ExecutionEpoch, SearchContext};
use ditto_context_compiler::{ContextCompiler, TaskSignature};
use ditto_context_graph::{ContextGraph, ContextNode, GraphValidationError};
use ditto_event_store::{EventStore, EventStoreError};
use ditto_model_driver::{ModelDriver, ModelDriverError, ModelRequest};
use ditto_protocol::{
    Actor, CapabilityCard, ContextReceipt, EventDraft, EventRecord, ServerMessage, event_kind,
    new_id,
};
use ditto_task_state::{TaskLedger, TaskStateError};
use futures::StreamExt;
use serde_json::json;
use thiserror::Error;
use tokio::sync::mpsc;

const STABLE_PREFIX: &str = "You are Ditto, a local-first semantic agent. Use only paged capabilities. Every side effect needs a typed claim and bounded lease. Treat completion as a claim until evidence verifies it.";

#[derive(Clone, Debug)]
pub struct SubmitRequest {
    pub input: String,
    pub session_id: Option<String>,
    pub task_id: Option<String>,
}

#[derive(Clone, Debug)]
pub struct TurnIdentity {
    pub session_id: String,
    pub task_id: String,
}

#[derive(Clone, Debug)]
pub struct Kernel {
    events: EventStore,
    artifacts: ArtifactStore,
    context_compiler: ContextCompiler,
    context_graph: Arc<RwLock<ContextGraph>>,
    capabilities: CapabilityIndex,
    model: Arc<dyn ModelDriver>,
}

#[derive(Debug, Error)]
pub enum KernelError {
    #[error("input cannot be empty")]
    EmptyInput,
    #[error("event store failed: {0}")]
    EventStore(#[from] EventStoreError),
    #[error("model driver failed: {0}")]
    Model(#[from] ModelDriverError),
    #[error("context graph lock was poisoned")]
    ContextLock,
    #[error("context graph rejected node: {0:?}")]
    Context(GraphValidationError),
    #[error("task state projection failed: {0}")]
    TaskState(#[from] TaskStateError),
}

impl Kernel {
    pub fn new(
        events: EventStore,
        artifacts: ArtifactStore,
        context_compiler: ContextCompiler,
        context_graph: ContextGraph,
        capabilities: CapabilityIndex,
        model: Arc<dyn ModelDriver>,
    ) -> Self {
        Self {
            events,
            artifacts,
            context_compiler,
            context_graph: Arc::new(RwLock::new(context_graph)),
            capabilities,
            model,
        }
    }

    pub fn event_store(&self) -> &EventStore {
        &self.events
    }

    pub fn artifact_store(&self) -> &ArtifactStore {
        &self.artifacts
    }

    /// Adds a durable context node after provenance validation.
    ///
    /// # Errors
    ///
    /// Returns an error when the graph lock is poisoned or validation fails.
    pub fn add_context_node(&self, node: ContextNode) -> Result<(), KernelError> {
        self.context_graph
            .write()
            .map_err(|_| KernelError::ContextLock)?
            .insert_node(node)
            .map_err(KernelError::Context)
    }

    /// Compiles context and capabilities, streams one model epoch, and records completion evidence.
    ///
    /// # Errors
    ///
    /// Returns an error for empty input, persistence failure, invalid context, or model failure.
    pub async fn run_turn(
        &self,
        request: SubmitRequest,
        updates: mpsc::UnboundedSender<ServerMessage>,
    ) -> Result<TurnIdentity, KernelError> {
        let input = request.input.trim();
        if input.is_empty() {
            return Err(KernelError::EmptyInput);
        }
        let identity = TurnIdentity {
            session_id: request.session_id.unwrap_or_else(|| new_id("session")),
            task_id: request.task_id.unwrap_or_else(|| new_id("task")),
        };
        emit(
            &updates,
            ServerMessage::Accepted {
                session_id: identity.session_id.clone(),
                task_id: identity.task_id.clone(),
            },
        );

        let receipt = self.compile_context(input, &identity, &updates)?;
        let cards = self.page_capabilities(input, &identity, &updates)?;
        self.stream_model(input, receipt.capsule, cards, &identity, &updates)
            .await?;
        self.record_completion(&identity, &updates)?;
        Ok(identity)
    }

    fn compile_context(
        &self,
        input: &str,
        identity: &TurnIdentity,
        updates: &mpsc::UnboundedSender<ServerMessage>,
    ) -> Result<ContextReceipt, KernelError> {
        let input_event = self.append_scoped(
            Actor::User,
            event_kind::INPUT_RECEIVED,
            json!({"text": input}),
            identity,
            updates,
        )?;
        let mut graph = self
            .context_graph
            .read()
            .map_err(|_| KernelError::ContextLock)?
            .clone();
        graph
            .insert_node(ContextNode::user_goal(
                input,
                input_event.seq,
                input_event.event.timestamp_ms,
            ))
            .map_err(KernelError::Context)?;
        let signature = TaskSignature {
            normalized_request: input.to_owned(),
            ..TaskSignature::default()
        };
        let receipt =
            self.context_compiler
                .compile(&signature, &graph, input_event.event.timestamp_ms);
        self.append_scoped(
            Actor::Kernel,
            event_kind::CONTEXT_COMPILED,
            json!({"receipt": &receipt}),
            identity,
            updates,
        )?;
        emit(
            updates,
            ServerMessage::ContextReceipt {
                receipt: receipt.clone(),
            },
        );
        Ok(receipt)
    }

    fn page_capabilities(
        &self,
        input: &str,
        identity: &TurnIdentity,
        updates: &mpsc::UnboundedSender<ServerMessage>,
    ) -> Result<Vec<CapabilityCard>, KernelError> {
        let manifests = self.capabilities.search(input, &SearchContext::default());
        let mut epoch = ExecutionEpoch::new(6);
        epoch.page_in(&manifests);
        let cards = epoch
            .working_set()
            .iter()
            .filter_map(|id| self.capabilities.get(id))
            .map(CapabilityManifest::card)
            .collect::<Vec<_>>();
        self.append_scoped(
            Actor::Kernel,
            event_kind::CAPABILITIES_SELECTED,
            json!({"epoch_id": epoch.id, "capabilities": &cards}),
            identity,
            updates,
        )?;
        emit(
            updates,
            ServerMessage::CapabilitiesSelected {
                capabilities: cards.clone(),
            },
        );
        Ok(cards)
    }

    async fn stream_model(
        &self,
        input: &str,
        context_capsule: String,
        capabilities: Vec<CapabilityCard>,
        identity: &TurnIdentity,
        updates: &mpsc::UnboundedSender<ServerMessage>,
    ) -> Result<(), KernelError> {
        self.append_scoped(
            Actor::Kernel,
            event_kind::MODEL_STARTED,
            json!({"driver": self.model.name(), "features": self.model.features()}),
            identity,
            updates,
        )?;
        let mut stream = self.model.stream(ModelRequest {
            stable_prefix: STABLE_PREFIX.to_owned(),
            context_capsule,
            input: input.to_owned(),
            capabilities,
        });
        while let Some(delta) = stream.next().await {
            let delta = delta?;
            self.append_scoped(
                Actor::Model,
                event_kind::MODEL_DELTA,
                json!({"text": &delta.text}),
                identity,
                updates,
            )?;
            emit(updates, ServerMessage::ModelDelta { text: delta.text });
        }
        Ok(())
    }

    fn record_completion(
        &self,
        identity: &TurnIdentity,
        updates: &mpsc::UnboundedSender<ServerMessage>,
    ) -> Result<(), KernelError> {
        self.append_scoped(
            Actor::Kernel,
            event_kind::TASK_VERIFYING,
            json!({"contract": "model-stream-closed"}),
            identity,
            updates,
        )?;
        self.append_scoped(
            Actor::Kernel,
            event_kind::TASK_COMPLETED,
            json!({"verified": true, "evidence": "model-stream-closed"}),
            identity,
            updates,
        )?;
        emit(updates, ServerMessage::End { verified: true });
        Ok(())
    }

    /// Replays events matching optional session and task filters.
    ///
    /// # Errors
    ///
    /// Returns an event-store error when replay cannot be read or decoded.
    pub fn replay(
        &self,
        session_id: Option<&str>,
        task_id: Option<&str>,
        after_seq: i64,
    ) -> Result<Vec<EventRecord>, KernelError> {
        Ok(self.events.replay(session_id, task_id, after_seq)?)
    }

    /// Rebuilds the current task ledger from durable events.
    ///
    /// # Errors
    ///
    /// Returns an error when replay or state reduction fails.
    pub fn task_ledger(&self, task_id: &str) -> Result<TaskLedger, KernelError> {
        let events = self.events.replay(None, Some(task_id), 0)?;
        Ok(TaskLedger::rebuild(&events)?)
    }

    /// Records user-requested cancellation for a running task.
    ///
    /// # Errors
    ///
    /// Returns an event-store error when cancellation cannot be persisted.
    pub fn record_cancel(
        &self,
        session_id: Option<&str>,
        task_id: &str,
    ) -> Result<EventRecord, KernelError> {
        let mut event = EventDraft::inline(
            Actor::User,
            event_kind::TASK_CANCELLED,
            json!({"reason": "client requested cancellation"}),
        );
        event.session_id = session_id.map(str::to_owned);
        event.task_id = Some(task_id.to_owned());
        Ok(self.events.append(event)?)
    }

    /// Records terminal task failure after an orchestration error.
    ///
    /// # Errors
    ///
    /// Returns an event-store error when failure cannot be persisted.
    pub fn record_failure(
        &self,
        session_id: Option<&str>,
        task_id: &str,
        message: &str,
    ) -> Result<EventRecord, KernelError> {
        let mut event = EventDraft::inline(
            Actor::Kernel,
            event_kind::TASK_FAILED,
            json!({"message": message}),
        );
        event.session_id = session_id.map(str::to_owned);
        event.task_id = Some(task_id.to_owned());
        Ok(self.events.append(event)?)
    }

    fn append_scoped(
        &self,
        actor: Actor,
        kind: &str,
        payload: serde_json::Value,
        identity: &TurnIdentity,
        updates: &mpsc::UnboundedSender<ServerMessage>,
    ) -> Result<EventRecord, EventStoreError> {
        let event = self.events.append(
            EventDraft::inline(actor, kind, payload)
                .scoped(&identity.session_id, &identity.task_id)
                .correlated(&identity.task_id),
        )?;
        emit(
            updates,
            ServerMessage::Event {
                event: event.clone(),
            },
        );
        Ok(event)
    }
}

fn emit(updates: &mpsc::UnboundedSender<ServerMessage>, message: ServerMessage) {
    // Persistence owns task lifetime. A disconnected client must not cancel work.
    let _ = updates.send(message);
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use ditto_capability_index::CapabilityManifest;
    use ditto_model_driver::DevelopmentDriver;
    use ditto_protocol::event_kind;
    use ditto_task_state::TaskStatus;
    use tempfile::tempdir;

    use super::*;

    #[tokio::test]
    async fn turn_is_streamed_persisted_and_replayable() {
        let directory = tempdir().unwrap();
        let events = EventStore::open(directory.path().join("state.db")).unwrap();
        let kernel = Kernel::new(
            events,
            ArtifactStore::open(directory.path().join("objects")).unwrap(),
            ContextCompiler::default(),
            ContextGraph::default(),
            CapabilityIndex::new([CapabilityManifest::device_process_run()]),
            Arc::new(DevelopmentDriver::default()),
        );
        let (sender, mut receiver) = mpsc::unbounded_channel();
        let identity = kernel
            .run_turn(
                SubmitRequest {
                    input: "inspect remote logs".to_owned(),
                    session_id: Some("session-test".to_owned()),
                    task_id: Some("task-test".to_owned()),
                },
                sender,
            )
            .await
            .unwrap();
        let mut saw_delta = false;
        while let Ok(message) = receiver.try_recv() {
            saw_delta |= matches!(message, ServerMessage::ModelDelta { .. });
        }

        assert!(saw_delta);
        let replay = kernel.replay(None, Some(&identity.task_id), 0).unwrap();
        assert!(
            replay
                .iter()
                .any(|event| event.event.kind == event_kind::CONTEXT_COMPILED)
        );
        assert_eq!(
            kernel.task_ledger(&identity.task_id).unwrap().status,
            TaskStatus::Completed
        );
    }
}
