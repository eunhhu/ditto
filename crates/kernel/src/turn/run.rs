use std::{cell::Cell, collections::BTreeSet};

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use ditto_artifact_read::{
    ARTIFACT_READ_ID, ARTIFACT_READ_VERSION, ArtifactReadAuthority, ArtifactReadDeriver,
    ArtifactReadError, ArtifactReadResource, ArtifactReadResult, capability_schema,
    validate_artifact_read_manifest,
};
use ditto_artifact_store::ArtifactRef;
use ditto_capability::{
    CapabilityDeriver, CapabilitySchema, InvocationCompiler, InvocationError, LiveExecutionEpoch,
    UntrustedToolCall, UntrustedToolCallError,
};
use ditto_context::{
    CompiledContext, ContextCandidate, ContextCapsule, ContextCompileError, ContextCompiler,
    ContextSelection, TaskSignature,
};
use ditto_model::{
    CancellationId, CancellationToken, ContentPart, ConversationItem, ExecutionEpochId,
    FeatureRequest, FinishReason, GenerationControls, ModelContractError, ModelDriver, ModelEvent,
    ModelFeature, ModelRequest, ModelRequestId, ModelTurn, OutputConstraint, ParallelToolCalls,
    ProviderCallId, RequestControl, ToolCallBuffer, ToolChoice, ToolUsePolicy,
};
use ditto_policy::{AuthorizationOutcome, InvocationAuthorizer, PolicyError, StaticPolicy};
use ditto_protocol::{
    EventActor, EventQuery, EventRecord, NewEvent, SubmitInputCommand, event_kind,
};
use futures_util::StreamExt;
use serde::Serialize;
use serde_json::Value;
use ulid::Ulid;

#[path = "sort_run.rs"]
mod sort_tool;

use crate::{DittoKernel, KernelError, normalize_identifier, normalize_input_text};

use super::shared::{
    Checkpoint, HistoryExchange, MAX_HISTORY_CANDIDATES, ReadyCall, ThreadExchange, agent_run_text,
    append_assistant_text, bounded_turn_failure_message, history_messages, select_history,
    stable_system_prefix, turn_failure_code_for_model,
};
use super::types::{
    ArtifactReadTurnOutcome, ArtifactReadTurnStatus, CapabilitiesSelectedPayload,
    CapabilityRequestedPayload, ContextCompiledPayload, ExecutionOutputPayload,
    ExecutionStartedPayload, MAX_ASSISTANT_TEXT_BYTES, MAX_MODEL_EVENTS_PER_REQUEST,
    MAX_MODEL_OUTPUT_BYTES_PER_REQUEST, MAX_MODEL_OUTPUT_EVENT_BYTES, MAX_MODEL_REQUESTS,
    MAX_TURN_DURATION, ModelOutputPayload, ModelRequestedPayload, ReadOnlyTurnControl,
    TURN_PAYLOAD_VERSION, TurnFailedPayload, TurnFailure, TurnFailureCode, TurnFailureEvidence,
    TurnFailureReason, TurnFinishedPayload, TurnRunError,
};

#[derive(Clone)]
pub(super) struct TurnScope {
    turn_id: String,
    session_id: String,
    task_id: String,
    request_count: Cell<u8>,
    tool_call_count: Cell<u8>,
    effective_deadline: Cell<Option<DateTime<Utc>>>,
    agent_run: bool,
    sort: Option<super::sort::SortGrant>,
}

pub(crate) struct AdmittedReadOnlyTurn {
    scope: TurnScope,
    text: String,
    input: EventRecord,
}

impl AdmittedReadOnlyTurn {
    pub(crate) fn input(&self) -> &EventRecord {
        &self.input
    }
}

enum ContextProvenanceError {
    Kernel(KernelError),
    Invalid(TurnFailureReason, String),
}

/// One admitted turn after its absolute deadline is fixed. `cause` is the
/// latest durable turn event and the causation of the next one.
struct TurnRun<'d> {
    scope: TurnScope,
    cause: String,
    accepted_at: DateTime<Utc>,
    deadline: DateTime<Utc>,
    cancellation: CancellationToken,
    driver: &'d dyn ModelDriver,
}

/// The sealed execution epoch and the authority derived from it.
struct TurnTools {
    live_epoch: LiveExecutionEpoch,
    authorizer: InvocationAuthorizer,
    execution_epoch_id: ExecutionEpochId,
    schemas: Vec<CapabilitySchema>,
    authority: ArtifactReadAuthority,
}

/// Turn-wide bounds that span model requests.
#[derive(Default)]
struct TurnTotals {
    call_ids: BTreeSet<ProviderCallId>,
    text_bytes: usize,
    tool_calls: u8,
}

/// The validated terminal state of one model request.
struct ModelResponse {
    finish_reason: FinishReason,
    ready_call: Option<ReadyCall>,
    text: String,
}

impl DittoKernel {
    /// Run one bounded, provider-neutral turn with the installed
    /// `artifact.read` builtin as its only executable capability.
    pub async fn run_artifact_read_turn(
        &self,
        command: SubmitInputCommand,
        context_candidates: impl IntoIterator<Item = ContextCandidate>,
        driver: &dyn ModelDriver,
        cancellation: CancellationToken,
        control: ReadOnlyTurnControl,
    ) -> Result<ArtifactReadTurnOutcome, TurnRunError> {
        let admitted = self.admit_read_only_turn(command, None)?;
        self.continue_read_only_turn(
            admitted,
            Some(context_candidates),
            driver,
            cancellation,
            control,
        )
        .await
    }

    pub(crate) fn admit_read_only_turn(
        &self,
        command: SubmitInputCommand,
        agent_run: Option<crate::agent_run::AgentRunMetadata>,
    ) -> Result<AdmittedReadOnlyTurn, KernelError> {
        let text = normalize_input_text(&command.text)?;

        let scope = TurnScope {
            turn_id: format!("turn_{}", Ulid::new()),
            session_id: normalize_identifier(command.session_id, "session")?,
            task_id: normalize_identifier(command.task_id, "task")?,
            request_count: Cell::new(0),
            tool_call_count: Cell::new(0),
            effective_deadline: Cell::new(None),
            agent_run: agent_run.is_some(),
            sort: agent_run
                .as_ref()
                .and_then(|metadata| metadata.sort.clone()),
        };
        if self.task_is_completed(&scope.session_id, &scope.task_id)? {
            return Err(KernelError::InvalidCommand(format!(
                "task {} is already completed",
                scope.task_id
            )));
        }
        let mut input = NewEvent::user_input(
            scope.session_id.clone(),
            Some(scope.task_id.clone()),
            text.clone(),
        );
        input.correlation_id = Some(scope.turn_id.clone());
        if let Some(metadata) = agent_run {
            input.payload["agent_run"] = serde_json::to_value(metadata)?;
        }
        let input_event = self.append_and_publish(input)?;
        Ok(AdmittedReadOnlyTurn {
            scope,
            text,
            input: input_event,
        })
    }

