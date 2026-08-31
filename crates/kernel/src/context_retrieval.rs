use chrono::{DateTime, Utc};
use ditto_capability::{CapabilitySearchError, ExecutionEpoch, SearchContext};
use ditto_context::{
    CompiledContext, CompiledContextValidationError, ContextCapsule, ContextCompileError,
    ContextCompiler, ContextQueryRanking, ContextQueryRankingError,
};
use ditto_context_projection::{ContextProjectionError, ProjectionCheckpoint};
use ditto_event_store::EventStoreError;
use ditto_retrieval::{
    CapabilityRootLimit, ContextResultLimit, EmbeddingDescriptor, ExecutionEpochLimit,
    RetrievalError, RetrievalMode, TaskQuery, TaskSignatureV2,
};
use thiserror::Error;

use super::DittoKernel;

/// Raw inputs for one all-or-nothing context and capability working set.
///
/// The three limits deliberately remain raw so the operation can enforce its
/// fixed context/root/epoch validation precedence before query or provider
/// work. The signature is normalized and validated when the shared
/// [`TaskQuery`] is built.
#[derive(Debug, Clone)]
pub struct WorkingSetRequest {
    pub session_id: String,
    pub task_id: Option<String>,
    pub signature: TaskSignatureV2,
    pub context_token_budget: Option<u32>,
    pub context_result_limit: usize,
    pub capability_root_limit: usize,
    pub execution_epoch_limit: usize,
    pub capability_search: SearchContext,
}

/// Safe retrieval metadata projected from the one shared task query.
///
/// The embedding vector stays private to `TaskQuery`; only its validated
/// descriptor is retained when embedded retrieval was explicitly configured.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkingSetRetrievalSummary {
    mode: RetrievalMode,
    embedding_descriptor: Option<EmbeddingDescriptor>,
}

impl WorkingSetRetrievalSummary {
    pub const fn mode(&self) -> RetrievalMode {
        self.mode
    }

    pub fn embedding_descriptor(&self) -> Option<&EmbeddingDescriptor> {
        self.embedding_descriptor.as_ref()
    }
}

/// One complete read-only working set from a single projection snapshot.
///
/// The context receipt remains owned by `compiled_context`; it is not
/// duplicated here. An epoch ID is a fresh issuance identity and is not a
/// replay-stable digest of these results.
#[derive(Debug, Clone)]
pub struct WorkingSet {
    retrieval: WorkingSetRetrievalSummary,
    projection_checkpoint: ProjectionCheckpoint,
    evaluated_at: DateTime<Utc>,
    compiled_context: CompiledContext,
    context_capsule: ContextCapsule,
    execution_epoch: ExecutionEpoch,
}

impl WorkingSet {
    pub fn retrieval(&self) -> &WorkingSetRetrievalSummary {
        &self.retrieval
    }

    pub fn projection_checkpoint(&self) -> &ProjectionCheckpoint {
        &self.projection_checkpoint
    }

    pub fn evaluated_at(&self) -> &DateTime<Utc> {
        &self.evaluated_at
    }

    pub fn compiled_context(&self) -> &CompiledContext {
        &self.compiled_context
    }

    pub fn context_capsule(&self) -> &ContextCapsule {
        &self.context_capsule
    }

    pub fn execution_epoch(&self) -> &ExecutionEpoch {
        &self.execution_epoch
    }
}

/// Failures from the all-or-nothing joint working-set operation.
#[derive(Debug, Error)]
pub enum WorkingSetError {
    #[error("invalid context-result limit: {0}")]
    ContextResultLimit(#[source] RetrievalError),
    #[error("invalid capability-root limit: {0}")]
    CapabilityRootLimit(#[source] RetrievalError),
    #[error("invalid execution-epoch limit: {0}")]
    ExecutionEpochLimit(#[source] RetrievalError),
    #[error("shared task query is invalid: {0}")]
    Query(#[source] RetrievalError),
    #[error("context admission/snapshot gate mutex was poisoned")]
    SharedGatePoisoned,
    #[error(transparent)]
    EventStore(#[from] EventStoreError),
    #[error(transparent)]
    ContextProjection(#[from] ContextProjectionError),
    #[error(transparent)]
    ContextRanking(#[from] ContextQueryRankingError),
    #[error(transparent)]
    ContextCompile(#[from] ContextCompileError),
    #[error(transparent)]
    ContextValidation(#[from] CompiledContextValidationError),
    #[error(transparent)]
    CapabilitySearch(#[from] CapabilitySearchError),
}

impl DittoKernel {
    /// Retrieve one bounded context/capability working set without appending an
    /// event, invoking a model, or persisting query or embedding state.
    pub fn retrieve_working_set(
        &self,
        request: WorkingSetRequest,
    ) -> Result<WorkingSet, WorkingSetError> {
        // Raw operational limits have a public, fixed failure precedence and
        // must fail before query/provider work or projection synchronization.
        let context_result_limit = ContextResultLimit::new(request.context_result_limit)
            .map_err(WorkingSetError::ContextResultLimit)?;
        let capability_root_limit = CapabilityRootLimit::new(request.capability_root_limit)
            .map_err(WorkingSetError::CapabilityRootLimit)?;
        let execution_epoch_limit = ExecutionEpochLimit::new(request.execution_epoch_limit)
            .map_err(WorkingSetError::ExecutionEpochLimit)?;

        let provider = self.inner.embedding_provider.as_deref();
        let query = TaskQuery::with_provider(request.signature, provider)
            .map_err(WorkingSetError::Query)?;

        // The clone-shared admission gate covers only the stable high-water,
        // projection synchronization, detached snapshot, and single wall-clock
        // capture. Potentially slow embedding and capability work runs after
        // the guard is dropped.
        let (snapshot, evaluated_at) = {
            let _gate = self
                .inner
                .context_admission_gate
                .lock()
                .map_err(|_| WorkingSetError::SharedGatePoisoned)?;
            let high_water = self.inner.events.latest_seq()?;
            let snapshot = self
                .inner
                .context_projection
                .synchronize_and_verified_snapshot_through(
                    &self.inner.events,
                    high_water,
                    &request.session_id,
                    request.task_id.as_deref(),
                )?;
            let evaluated_at = Utc::now();
            (snapshot, evaluated_at)
        };

        let projection_checkpoint = snapshot.checkpoint().clone();
        let ranking = ContextQueryRanking::new(
            &query,
            snapshot.into_candidates(),
            evaluated_at,
            context_result_limit,
            provider,
        )?;
        let compiler = ContextCompiler::default();
        let compiled_context =
            compiler.compile_ranked_query(&ranking, request.context_token_budget)?;
        let context_capsule = ContextCapsule::from(&compiled_context);
        compiler.validate_compiled_ranked_query(
            &ranking,
            &compiled_context,
            &context_capsule,
            request.context_token_budget,
        )?;

        let cards = self.inner.capabilities.search_task_query(
            &query,
            &request.capability_search,
            capability_root_limit,
            execution_epoch_limit,
            provider,
        )?;
        let mut execution_epoch = ExecutionEpoch::new(execution_epoch_limit.get());
        execution_epoch.page_in(cards);

        Ok(WorkingSet {
            retrieval: WorkingSetRetrievalSummary {
                mode: query.mode(),
                embedding_descriptor: query.embedding_descriptor().cloned(),
            },
            projection_checkpoint,
            evaluated_at,
            compiled_context,
            context_capsule,
            execution_epoch,
        })
    }
}
