use std::{
    cell::{Cell, RefCell},
    collections::BTreeSet,
    sync::OnceLock,
};

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use ditto_artifact_read::{
    ARTIFACT_READ_ID, ARTIFACT_READ_VERSION, ArtifactReadAuthority, ArtifactReadDeriver,
    ArtifactReadError, ArtifactReadResource, ArtifactReadResult, capability_schema,
    validate_artifact_read_manifest,
};
use ditto_artifact_store::ArtifactRef;
use ditto_capability::{
    CapabilityDeriver, CapabilitySchema, InvocableContract, InvocationCompiler, InvocationError,
    LiveExecutionEpoch, UntrustedToolCall, UntrustedToolCallError,
};
use ditto_context::{
    CompiledContext, ContextCandidate, ContextCapsule, ContextCompileError, ContextCompiler,
    ContextSelection, TaskSignature,
};
use ditto_model::{
    CancellationToken, ContentPart, ConversationItem, ExecutionEpochId, FinishReason,
    ModelContractError, ModelDriver, ModelEvent, ModelRequest, ModelRequestId, ProviderCallId,
    StableSystemPrefix, ToolCallBuffer,
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

#[path = "fetch_run.rs"]
mod fetch_tool;

use crate::{DittoKernel, KernelError, normalize_identifier, normalize_input_text};

use super::shared::{
    Checkpoint, HistoryExchange, ReadyCall, RequestInputs, append_assistant_text,
    bounded_turn_failure_message, history_messages, latest_user_text, local_utc_offset_minutes,
    model_request, presented_context, request_sha256, system_prefix, turn_failure_code_for_model,
};
use super::types::{
    ArtifactReadTurnOutcome, ArtifactReadTurnStatus, CapabilitiesSelectedPayload,
    CapabilityRequestedPayload, ContextCompiledPayload, ExecutionOutputPayload,
    ExecutionStartedPayload, MAX_ASSISTANT_TEXT_BYTES, MAX_MODEL_EVENTS_PER_REQUEST,
    MAX_MODEL_OUTPUT_BYTES_PER_REQUEST, MAX_MODEL_OUTPUT_EVENT_BYTES, MAX_MODEL_REQUESTS,
    MAX_TURN_DURATION, ModelOutputPayload, ModelRequestDigestPayload, ReadOnlyTurnControl,
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
    /// Whether `web.fetch` is offered. From version 6 it is part of every
    /// agent run's stable tool surface while enabled.
    fetch_offered: bool,
    /// URLs from the user's message that `web.fetch` may read.
    fetch: Vec<String>,
    /// Prelude transitions awaiting the turn's next append, which commits
    /// them with it in one transaction (ADR 0028 Phase B).
    staged: RefCell<Vec<(String, NewEvent)>>,
    /// Streamed text admitted but not yet durable (turn payload version 7):
    /// the turn's next append or the chunk's flush deadline commits it.
    pending_text: RefCell<Option<PendingText>>,
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
    /// System instructions, fixed when the context is compiled.
    system_prefix: StableSystemPrefix,
    /// The host's UTC offset recorded with the context; it fixes the local
    /// time the latest message's note states.
    utc_offset_minutes: i32,
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

/// Streamed text commits as one `model.output` chunk at most this long after
/// its first delta. Text after a quiet interval of the same length commits at
/// once, so a slow stream is never delayed and a fast one writes about twenty
/// chunks a second.
const TEXT_FLUSH_INTERVAL: std::time::Duration = std::time::Duration::from_millis(48);
/// A chunk commits before it would pass this much text.
const TEXT_CHUNK_BYTES: usize = 2 * 1_024;

/// Coalesced text awaiting its commit, with its pre-assigned event ID so later
/// transitions can name it as their cause.
#[derive(Clone)]
struct PendingText {
    event_id: String,
    causation_id: String,
    output: ModelOutputPayload,
    encoded_bytes: usize,
    opened: tokio::time::Instant,
}

/// How one admitted provider event enters the journal.
enum OutputAdmission {
    /// Text extending the pending chunk, with the chunk's new encoded size.
    Extend(Box<ModelOutputPayload>, usize),
    /// Text opening a new chunk after the pending one commits.
    Open,
    /// Any other output, committed individually after pending text.
    Append,
}

impl TurnScope {
    fn pending_text_flush_at(&self) -> Option<tokio::time::Instant> {
        self.pending_text
            .borrow()
            .as_ref()
            .map(|pending| pending.opened + TEXT_FLUSH_INTERVAL)
    }

    fn pending_text_bytes(&self) -> usize {
        self.pending_text
            .borrow()
            .as_ref()
            .map_or(0, |pending| pending.encoded_bytes)
    }

    /// The pending chunk extended by one text delta, unless that would pass
    /// `TEXT_CHUNK_BYTES` or nothing is pending.
    fn pending_text_with(
        &self,
        delta: &ModelOutputPayload,
    ) -> Result<Option<(ModelOutputPayload, usize)>, serde_json::Error> {
        let pending = self.pending_text.borrow();
        let Some(pending) = pending.as_ref() else {
            return Ok(None);
        };
        let (ModelEvent::TextDelta { text: joined }, ModelEvent::TextDelta { text }) = (
            &pending.output.stream_event.event,
            &delta.stream_event.event,
        ) else {
            return Ok(None);
        };
        if joined.len() + text.len() > TEXT_CHUNK_BYTES {
            return Ok(None);
        }
        let mut extended = pending.output.clone();
        extended.stream_event.event = ModelEvent::TextDelta {
            text: format!("{joined}{text}"),
        };
        extended.through_sequence = Some(delta.stream_event.sequence);
        extended.admitted_at = delta.admitted_at;
        let bytes = serde_json::to_vec(&extended)?.len();
        Ok(Some((extended, bytes)))
    }

    fn extend_pending_text(&self, extended: ModelOutputPayload, encoded_bytes: usize) {
        if let Some(pending) = self.pending_text.borrow_mut().as_mut() {
            pending.output = extended;
            pending.encoded_bytes = encoded_bytes;
        }
    }

    fn open_pending_text(&self, pending: PendingText) {
        let previous = self.pending_text.replace(Some(pending));
        debug_assert!(
            previous.is_none(),
            "pending text commits before the next chunk opens"
        );
    }

    /// Staged prelude and pending text, in journal order, ready to commit.
    fn take_uncommitted(&self) -> Result<Vec<(String, NewEvent)>, TurnRunError> {
        let mut batch = self.staged.take();
        if let Some(pending) = self.pending_text.take() {
            let span = pending.output.request_id.to_string();
            let event = turn_event(
                self,
                EventActor::Model,
                event_kind::MODEL_OUTPUT,
                &pending.output,
                Some(pending.causation_id),
                Some(span),
            )?;
            batch.push((pending.event_id, event));
        }
        Ok(batch)
    }
}

/// Builtin tool contracts, paged and validated on first use and then reused
/// for the process: the startup header pins each package digest, so a
/// verified contract cannot change underneath a running kernel. Failures are
/// not kept; the next turn pages again.
#[derive(Default)]
pub(crate) struct ToolContracts {
    read: OnceLock<InvocableContract>,
    fetch: OnceLock<InvocableContract>,
    sort: OnceLock<InvocableContract>,
}

fn cached_contract<E>(
    cell: &OnceLock<InvocableContract>,
    build: impl FnOnce() -> Result<InvocableContract, E>,
) -> Result<&InvocableContract, E> {
    if let Some(contract) = cell.get() {
        return Ok(contract);
    }
    let contract = build()?;
    Ok(cell.get_or_init(|| contract))
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
            fetch_offered: agent_run.is_some() && self.inner.web_fetch.is_some(),
            fetch: if agent_run.is_some() && self.inner.web_fetch.is_some() {
                super::fetch::grant(&text)
            } else {
                Vec::new()
            },
            staged: RefCell::default(),
            pending_text: RefCell::default(),
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
            system_prefix: StableSystemPrefix::default(),
            utc_offset_minutes: 0,
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
        let (context_candidates, sources_verified) = match context_candidates {
            Some(candidates) => (candidates.into_iter().collect(), false),
            None => match self.agent_context_candidates(
                &run.scope.session_id,
                &run.scope.task_id,
                run.accepted_at,
            ) {
                Ok(context) => (context.candidates, context.sources_verified),
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
        // Session-wide sources were checked once when the context was taken.
        let provenance = if sources_verified {
            Ok(())
        } else {
            self.validate_compiled_context_provenance(&run.scope, &compiled, provenance_through_seq)
        };
        match provenance {
            Ok(()) => {}
            Err(ContextProvenanceError::Kernel(error)) => return Err(TurnRunError::Kernel(error)),
            Err(ContextProvenanceError::Invalid(reason, message)) => {
                return Err(self.fail_with(run, reason, message, None, None));
            }
        }
        let history = if run.scope.agent_run {
            self.conversation_history(&run.scope.session_id, input_event.seq)
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
        let utc_offset_minutes = local_utc_offset_minutes(run.accepted_at);
        run.utc_offset_minutes = utc_offset_minutes;
        run.system_prefix = system_prefix(
            TURN_PAYLOAD_VERSION,
            run.accepted_at,
            Some(utc_offset_minutes),
        )
        .ok_or(TurnRunError::Internal("host UTC offset is out of range"))?;
        self.stage_turn_event(
            run,
            EventActor::System,
            event_kind::CONTEXT_COMPILED,
            &ContextCompiledPayload {
                event_version: TURN_PAYLOAD_VERSION,
                turn_id: run.scope.turn_id.clone(),
                provenance_through_seq,
                compiled,
                capsule: None,
                history_turn_ids: history
                    .iter()
                    .map(|exchange| exchange.turn_id.clone())
                    .collect(),
                utc_offset_minutes: Some(utc_offset_minutes),
            },
            None,
        )?;
        Ok((capsule, history))
    }

    /// Page the permitted capabilities into one sealed execution epoch,
    /// journal the selection, and derive its authorization ledger.
    fn select_turn_tools(
        &self,
        run: &mut TurnRun<'_>,
        input_event: &EventRecord,
    ) -> Result<TurnTools, TurnRunError> {
        let read = cached_contract(&self.inner.tool_contracts.read, || {
            self.artifact_read_contract(run)
        })?;
        // An unavailable or altered web.fetch package withdraws the tool
        // instead of failing the turn; the epoch is sized after the decision.
        let fetch = if run.scope.fetch_offered {
            cached_contract(&self.inner.tool_contracts.fetch, || {
                self.inner
                    .capabilities
                    .page_manifest(ditto_web_fetch::ID)
                    .ok()
                    .flatten()
                    .filter(ditto_web_fetch::validate_manifest)
                    .and_then(|manifest| {
                        InvocableContract::new(
                            &manifest,
                            &ditto_web_fetch::schema(),
                            ditto_web_fetch::FetchDeriver::default().revision().clone(),
                        )
                        .ok()
                    })
                    .ok_or(())
            })
            .ok()
        } else {
            None
        };
        if fetch.is_none() {
            run.scope.fetch_offered = false;
            run.scope.fetch.clear();
        }
        let mut live_epoch = LiveExecutionEpoch::new(
            1 + usize::from(run.scope.sort.is_some()) + usize::from(fetch.is_some()),
        );
        let paged = live_epoch.page_in_contract(read).map_err(|error| {
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
        let sort = if let Some(grant) = &run.scope.sort {
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
            let Ok(contract) = cached_contract(&self.inner.tool_contracts.sort, || {
                self.inner
                    .capabilities
                    .page_manifest(ditto_artifact_sort::ID)
                    .ok()
                    .flatten()
                    .filter(|manifest| ditto_artifact_sort::validate_manifest(manifest).is_ok())
                    .and_then(|manifest| {
                        InvocableContract::new(
                            &manifest,
                            &ditto_artifact_sort::schema(),
                            ditto_artifact_sort::SortDeriver::default()
                                .revision()
                                .clone(),
                        )
                        .ok()
                    })
                    .ok_or(())
            }) else {
                return Err(self.fail_with(
                    run,
                    TurnFailureReason::SortContractUnavailable,
                    "installed artifact.sort contract is unavailable",
                    None,
                    None,
                ));
            };
            live_epoch.page_in_contract(contract).map_err(|_| {
                TurnRunError::Internal("sort capability could not enter the live epoch")
            })?;
            Some(contract)
        } else {
            None
        };
        if let Some(contract) = fetch {
            live_epoch.page_in_contract(contract).map_err(|_| {
                TurnRunError::Internal("fetch capability could not enter the live epoch")
            })?;
        }
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
        schemas.extend(sort.map(|contract| contract.schema().clone()));
        schemas.extend(fetch.map(|contract| contract.schema().clone()));
        let sort_manifest = sort.map(|contract| contract.manifest().clone());
        let fetch_manifest = fetch.map(|contract| contract.manifest().clone());
        self.stage_turn_event(
            run,
            EventActor::System,
            event_kind::CAPABILITIES_SELECTED,
            &CapabilitiesSelectedPayload {
                event_version: TURN_PAYLOAD_VERSION,
                turn_id: run.scope.turn_id.clone(),
                manifest,
                sort_manifest,
                fetch_manifest,
                epoch: live_epoch.evidence().clone(),
                schemas: Vec::new(),
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
        if !run.scope.fetch.is_empty() {
            let resources = run
                .scope
                .fetch
                .iter()
                .map(|url| {
                    ditto_capability::CanonicalResource::url(url.clone())
                        .map(ditto_policy::ResourceScope::Exact)
                })
                .collect::<Result<Vec<_>, _>>()
                .map_err(|_| TurnRunError::Internal("invalid fetch grant"))?;
            authorizer
                .register_lease(
                    ditto_policy::CapabilityLease::new(
                        "agent-fetch",
                        run.deadline,
                        ditto_web_fetch::effect(),
                        super::fetch::call_budget(&run.scope.fetch),
                        BTreeSet::from([ditto_web_fetch::ID.into()]),
                        resources,
                        ditto_policy::ApprovalRequirement::Never,
                    )
                    .map_err(|_| TurnRunError::Internal("invalid fetch lease"))?,
                )
                .map_err(|_| TurnRunError::Internal("fetch lease registration failed"))?;
        }

        Ok(TurnTools {
            live_epoch,
            authorizer,
            execution_epoch_id,
            schemas,
            authority: ArtifactReadAuthority::new(self.inner.artifacts.clone()),
        })
    }

    /// Page and validate the installed `artifact.read` contract. Every failure
    /// is a typed, journaled turn failure.
    fn artifact_read_contract(&self, run: &TurnRun<'_>) -> Result<InvocableContract, TurnRunError> {
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
        InvocableContract::new(
            &manifest,
            &schema,
            ArtifactReadDeriver::default().revision().clone(),
        )
        .map_err(|error| {
            self.fail_with(
                run,
                TurnFailureReason::ArtifactReadSelectionFailed,
                error.to_string(),
                None,
                None,
            )
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
        let text = latest_user_text(
            TURN_PAYLOAD_VERSION,
            &text,
            run.accepted_at,
            Some(run.utc_offset_minutes),
        )
        .ok_or(TurnRunError::Internal("host UTC offset is out of range"))?;
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
        let request = ModelRequestId::new(format!("model_request_{}", Ulid::new()))
            .map_err(|error| error.to_string())
            .and_then(|request_id| {
                model_request(RequestInputs {
                    request_id,
                    execution_epoch_id: tools.execution_epoch_id.clone(),
                    system_prefix: run.system_prefix.clone(),
                    context: presented_context(TURN_PAYLOAD_VERSION, capsule),
                    tools: tools.schemas.clone(),
                    conversation: conversation.to_vec(),
                    first_tool_required: request_index == 0 && !run.scope.agent_run,
                    turn_id: run.scope.turn_id.clone(),
                    deadline: run.deadline,
                })
            })
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

        // Replay rebuilds the rest of the request from earlier durable events.
        let requested = self.append_turn_event(
            run,
            EventActor::System,
            event_kind::MODEL_REQUESTED,
            &ModelRequestDigestPayload {
                event_version: TURN_PAYLOAD_VERSION,
                turn_id: run.scope.turn_id.clone(),
                request_index: request_index as u8,
                request_id: request.request_id.clone(),
                deadline: run.deadline,
                request_sha256: request_sha256(&request),
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
        // Encoded bytes of this request's committed outputs; pending text adds
        // its own, and the two together never pass the request's bound.
        let mut model_output_bytes = 0_usize;
        let mut tool_buffer = ToolCallBuffer::default();
        let mut ready_call: Option<ReadyCall> = None;
        let mut request_text = String::new();
        // When text last became durable. Text after a quiet interval commits
        // at once, so coalescing never delays the first words of a burst.
        let mut text_committed_at: Option<tokio::time::Instant> = None;

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
            let flush_at = run.scope.pending_text_flush_at();
            let flush_wait = async move {
                match flush_at {
                    Some(at) => tokio::time::sleep_until(at).await,
                    None => std::future::pending().await,
                }
            };
            tokio::pin!(flush_wait);
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
                () = &mut flush_wait => {
                    model_output_bytes += self.commit_pending_text(&run.scope)?;
                    text_committed_at = Some(tokio::time::Instant::now());
                    continue;
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
            let output_payload = ModelOutputPayload {
                event_version: TURN_PAYLOAD_VERSION,
                turn_id: run.scope.turn_id.clone(),
                request_index: request_index as u8,
                request_id: request.request_id.clone(),
                admitted_at,
                stream_event: stream_event.clone(),
                through_sequence: None,
            };
            let encoded_output_bytes = serde_json::to_vec(&output_payload)?.len();
            // Text joins the pending chunk, or opens the next one; any other
            // output commits after the pending chunk, in the same transaction.
            let admission = match &stream_event.event {
                ModelEvent::TextDelta { .. } => {
                    match run.scope.pending_text_with(&output_payload)? {
                        Some((extended, bytes)) => {
                            OutputAdmission::Extend(Box::new(extended), bytes)
                        }
                        None => OutputAdmission::Open,
                    }
                }
                _ => OutputAdmission::Append,
            };
            // Bytes this output leaves pending or newly committed beyond the
            // committed total; a chunk holds at most one delta past
            // `TEXT_CHUNK_BYTES`, so only single events can reach the event bound.
            let prospective_bytes = match &admission {
                OutputAdmission::Extend(_, bytes) => *bytes,
                OutputAdmission::Open | OutputAdmission::Append => {
                    run.scope.pending_text_bytes() + encoded_output_bytes
                }
            };
            self.ensure_output_bounds(
                run,
                index,
                model_output_bytes + prospective_bytes.saturating_sub(encoded_output_bytes),
                encoded_output_bytes,
            )?;
            self.ensure_live(run, Checkpoint::AwaitingModelOutput, index, None)?;
            event_count += 1;

            match admission {
                OutputAdmission::Extend(extended, bytes) => {
                    run.scope.extend_pending_text(*extended, bytes);
                }
                OutputAdmission::Open => {
                    model_output_bytes += self.commit_pending_text(&run.scope)?;
                    let now = tokio::time::Instant::now();
                    let event_id = Ulid::new().to_string();
                    run.scope.open_pending_text(PendingText {
                        event_id: event_id.clone(),
                        causation_id: run.cause.clone(),
                        output: output_payload,
                        encoded_bytes: encoded_output_bytes,
                        opened: now,
                    });
                    run.cause = event_id;
                    if text_committed_at.is_none_or(|at| now - at >= TEXT_FLUSH_INTERVAL) {
                        model_output_bytes += self.commit_pending_text(&run.scope)?;
                        text_committed_at = Some(now);
                    }
                }
                OutputAdmission::Append => {
                    model_output_bytes += run.scope.pending_text_bytes();
                    self.append_turn_event(
                        run,
                        EventActor::Model,
                        event_kind::MODEL_OUTPUT,
                        &output_payload,
                        Some(request.request_id.to_string()),
                    )?;
                    model_output_bytes += encoded_output_bytes;
                }
            }

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
        let (value, is_error) = if call.capability_id == ditto_web_fetch::ID {
            let binding = tools
                .live_epoch
                .invocable_binding(ditto_web_fetch::ID)
                .ok_or(TurnRunError::Internal("missing fetch binding"))?;
            let result = self
                .run_fetch_tool(
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
        } else if call.capability_id == ditto_artifact_sort::ID {
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
            || (run.scope.fetch_offered && capability_id == ditto_web_fetch::ID)
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

    /// Commit pending streamed text, with anything staged before it. Returns
    /// the committed chunk's encoded bytes.
    fn commit_pending_text(&self, scope: &TurnScope) -> Result<usize, TurnRunError> {
        let bytes = scope.pending_text_bytes();
        let batch = scope.take_uncommitted()?;
        if !batch.is_empty() {
            self.append_and_publish_batch(batch)?;
        }
        Ok(bytes)
    }

    /// Stage a prelude transition caused by the previous one; the turn's next
    /// append, its first model request or its failure, commits it.
    fn stage_turn_event<T: Serialize>(
        &self,
        run: &mut TurnRun<'_>,
        actor: EventActor,
        kind: &str,
        payload: &T,
        span_id: Option<String>,
    ) -> Result<(), TurnRunError> {
        let event_id = Ulid::new().to_string();
        let event = turn_event(
            &run.scope,
            actor,
            kind,
            payload,
            Some(run.cause.clone()),
            span_id,
        )?;
        run.scope
            .staged
            .borrow_mut()
            .push((event_id.clone(), event));
        run.cause = event_id;
        Ok(())
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
        let event = turn_event(scope, actor, kind, payload, causation_id, span_id)?;
        let mut batch = scope.take_uncommitted()?;
        if batch.is_empty() {
            return Ok(self.append_and_publish(event)?);
        }
        batch.push((Ulid::new().to_string(), event));
        Ok(self
            .append_and_publish_batch(batch)?
            .pop()
            .expect("a committed batch returns its last event"))
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

fn turn_event<T: Serialize>(
    scope: &TurnScope,
    actor: EventActor,
    kind: &str,
    payload: &T,
    causation_id: Option<String>,
    span_id: Option<String>,
) -> Result<NewEvent, TurnRunError> {
    Ok(NewEvent {
        session_id: Some(scope.session_id.clone()),
        task_id: Some(scope.task_id.clone()),
        actor,
        kind: kind.into(),
        payload: serde_json::to_value(payload)?,
        causation_id,
        correlation_id: Some(scope.turn_id.clone()),
        span_id,
    })
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