    /// Drive an admitted turn: compile context, select capabilities, then run
    /// the bounded model/tool loop. Every failure is journaled before return.
    pub(crate) async fn continue_read_only_turn(
        &self,
        admitted: AdmittedReadOnlyTurn,
        context_candidates: Option<impl IntoIterator<Item = ContextCandidate>>,
        driver: &dyn ModelDriver,
        cancellation: CancellationToken,
        control: ReadOnlyTurnControl,
    ) -> Result<ArtifactReadTurnOutcome, TurnRunError> {
        let AdmittedReadOnlyTurn {
            scope,
            text,
            input: input_event,
        } = admitted;
        // The event store durably records millisecond timestamps. Derive the
        // ceiling from that exact precision so a reopen replay reconstructs
        // the same acceptance basis rather than comparing against lost nanos.
        let accepted_at =
            DateTime::from_timestamp_millis(input_event.recorded_at.timestamp_millis())
                .expect("a current UTC event timestamp is representable at millisecond precision");
        let hard_deadline = accepted_at
            + ChronoDuration::from_std(MAX_TURN_DURATION)
                .expect("five-minute turn ceiling fits chrono duration");
        let deadline = floor_to_millis(
            control
                .deadline
                .map_or(hard_deadline, |requested| requested.min(hard_deadline)),
        );
        scope.effective_deadline.set(Some(deadline));
        let mut run = TurnRun {
            scope,
            cause: input_event.event_id.clone(),
            accepted_at,
            deadline,
            cancellation,
            driver,
        };

        self.ensure_live(&run, Checkpoint::BeforeContextCompilation, None, None)?;
        let (capsule, history) =
            self.compile_turn_context(&mut run, &text, context_candidates, &input_event)?;
        let tools = self.select_turn_tools(&mut run, &input_event)?;
        self.run_model_requests(&mut run, text, history, capsule, tools)
            .await
    }

    /// Compile and journal the turn's source-verified context capsule and, for
    /// agent runs, the conversation thread's recent exchanges.
    fn compile_turn_context(
        &self,
        run: &mut TurnRun<'_>,
        text: &str,
        context_candidates: Option<impl IntoIterator<Item = ContextCandidate>>,
        input_event: &EventRecord,
    ) -> Result<(ContextCapsule, Vec<HistoryExchange>), TurnRunError> {
        let context_candidates = match context_candidates {
            Some(candidates) => candidates.into_iter().collect(),
            None => match self.agent_context_candidates(
                &run.scope.session_id,
                &run.scope.task_id,
                run.accepted_at,
            ) {
                Ok(candidates) => candidates,
                Err(_) => {
                    return Err(self.fail_with(
                        run,
                        TurnFailureReason::SessionContextUnavailable,
                        "verified session context is unavailable",
                        None,
                        None,
                    ));
                }
            },
        };
        let compiled = ContextCompiler::default()
            .compile_with(
                ContextSelection::CompleteSet,
                &turn_signature(text),
                context_candidates,
                None,
                run.accepted_at,
            )
            .map_err(|error| match context_compile_reason(&error) {
                Some(reason) => self.fail_with(run, reason, error.to_string(), None, None),
                None => self.fail(
                    run,
                    TurnFailureCode::ContextCompilation,
                    error.to_string(),
                    None,
                    None,
                ),
            })?;
        let capsule = ContextCapsule::from(&compiled);
        let provenance_through_seq = self.latest_event_seq()?;
        match self.validate_compiled_context_provenance(
            &run.scope,
            &compiled,
            provenance_through_seq,
        ) {
            Ok(()) => {}
            Err(ContextProvenanceError::Kernel(error)) => return Err(TurnRunError::Kernel(error)),
            Err(ContextProvenanceError::Invalid(reason, message)) => {
                return Err(self.fail_with(run, reason, message, None, None));
            }
        }
        let history = if run.scope.agent_run {
            self.conversation_history(&run.scope, input_event)
                .map_err(|_| {
                    self.fail_with(
                        run,
                        TurnFailureReason::ConversationHistoryUnavailable,
                        "conversation history is unavailable",
                        None,
                        None,
                    )
                })?
        } else {
            Vec::new()
        };
        self.append_turn_event(
            run,
            EventActor::System,
            event_kind::CONTEXT_COMPILED,
            &ContextCompiledPayload {
                event_version: TURN_PAYLOAD_VERSION,
                turn_id: run.scope.turn_id.clone(),
                provenance_through_seq,
                compiled,
                capsule: capsule.clone(),
                history_turn_ids: history
                    .iter()
                    .map(|exchange| exchange.turn_id.clone())
                    .collect(),
            },
            None,
        )?;
        Ok((capsule, history))
    }

    /// The current thread's finished agent-run exchanges before this input:
    /// bounded indexed reads of the latest candidates after the last reset.
    fn conversation_history(
        &self,
        scope: &TurnScope,
        input: &EventRecord,
    ) -> Result<Vec<HistoryExchange>, KernelError> {
        let exchanges =
            self.thread_exchanges(&scope.session_id, input.seq, MAX_HISTORY_CANDIDATES)?;
        Ok(select_history(
            exchanges.into_iter().map(|thread| thread.exchange),
        ))
    }

    /// Finished agent-run exchanges of the session's current thread before
    /// `before_seq`, newest first. At most `candidates` finished turns are
    /// examined; other turns in the thread are skipped.
    pub(crate) fn thread_exchanges(
        &self,
        session: &str,
        before_seq: i64,
        candidates: usize,
    ) -> Result<Vec<ThreadExchange>, KernelError> {
        let finished = self
            .inner
            .events
            .conversation_finished_turns(session, before_seq, candidates)?;
        let mut exchanges = Vec::with_capacity(finished.len());
        for event in finished {
            let payload: TurnFinishedPayload = serde_json::from_value(event.payload)?;
            let task = event
                .task_id
                .ok_or_else(|| KernelError::InvalidCommand("finished turn has no task".into()))?;
            let turn_input = self
                .inner
                .events
                .turn_input(session, &task, &payload.turn_id)?
                .ok_or_else(|| KernelError::InvalidCommand("finished turn has no input".into()))?;
            if let Some(user) = agent_run_text(&turn_input) {
                exchanges.push(ThreadExchange {
                    task_id: task,
                    finished_seq: event.seq,
                    exchange: HistoryExchange {
                        turn_id: payload.turn_id,
                        user: user.to_owned(),
                        assistant: payload.outcome.response,
                    },
                });
            }
        }
        Ok(exchanges)
    }

    /// Page the permitted capabilities into one sealed execution epoch,
    /// journal the selection, and derive its authorization ledger.
    fn select_turn_tools(
        &self,
        run: &mut TurnRun<'_>,
        input_event: &EventRecord,
    ) -> Result<TurnTools, TurnRunError> {
        let manifest = match self.inner.capabilities.page_manifest(ARTIFACT_READ_ID) {
            Ok(Some(manifest)) => manifest,
            Ok(None) => {
                return Err(self.fail_with(
                    run,
                    TurnFailureReason::ArtifactReadUnavailable,
                    "installed artifact.read capability is unavailable",
                    None,
                    None,
                ));
            }
            Err(_) => {
                return Err(self.fail_with(
                    run,
                    TurnFailureReason::ArtifactReadPackageUnverified,
                    "installed artifact.read package could not be verified",
                    None,
                    None,
                ));
            }
        };
        if let Err(error) = validate_artifact_read_manifest(&manifest) {
            return Err(self.fail_with(
                run,
                TurnFailureReason::ArtifactReadManifestMismatch,
                error.to_string(),
                None,
                None,
            ));
        }

        let schema = capability_schema();
        if schema.id != manifest.id || schema.version != manifest.version {
            return Err(self.fail_with(
                run,
                TurnFailureReason::ArtifactReadSchemaMismatch,
                "artifact.read level-2 schema does not match the installed manifest",
                None,
                None,
            ));
        }
        if let Err(error) = schema.validate() {
            return Err(self.fail_with(
                run,
                TurnFailureReason::ArtifactReadSchemaMismatch,
                error.to_string(),
                None,
                None,
            ));
        }

        let deriver = ArtifactReadDeriver::default();
        let mut live_epoch = LiveExecutionEpoch::new(if run.scope.sort.is_some() { 2 } else { 1 });
        let paged = live_epoch
            .page_in_invocable(&manifest, &schema, deriver.revision().clone())
            .map_err(|error| {
                self.fail_with(
                    run,
                    TurnFailureReason::ArtifactReadSelectionFailed,
                    error.to_string(),
                    None,
                    None,
                )
            })?;
        if paged != 1
            || live_epoch.evidence().capabilities().len() != 1
            || live_epoch.evidence().capabilities()[0].id != ARTIFACT_READ_ID
        {
            return Err(self.fail_with(
                run,
                TurnFailureReason::ArtifactReadSelectionFailed,
                "artifact.read could not be selected as the sole execution capability",
                None,
                None,
            ));
        }
        let sort_manifest = if let Some(grant) = &run.scope.sort {
            let root = self
                .inner
                .events
                .get_by_event_id(&grant.source_event_id)
                .map_err(KernelError::from)?;
            if !root
                .as_ref()
                .is_some_and(|root| grant.matches_root(root, input_event))
            {
                return Err(self.fail_with(
                    run,
                    TurnFailureReason::SortPermissionSourceUnavailable,
                    "sort permission source is unavailable",
                    None,
                    None,
                ));
            }
            let selected = self
                .inner
                .capabilities
                .page_manifest(ditto_artifact_sort::ID)
                .ok()
                .flatten();
            let Some(selected) = selected
                .filter(|manifest| ditto_artifact_sort::validate_manifest(manifest).is_ok())
            else {
                return Err(self.fail_with(
                    run,
                    TurnFailureReason::SortContractUnavailable,
                    "installed artifact.sort contract is unavailable",
                    None,
                    None,
                ));
            };
            let sort_deriver = ditto_artifact_sort::SortDeriver::default();
            live_epoch
                .page_in_invocable(
                    &selected,
                    &ditto_artifact_sort::schema(),
                    sort_deriver.revision().clone(),
                )
                .map_err(|_| {
                    TurnRunError::Internal("sort capability could not enter the live epoch")
                })?;
            Some(selected)
        } else {
            None
        };
        let authorization_ticket = live_epoch.seal_for_authorization().map_err(|error| {
            self.fail_with(
                run,
                TurnFailureReason::ArtifactReadSelectionFailed,
                error.to_string(),
                None,
                None,
            )
        })?;
        let binding =
            live_epoch
                .invocable_binding(ARTIFACT_READ_ID)
                .ok_or(TurnRunError::Internal(
                    "artifact.read live epoch issued no invocation binding",
                ))?;
        let execution_epoch_id = ExecutionEpochId::new(live_epoch.id()).map_err(|error| {
            self.fail_with(
                run,
                TurnFailureReason::ArtifactReadSelectionFailed,
                error.to_string(),
                None,
                None,
            )
        })?;
        let manifest = binding.manifest().clone();
        let mut schemas = vec![binding.schema().clone()];
        if run.scope.sort.is_some() {
            schemas.push(ditto_artifact_sort::schema());
        }
        self.append_turn_event(
            run,
            EventActor::System,
            event_kind::CAPABILITIES_SELECTED,
            &CapabilitiesSelectedPayload {
                event_version: TURN_PAYLOAD_VERSION,
                turn_id: run.scope.turn_id.clone(),
                manifest,
                sort_manifest,
                epoch: live_epoch.evidence().clone(),
                schemas: schemas.clone(),
            },
            None,
        )?;

        let authorizer = InvocationAuthorizer::from_ticket(authorization_ticket, run.deadline)
            .map_err(|_| TurnRunError::Internal("live epoch authorization setup failed"))?;
        if let Some(grant) = &run.scope.sort {
            authorizer
                .register_lease(
                    ditto_policy::CapabilityLease::new(
                        "agent-sort",
                        run.deadline,
                        ditto_artifact_sort::effect(),
                        1,
                        BTreeSet::from([ditto_artifact_sort::ID.into()]),
                        vec![ditto_policy::ResourceScope::Exact(
                            ditto_capability::CanonicalResource::artifact(&grant.reference)
                                .map_err(|_| TurnRunError::Internal("invalid sort grant"))?,
                        )],
                        ditto_policy::ApprovalRequirement::Never,
                    )
                    .map_err(|_| TurnRunError::Internal("invalid sort lease"))?,
                )
                .map_err(|_| TurnRunError::Internal("sort lease registration failed"))?;
        }

        Ok(TurnTools {
            live_epoch,
            authorizer,
            execution_epoch_id,
            schemas,
            authority: ArtifactReadAuthority::new(self.inner.artifacts.clone()),
        })
    }

    /// The bounded request loop: each model request either ends the turn with
    /// an unverified answer or yields exactly one tool call to execute.
    async fn run_model_requests(
        &self,
        run: &mut TurnRun<'_>,
        text: String,
        history: Vec<HistoryExchange>,
        capsule: ContextCapsule,
        tools: TurnTools,
    ) -> Result<ArtifactReadTurnOutcome, TurnRunError> {
        let mut conversation = history_messages(&history);
        conversation.extend(super::sort::initial_conversation(
            text,
            run.scope.sort.as_ref(),
        ));
        let mut totals = TurnTotals::default();

        for request_index in 0..MAX_MODEL_REQUESTS {
            let index = Some(request_index as u8);
            self.ensure_live(run, Checkpoint::BeforeModelRequest, index, None)?;
            let (request, requested_at) =
                self.dispatch_model_request(run, request_index, &tools, &capsule, &conversation)?;
            tokio::task::yield_now().await;
            self.ensure_live(run, Checkpoint::AfterModelRequestPersisted, index, None)?;
            if let Err(error) = request.validate_at(requested_at) {
                return Err(self.fail_with(
                    run,
                    TurnFailureReason::RequestInvalidAtDispatch,
                    error.to_string(),
                    index,
                    None,
                ));
            }

            let response = self
                .stream_model_response(run, request_index, &request, &mut conversation, &mut totals)
                .await?;
            if response.finish_reason == FinishReason::ToolCalls {
                self.execute_tool_call(
                    run,
                    request_index,
                    response.ready_call,
                    &tools,
                    &mut conversation,
                    &mut totals,
                )
                .await?;
                continue;
            }
            return self
                .finish_turn(run, request_index, response, &tools, &totals)
                .await;
        }

        unreachable!("the bounded request loop returns from every terminal path")
    }

    /// Build, validate, and journal one model request before driver I/O.
    /// Returns the request and its durable `model.requested` timestamp.
    fn dispatch_model_request(
        &self,
        run: &mut TurnRun<'_>,
        request_index: usize,
        tools: &TurnTools,
        capsule: &ContextCapsule,
        conversation: &[ConversationItem],
    ) -> Result<(ModelRequest, DateTime<Utc>), TurnRunError> {
        let index = Some(request_index as u8);
        let request = build_model_request(
            &run.scope,
            request_index,
            tools.execution_epoch_id.clone(),
            capsule.clone(),
            tools.schemas.clone(),
            conversation.to_vec(),
            run.deadline,
        )
        .map_err(|error| self.fail(run, TurnFailureCode::DriverContract, error, index, None))?;
        if let Err(error) = request.validate_at(run.accepted_at) {
            return Err(self.fail(
                run,
                TurnFailureCode::DriverContract,
                error.to_string(),
                index,
                None,
            ));
        }
        if let Err(error) = request.validate_against(run.driver.descriptor()) {
            return Err(match driver_contract_reason(&error) {
                Some(reason) => self.fail_with(run, reason, error.to_string(), index, None),
                None => self.fail(
                    run,
                    TurnFailureCode::DriverContract,
                    error.to_string(),
                    index,
                    None,
                ),
            });
        }
        self.ensure_before_deadline(run, Checkpoint::BeforeModelRequest, index, None)?;

        let requested = self.append_turn_event(
            run,
            EventActor::System,
            event_kind::MODEL_REQUESTED,
            &ModelRequestedPayload {
                event_version: TURN_PAYLOAD_VERSION,
                turn_id: run.scope.turn_id.clone(),
                request_index: request_index as u8,
                request: request.clone(),
            },
            Some(request.request_id.to_string()),
        )?;
        run.scope.request_count.set((request_index + 1) as u8);
        Ok((request, requested.recorded_at))
    }

    /// Admit and journal the driver's validated stream for one request.
    async fn stream_model_response(
        &self,
        run: &mut TurnRun<'_>,
        request_index: usize,
        request: &ModelRequest,
        conversation: &mut Vec<ConversationItem>,
        totals: &mut TurnTotals,
    ) -> Result<ModelResponse, TurnRunError> {
        let index = Some(request_index as u8);
        // The complete request is durable before the driver receives it.
        let driver = run.driver;
        let mut stream = driver.stream(request.clone(), run.cancellation.clone());
        let mut event_count = 0_usize;
        let mut model_output_bytes = 0_usize;
        let mut tool_buffer = ToolCallBuffer::default();
        let mut ready_call: Option<ReadyCall> = None;
        let mut request_text = String::new();

        loop {
            if event_count == MAX_MODEL_EVENTS_PER_REQUEST {
                return Err(self.fail(
                    run,
                    TurnFailureCode::BoundExceeded,
                    format!(
                        "model request exceeded {MAX_MODEL_EVENTS_PER_REQUEST} events without a terminal"
                    ),
                    index,
                    None,
                ));
            }
            let deadline_wait = tokio::time::sleep(duration_until(run.deadline));
            tokio::pin!(deadline_wait);
            let next = tokio::select! {
                biased;
                _ = run.cancellation.cancelled() => {
                    return Err(self.fail(
                        run,
                        TurnFailureCode::Cancelled,
                        Checkpoint::AwaitingModelOutput.cancelled_message(),
                        index,
                        None,
                    ));
                }
                _ = &mut deadline_wait => {
                    return Err(self.fail(
                        run,
                        TurnFailureCode::DeadlineExceeded,
                        Checkpoint::AwaitingModelOutput.deadline_message(),
                        index,
                        None,
                    ));
                }
                next = stream.next() => next,
            };
            let Some(stream_event) = next else {
                return Err(TurnRunError::Internal(
                    "validated model stream ended without an admitted terminal",
                ));
            };
            self.ensure_live(run, Checkpoint::AwaitingModelOutput, index, None)?;
            if let ModelEvent::TextDelta { text } = &stream_event.event
                && totals.text_bytes.saturating_add(text.len()) > MAX_ASSISTANT_TEXT_BYTES
            {
                return Err(self.fail(
                    run,
                    TurnFailureCode::BoundExceeded,
                    format!("assistant text exceeded {MAX_ASSISTANT_TEXT_BYTES} bytes"),
                    index,
                    None,
                ));
            }

            let mut output_payload = ModelOutputPayload {
                event_version: TURN_PAYLOAD_VERSION,
                turn_id: run.scope.turn_id.clone(),
                request_index: request_index as u8,
                request_id: request.request_id.clone(),
                admitted_at: ceil_to_millis(Utc::now()),
                stream_event: stream_event.clone(),
            };
            let encoded_output_bytes = serde_json::to_vec(&output_payload)?.len();
            self.ensure_output_bounds(run, index, model_output_bytes, encoded_output_bytes)?;
            self.ensure_not_cancelled(run, Checkpoint::AwaitingModelOutput, index, None)?;
            let admitted_at = ceil_to_millis(Utc::now());
            if admitted_at >= run.deadline {
                return Err(self.fail(
                    run,
                    TurnFailureCode::DeadlineExceeded,
                    Checkpoint::AwaitingModelOutput.deadline_message(),
                    index,
                    None,
                ));
            }
            output_payload.admitted_at = admitted_at;
            let encoded_output_bytes = serde_json::to_vec(&output_payload)?.len();
            self.ensure_output_bounds(run, index, model_output_bytes, encoded_output_bytes)?;
            self.ensure_live(run, Checkpoint::AwaitingModelOutput, index, None)?;
            event_count += 1;

            self.append_turn_event(
                run,
                EventActor::Model,
                event_kind::MODEL_OUTPUT,
                &output_payload,
                Some(request.request_id.to_string()),
            )?;
            model_output_bytes = model_output_bytes.saturating_add(encoded_output_bytes);

            match &stream_event.event {
                ModelEvent::TextDelta { text } => {
                    totals.text_bytes += text.len();
                    request_text.push_str(text);
                    append_assistant_text(conversation, text);
                }
                ModelEvent::ToolCallStarted {
                    call_id,
                    capability_id,
                } => {
                    self.ensure_known_capability(run, index, call_id, capability_id)?;
                    if !totals.call_ids.insert(call_id.clone()) {
                        return Err(self.fail(
                            run,
                            TurnFailureCode::Protocol,
                            format!("duplicate epoch-wide tool call id {call_id}"),
                            index,
                            Some(call_id.clone()),
                        ));
                    }
                    if let Err(error) = tool_buffer.start(call_id.clone(), capability_id.clone()) {
                        return Err(self.fail_with(
                            run,
                            TurnFailureReason::ToolCallLifecycle,
                            error.to_string(),
                            index,
                            Some(error.call_id().clone()),
                        ));
                    }
                }
                ModelEvent::ToolCallArgumentDelta { call_id, delta } => {
                    if let Err(error) = tool_buffer.push_arguments(call_id, delta) {
                        return Err(self.fail_with(
                            run,
                            TurnFailureReason::ToolCallLifecycle,
                            error.to_string(),
                            index,
                            Some(error.call_id().clone()),
                        ));
                    }
                }
                ModelEvent::ToolCallReady {
                    call_id,
                    capability_id,
                    arguments,
                } => {
                    self.ensure_known_capability(run, index, call_id, capability_id)?;
                    if ready_call.is_some() {
                        return Err(self.fail(
                            run,
                            TurnFailureCode::Protocol,
                            "a model request produced more than one ready tool call",
                            index,
                            Some(call_id.clone()),
                        ));
                    }
                    let rebuilt = tool_buffer.finish(call_id).map_err(|error| {
                        self.fail_with(
                            run,
                            TurnFailureReason::ToolCallLifecycle,
                            error.to_string(),
                            index,
                            Some(error.call_id().clone()),
                        )
                    })?;
                    if rebuilt != stream_event.event {
                        return Err(self.fail(
                            run,
                            TurnFailureCode::Protocol,
                            "ready tool call does not match accumulated arguments",
                            index,
                            Some(call_id.clone()),
                        ));
                    }
                    ready_call = Some(ReadyCall {
                        call_id: call_id.clone(),
                        capability_id: capability_id.clone(),
                        arguments: arguments.clone(),
                    });
                    conversation.push(ConversationItem::ToolCall {
                        call_id: call_id.clone(),
                        capability_id: capability_id.clone(),
                        arguments: arguments.clone(),
                    });
                }
                ModelEvent::ReasoningItemStarted { .. }
                | ModelEvent::ReasoningDelta { .. }
                | ModelEvent::ReasoningItemReady { .. } => {
                    return Err(self.fail(
                        run,
                        TurnFailureCode::Protocol,
                        "reasoning events are not permitted in this turn loop",
                        index,
                        None,
                    ));
                }
                ModelEvent::StructuredOutput { .. } => {
                    return Err(self.fail(
                        run,
                        TurnFailureCode::Protocol,
                        "text-constrained turn received structured output",
                        index,
                        None,
                    ));
                }
                ModelEvent::Failed { failure } => {
                    return Err(self.fail(
                        run,
                        turn_failure_code_for_model(failure.kind),
                        bounded_turn_failure_message(&failure.message),
                        index,
                        failure.call_id.clone(),
                    ));
                }
                ModelEvent::Completed {
                    finish_reason,
                    continuation,
                } => {
                    if tool_buffer.has_active_calls() {
                        return Err(self.fail(
                            run,
                            TurnFailureCode::Protocol,
                            "model terminal has an unfinished tool call",
                            index,
                            None,
                        ));
                    }
                    if continuation.is_some() {
                        return Err(self.fail(
                            run,
                            TurnFailureCode::Protocol,
                            "provider-managed continuation is not permitted in this turn loop",
                            index,
                            None,
                        ));
                    }
                    return Ok(ModelResponse {
                        finish_reason: finish_reason.clone(),
                        ready_call,
                        text: request_text,
                    });
                }
                ModelEvent::UsageUpdate { .. } | ModelEvent::ProviderWarning { .. } => {}
            }
        }
    }

    /// Execute the one ready call of a `ToolCalls` terminal and append its
    /// structured result to the conversation.
    async fn execute_tool_call(
        &self,
        run: &mut TurnRun<'_>,
        request_index: usize,
        ready_call: Option<ReadyCall>,
        tools: &TurnTools,
        conversation: &mut Vec<ConversationItem>,
        totals: &mut TurnTotals,
    ) -> Result<(), TurnRunError> {
        let index = Some(request_index as u8);
        let Some(call) = ready_call else {
            return Err(self.fail(
                run,
                TurnFailureCode::Protocol,
                "tool-call terminal contained no ready tool call",
                index,
                None,
            ));
        };
        if request_index + 1 >= MAX_MODEL_REQUESTS {
            return Err(self.fail(
                run,
                TurnFailureCode::BoundExceeded,
                format!("turn exhausted the {MAX_MODEL_REQUESTS}-request limit"),
                index,
                Some(call.call_id),
            ));
        }

        // Give cancellation a deterministic checkpoint after the provider
        // terminal and before any capability request is journaled.
        tokio::task::yield_now().await;
        self.ensure_live(
            run,
            Checkpoint::BeforeCapabilityRequest,
            index,
            Some(call.call_id.clone()),
        )?;
        let (value, is_error) = if call.capability_id == ditto_artifact_sort::ID {
            let binding = tools
                .live_epoch
                .invocable_binding(ditto_artifact_sort::ID)
                .ok_or(TurnRunError::Internal("missing sort binding"))?;
            let result = self
                .run_sort_tool(
                    &mut run.scope,
                    &mut run.cause,
                    request_index as u8,
                    &call,
                    binding,
                    &tools.authorizer,
                    run.cancellation.clone(),
                    run.deadline,
                )
                .await?;
            (result.model_value(), result.is_error())
        } else {
            let result = self
                .execute_artifact_read(run, request_index, &call, tools)
                .await?;
            (serde_json::to_value(&result)?, result.is_error())
        };
        conversation.push(ConversationItem::ToolResult {
            call_id: call.call_id,
            content: vec![ContentPart::Structured { value }],
            is_error,
        });
        totals.tool_calls = totals.tool_calls.saturating_add(1);
        run.scope.tool_call_count.set(totals.tool_calls);
        Ok(())
    }

    /// Normalize, authorize, and execute one `artifact.read` call, journaling
    /// the request, start, and deterministic result.
    async fn execute_artifact_read(
        &self,
        run: &mut TurnRun<'_>,
        request_index: usize,
        call: &ReadyCall,
        tools: &TurnTools,
    ) -> Result<ArtifactReadResult, TurnRunError> {
        let index = Some(request_index as u8);
        let binding =
            tools
                .live_epoch
                .invocable_binding(ARTIFACT_READ_ID)
                .ok_or(TurnRunError::Internal(
                    "artifact.read live epoch issued no invocation binding",
                ))?;
        let untrusted_call = match UntrustedToolCall::new(
            call.call_id.to_string(),
            call.capability_id.clone(),
            call.arguments.clone(),
        ) {
            Ok(call) => Some(call),
            Err(
                UntrustedToolCallError::ArgumentsTooLarge { .. }
                | UntrustedToolCallError::ArgumentsTooDeep { .. }
                | UntrustedToolCallError::ArgumentsTooComplex { .. },
            ) => None,
            Err(error) => {
                return Err(self.fail(
                    run,
                    TurnFailureCode::CapabilityContract,
                    error.to_string(),
                    index,
                    Some(call.call_id.clone()),
                ));
            }
        };
        let canonical = match untrusted_call {
            Some(untrusted_call) => {
                match InvocationCompiler::compile(
                    binding,
                    untrusted_call,
                    &ArtifactReadDeriver::default(),
                ) {
                    Ok(invocation) => Some(invocation),
                    Err(InvocationError::ArgumentsSchema {
                        stage: ditto_capability::ArgumentStage::Raw,
                        ..
                    }) => None,
                    Err(error) => {
                        return Err(self.fail(
                            run,
                            TurnFailureCode::CapabilityContract,
                            error.to_string(),
                            index,
                            Some(call.call_id.clone()),
                        ));
                    }
                }
            }
            None => None,
        };
        let normalized: Result<ArtifactReadResource, ArtifactReadError> = match canonical.as_ref() {
            Some(invocation) => Ok(serde_json::from_value(
                invocation.normalized_arguments().clone(),
            )
            .map_err(|_| {
                TurnRunError::Internal("canonical artifact.read arguments are not a typed resource")
            })?),
            None => Err(artifact_read_argument_error(&call.arguments)),
        };
        self.append_turn_event(
            run,
            EventActor::Model,
            event_kind::CAPABILITY_REQUESTED,
            &CapabilityRequestedPayload {
                event_version: TURN_PAYLOAD_VERSION,
                turn_id: run.scope.turn_id.clone(),
                request_index: request_index as u8,
                execution_epoch_id: tools.execution_epoch_id.clone(),
                call_id: call.call_id.clone(),
                capability_id: call.capability_id.clone(),
                capability_version: ARTIFACT_READ_VERSION.into(),
                arguments: call.arguments.clone(),
                normalized: normalized.as_ref().ok().cloned(),
            },
            Some(call.call_id.to_string()),
        )?;

        let authorization_through_seq = self.latest_event_seq()?;
        let authorized = match &normalized {
            Ok(resource) => {
                self.artifact_is_authorized(&run.scope, resource, authorization_through_seq)?
            }
            Err(_) => false,
        };
        let permit = if let Some(invocation) = canonical.as_ref() {
            let policy_resource = authorized
                .then(|| invocation.resources().iter().next().cloned())
                .flatten();
            let policy = StaticPolicy::artifact_read_scope(policy_resource).map_err(|_| {
                TurnRunError::Internal("artifact.read static policy construction failed")
            })?;
            match tools
                .authorizer
                .authorize_static(invocation, &policy, Utc::now())
            {
                Ok(AuthorizationOutcome::Permitted(permit)) if authorized => Some(permit),
                Err(PolicyError::MissingResourceScope) if !authorized => None,
                Ok(AuthorizationOutcome::ApprovalRequired(_))
                | Ok(AuthorizationOutcome::Permitted(_))
                | Err(_) => {
                    return Err(TurnRunError::Internal(
                        "artifact.read static policy authorization contradicted scope",
                    ));
                }
            }
        } else {
            None
        };
        // Authorization is bounded by the captured high-water. Yield once
        // more so cancellation/deadline can stop the turn before an
        // execution.started claim is made.
        tokio::task::yield_now().await;
        self.ensure_live(
            run,
            Checkpoint::AfterCapabilityRequest,
            index,
            Some(call.call_id.clone()),
        )?;
        self.append_turn_event(
            run,
            EventActor::Capability,
            event_kind::EXECUTION_STARTED,
            &ExecutionStartedPayload {
                event_version: TURN_PAYLOAD_VERSION,
                turn_id: run.scope.turn_id.clone(),
                request_index: request_index as u8,
                call_id: call.call_id.clone(),
                capability_id: ARTIFACT_READ_ID.into(),
                capability_version: ARTIFACT_READ_VERSION.into(),
                authorization_through_seq,
                resource: normalized.as_ref().ok().cloned(),
            },
            Some(call.call_id.to_string()),
        )?;

        // This yield is an intentional, deterministic cancellation checkpoint
        // after the durable start and before any store read or durable result.
        tokio::task::yield_now().await;
        self.ensure_live(
            run,
            Checkpoint::AfterExecutionStarted,
            index,
            Some(call.call_id.clone()),
        )?;

        let result = match normalized {
            Err(error) => ArtifactReadResult::error(error),
            Ok(resource) if !authorized => {
                ArtifactReadResult::error(ditto_artifact_read::ArtifactReadError::not_authorized(
                    resource.reference().clone(),
                ))
            }
            Ok(resource) => {
                let invocation = canonical.as_ref().ok_or(TurnRunError::Internal(
                    "authorized artifact.read has no canonical invocation",
                ))?;
                let permit = permit.as_ref().ok_or(TurnRunError::Internal(
                    "authorized artifact.read has no invocation permit",
                ))?;
                permit.validate(invocation, Utc::now()).map_err(|_| {
                    TurnRunError::Internal(
                        "artifact.read invocation permit is invalid at execution",
                    )
                })?;
                tools.authority.execute(&resource)
            }
        };
        self.ensure_live(
            run,
            Checkpoint::AfterArtifactRead,
            index,
            Some(call.call_id.clone()),
        )?;
        self.append_turn_event(
            run,
            EventActor::Capability,
            event_kind::EXECUTION_OUTPUT,
            &ExecutionOutputPayload {
                event_version: TURN_PAYLOAD_VERSION,
                turn_id: run.scope.turn_id.clone(),
                request_index: request_index as u8,
                call_id: call.call_id.clone(),
                capability_id: ARTIFACT_READ_ID.into(),
                capability_version: ARTIFACT_READ_VERSION.into(),
                result: result.clone(),
            },
            Some(call.call_id.to_string()),
        )?;
        Ok(result)
    }

    /// Journal the unverified final answer of a non-tool model terminal.
    async fn finish_turn(
        &self,
        run: &mut TurnRun<'_>,
        request_index: usize,
        response: ModelResponse,
        tools: &TurnTools,
        totals: &TurnTotals,
    ) -> Result<ArtifactReadTurnOutcome, TurnRunError> {
        let index = Some(request_index as u8);
        if let Some(call) = response.ready_call {
            return Err(self.fail(
                run,
                TurnFailureCode::Protocol,
                "non-tool terminal followed a ready tool call",
                index,
                Some(call.call_id),
            ));
        }
        if totals.tool_calls == 0 && !run.scope.agent_run {
            return Err(self.fail(
                run,
                TurnFailureCode::Protocol,
                "turn ended before executing artifact.read",
                index,
                None,
            ));
        }
        if response.text.is_empty() {
            return Err(self.fail(
                run,
                TurnFailureCode::Protocol,
                "final model request produced no assistant text",
                index,
                None,
            ));
        }

        tokio::task::yield_now().await;
        self.ensure_live(run, Checkpoint::AfterFinalOutput, index, None)?;

        let outcome = ArtifactReadTurnOutcome {
            turn_id: run.scope.turn_id.clone(),
            session_id: run.scope.session_id.clone(),
            task_id: run.scope.task_id.clone(),
            execution_epoch_id: tools.execution_epoch_id.clone(),
            response: response.text,
            status: ArtifactReadTurnStatus::Unverified,
            request_count: (request_index + 1) as u8,
            tool_call_count: totals.tool_calls,
        };
        self.append_turn_event(
            run,
            EventActor::System,
            event_kind::TURN_FINISHED,
            &TurnFinishedPayload {
                event_version: TURN_PAYLOAD_VERSION,
                turn_id: run.scope.turn_id.clone(),
                outcome: outcome.clone(),
            },
            None,
        )?;
        Ok(outcome)
    }

    fn ensure_known_capability(
        &self,
        run: &TurnRun<'_>,
        index: Option<u8>,
        call_id: &ProviderCallId,
        capability_id: &str,
    ) -> Result<(), TurnRunError> {
        if capability_id == ARTIFACT_READ_ID
            || (run.scope.sort.is_some() && capability_id == ditto_artifact_sort::ID)
        {
            return Ok(());
        }
        Err(self.fail(
            run,
            TurnFailureCode::Protocol,
            format!("unknown capability {capability_id}"),
            index,
            Some(call_id.clone()),
        ))
    }

    fn ensure_output_bounds(
        &self,
        run: &TurnRun<'_>,
        index: Option<u8>,
        request_bytes: usize,
        event_bytes: usize,
    ) -> Result<(), TurnRunError> {
        if event_bytes > MAX_MODEL_OUTPUT_EVENT_BYTES {
            return Err(self.fail(
                run,
                TurnFailureCode::BoundExceeded,
                format!("model output exceeded {MAX_MODEL_OUTPUT_EVENT_BYTES} encoded bytes"),
                index,
                None,
            ));
        }
        if request_bytes.saturating_add(event_bytes) > MAX_MODEL_OUTPUT_BYTES_PER_REQUEST {
            return Err(self.fail(
                run,
                TurnFailureCode::BoundExceeded,
                format!(
                    "model request output exceeded {MAX_MODEL_OUTPUT_BYTES_PER_REQUEST} encoded bytes"
                ),
                index,
                None,
            ));
        }
        Ok(())
    }

    /// Stop at a checkpoint when cancelled, then when the deadline elapsed.
    fn ensure_live(
        &self,
        run: &TurnRun<'_>,
        checkpoint: Checkpoint,
        request_index: Option<u8>,
        call_id: Option<ProviderCallId>,
    ) -> Result<(), TurnRunError> {
        self.ensure_not_cancelled(run, checkpoint, request_index, call_id.clone())?;
        self.ensure_before_deadline(run, checkpoint, request_index, call_id)
    }

    fn ensure_not_cancelled(
        &self,
        run: &TurnRun<'_>,
        checkpoint: Checkpoint,
        request_index: Option<u8>,
        call_id: Option<ProviderCallId>,
    ) -> Result<(), TurnRunError> {
        if run.cancellation.is_cancelled() {
            return Err(self.fail(
                run,
                TurnFailureCode::Cancelled,
                checkpoint.cancelled_message(),
                request_index,
                call_id,
            ));
        }
        Ok(())
    }

    fn ensure_before_deadline(
        &self,
        run: &TurnRun<'_>,
        checkpoint: Checkpoint,
        request_index: Option<u8>,
        call_id: Option<ProviderCallId>,
    ) -> Result<(), TurnRunError> {
        if deadline_expired(run.deadline) {
            return Err(self.fail(
                run,
                TurnFailureCode::DeadlineExceeded,
                checkpoint.deadline_message(),
                request_index,
                call_id,
            ));
        }
        Ok(())
    }

    fn fail(
        &self,
        run: &TurnRun<'_>,
        code: TurnFailureCode,
        message: impl Into<String>,
        request_index: Option<u8>,
        call_id: Option<ProviderCallId>,
    ) -> TurnRunError {
        self.persist_turn_failure(
            &run.scope,
            &run.cause,
            code,
            message,
            request_index,
            call_id,
        )
    }

    /// Journal a validator-derived failure with its typed reason.
    fn fail_with(
        &self,
        run: &TurnRun<'_>,
        reason: TurnFailureReason,
        message: impl Into<String>,
        request_index: Option<u8>,
        call_id: Option<ProviderCallId>,
    ) -> TurnRunError {
        self.persist_failure(
            &run.scope,
            &run.cause,
            reason.code(),
            Some(reason),
            message.into(),
            request_index,
            call_id,
        )
    }

    /// Append one turn transition caused by the previous one.
    fn append_turn_event<T: Serialize>(
        &self,
        run: &mut TurnRun<'_>,
        actor: EventActor,
        kind: &str,
        payload: &T,
        span_id: Option<String>,
    ) -> Result<EventRecord, TurnRunError> {
        let event = self.append_turn_payload(
            &run.scope,
            actor,
            kind,
            payload,
            Some(run.cause.clone()),
            span_id,
        )?;
        run.cause = event.event_id.clone();
        Ok(event)
    }

    fn append_turn_payload<T: Serialize>(
        &self,
        scope: &TurnScope,
        actor: EventActor,
        kind: &str,
        payload: &T,
        causation_id: Option<String>,
        span_id: Option<String>,
    ) -> Result<EventRecord, TurnRunError> {
        Ok(self.append_and_publish(NewEvent {
            session_id: Some(scope.session_id.clone()),
            task_id: Some(scope.task_id.clone()),
            actor,
            kind: kind.into(),
            payload: serde_json::to_value(payload)?,
            causation_id,
            correlation_id: Some(scope.turn_id.clone()),
            span_id,
        })?)
    }

    fn persist_turn_failure(
        &self,
        scope: &TurnScope,
        cause: &str,
        code: TurnFailureCode,
        message: impl Into<String>,
        request_index: Option<u8>,
        call_id: Option<ProviderCallId>,
    ) -> TurnRunError {
        self.persist_failure(
            scope,
            cause,
            code,
            None,
            message.into(),
            request_index,
            call_id,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn persist_failure(
        &self,
        scope: &TurnScope,
        cause: &str,
        code: TurnFailureCode,
        reason: Option<TurnFailureReason>,
        message: String,
        request_index: Option<u8>,
        call_id: Option<ProviderCallId>,
    ) -> TurnRunError {
        let evidence =
            (code == TurnFailureCode::DeadlineExceeded).then(|| TurnFailureEvidence::Deadline {
                deadline: scope
                    .effective_deadline
                    .get()
                    .expect("deadline is fixed before any durable turn failure"),
            });
        let failure = TurnFailure {
            turn_id: scope.turn_id.clone(),
            session_id: scope.session_id.clone(),
            task_id: scope.task_id.clone(),
            code,
            message: bounded_turn_failure_message(&message),
            request_index,
            call_id,
            evidence,
            reason,
        };
        match self.append_turn_payload(
            scope,
            EventActor::System,
            event_kind::TURN_FAILED,
            &TurnFailedPayload {
                event_version: TURN_PAYLOAD_VERSION,
                turn_id: scope.turn_id.clone(),
                failure: failure.clone(),
                status: ArtifactReadTurnStatus::Unverified,
                request_count: scope.request_count.get(),
                tool_call_count: scope.tool_call_count.get(),
            },
            Some(cause.to_owned()),
            None,
        ) {
            Ok(_) => TurnRunError::Failed(Box::new(failure)),
            Err(error) => error,
        }
    }

    fn artifact_is_authorized(
        &self,
        scope: &TurnScope,
        resource: &ArtifactReadResource,
        high_water: i64,
    ) -> Result<bool, KernelError> {
        let mut query = EventQuery {
            session_id: Some(scope.session_id.clone()),
            ..EventQuery::default()
        };
        loop {
            query.limit = Some(1_000);
            let page = self.list_events_through(&query, high_water)?;
            if page.is_empty() {
                break;
            }
            let reference = resource.reference().to_string();
            if page.iter().any(|event| {
                event.kind == event_kind::ARTIFACT_CREATED
                    && event.actor == EventActor::System
                    && event.payload.get("reference").and_then(Value::as_str)
                        == Some(reference.as_str())
                    && event
                        .task_id
                        .as_deref()
                        .is_none_or(|task_id| task_id == scope.task_id)
            }) {
                return Ok(true);
            }
            let last_seq = page.last().map_or(0, |event| event.seq);
            if last_seq >= high_water || page.len() < 1_000 {
                break;
            }
            query.after_seq = Some(last_seq);
        }
        Ok(false)
    }

    fn task_is_completed(&self, session_id: &str, task_id: &str) -> Result<bool, KernelError> {
        Ok(self.inner.events.task_has_event_kind(
            session_id,
            task_id,
            event_kind::TASK_COMPLETED,
        )?)
    }

    fn validate_compiled_context_provenance(
        &self,
        scope: &TurnScope,
        compiled: &CompiledContext,
        high_water: i64,
    ) -> Result<(), ContextProvenanceError> {
        let mut required = BTreeSet::new();
        for node in &compiled.nodes {
            if node.source_event_ids.is_empty() {
                return Err(ContextProvenanceError::Invalid(
                    TurnFailureReason::MissingContextProvenance,
                    format!(
                        "included context node {} has no source event provenance",
                        node.id
                    ),
                ));
            }
            required.extend(node.source_event_ids.iter().cloned());
        }
        if required.is_empty() {
            return Ok(());
        }

        let mut found = BTreeSet::new();
        for id in &required {
            let event = self
                .inner
                .events
                .get_by_event_id(id)
                .map_err(KernelError::from)
                .map_err(ContextProvenanceError::Kernel)?;
            if event.is_some_and(|event| {
                event.seq <= high_water
                    && event.session_id.as_deref() == Some(&scope.session_id)
                    && event
                        .task_id
                        .as_deref()
                        .is_none_or(|task| task == scope.task_id)
            }) {
                found.insert(id.clone());
            }
        }
        if found.len() == required.len() {
            return Ok(());
        }
        let missing = required.difference(&found).cloned().collect::<Vec<_>>();
        Err(ContextProvenanceError::Invalid(
            TurnFailureReason::UnresolvedContextProvenance,
            format!(
                "included context provenance does not resolve in the current scope: {}",
                missing.join(", ")
            ),
        ))
    }
}

/// The version-2 retrieval signature of a turn: the request alone. Version 1
/// also appended the fixed text `local content read`, which matched unrelated
/// memories; replay rebuilds that legacy form for version-1 turns.
pub(super) fn turn_signature(text: &str) -> TaskSignature {
    TaskSignature {
        request: text.to_owned(),
        ..TaskSignature::default()
    }
}

fn context_compile_reason(error: &ContextCompileError) -> Option<TurnFailureReason> {
    match error {
        ContextCompileError::DuplicateCandidate { .. } => {
            Some(TurnFailureReason::DuplicateContextCandidate)
        }
        ContextCompileError::InvalidPolicyReason { .. } => {
            Some(TurnFailureReason::EmptyPolicyReason)
        }
        ContextCompileError::InvalidRequiredContext { .. } => {
            Some(TurnFailureReason::InvalidRequiredContext)
        }
        ContextCompileError::RequiredContextBudgetExceeded { .. } => {
            Some(TurnFailureReason::RequiredContextOverBudget)
        }
        // The five-field compiler builds no shared retrieval query.
        ContextCompileError::Retrieval(_) => None,
    }
}

/// Map the driver-contract errors a turn request can meet. Any other variant
/// stays untyped and therefore unreplayable, exactly as in version 1.
fn driver_contract_reason(error: &ModelContractError) -> Option<TurnFailureReason> {
    match error {
        ModelContractError::UnsupportedRequiredFeatures { .. } => {
            Some(TurnFailureReason::DriverFeaturesUnsupported)
        }
        ModelContractError::UnsupportedGenerationControl { control, .. }
            if *control == "tool_use.choice" =>
        {
            Some(TurnFailureReason::DriverToolChoiceUnsupported)
        }
        ModelContractError::UnsupportedGenerationControl { control, .. }
            if *control == "tool_use.parallel_calls" =>
        {
            Some(TurnFailureReason::DriverParallelCallsUnsupported)
        }
        _ => None,
    }
}

fn artifact_read_argument_error(arguments: &Value) -> ArtifactReadError {
    if let Some(reference) = arguments.get("reference").and_then(Value::as_str)
        && ArtifactRef::new(reference.to_owned()).is_err()
    {
        return ArtifactReadError::invalid_reference();
    }
    ArtifactReadError::invalid_arguments()
}

fn build_model_request(
    scope: &TurnScope,
    request_index: usize,
    execution_epoch_id: ExecutionEpochId,
    context: ContextCapsule,
    tools: Vec<CapabilitySchema>,
    conversation: Vec<ConversationItem>,
    deadline: DateTime<Utc>,
) -> Result<ModelRequest, String> {
    let request_id = ModelRequestId::new(format!("model_request_{}", Ulid::new()))
        .map_err(|error| error.to_string())?;
    let cancellation_id =
        CancellationId::new(scope.turn_id.clone()).map_err(|error| error.to_string())?;
    let mut required = BTreeSet::new();
    required.insert(ModelFeature::Text);
    required.insert(ModelFeature::ToolCalls);

    let mut request = ModelRequest::new(
        request_id,
        execution_epoch_id,
        stable_system_prefix(),
        ModelTurn {
            conversation,
            context,
            output: OutputConstraint::Text,
        },
    );
    request.tools = tools;
    request.features = FeatureRequest {
        required,
        preferred: BTreeSet::new(),
    };
    request.generation = GenerationControls {
        reasoning: None,
        prompt_cache: Default::default(),
        tool_use: ToolUsePolicy {
            choice: if request_index == 0 && !scope.agent_run {
                ToolChoice::Required
            } else {
                ToolChoice::Auto
            },
            parallel_calls: ParallelToolCalls::Forbid,
        },
    };
    request.control = RequestControl {
        cancellation_id: Some(cancellation_id),
        deadline: Some(deadline),
    };
    Ok(request)
}
fn deadline_expired(deadline: DateTime<Utc>) -> bool {
    Utc::now() >= deadline
}

fn floor_to_millis(value: DateTime<Utc>) -> DateTime<Utc> {
    DateTime::from_timestamp_millis(value.timestamp_millis())
        .expect("a current UTC timestamp is representable at millisecond precision")
}

fn ceil_to_millis(value: DateTime<Utc>) -> DateTime<Utc> {
    let millis = value.timestamp_millis();
    let floor = DateTime::from_timestamp_millis(millis)
        .expect("a current UTC timestamp is representable at millisecond precision");
    if floor < value {
        DateTime::from_timestamp_millis(millis.saturating_add(1))
            .expect("a current UTC timestamp plus one millisecond is representable")
    } else {
        floor
    }
}

fn duration_until(deadline: DateTime<Utc>) -> std::time::Duration {
    (deadline - Utc::now()).to_std().unwrap_or_default()
}
