use std::collections::BTreeSet;

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use ditto_artifact_read::{
    ARTIFACT_READ_ID, ARTIFACT_READ_VERSION, ArtifactReadDeriver, ArtifactReadError,
    ArtifactReadResource, ArtifactReadResult, capability_schema, validate_artifact_read_manifest,
};
use ditto_capability::{CapabilityCard, CapabilityDeriver, CapabilityRevision, CapabilitySchema};
use ditto_context::{CompiledContext, ContextCapsule, ContextCompiler, ContextSelection};
use ditto_model::{
    CancellationId, ContentPart, ConversationItem, ExecutionEpochId, FinishReason,
    GenerationControls, ModelEvent, ModelFeature, ModelRequest, OutputConstraint,
    ParallelToolCalls, ProviderCallId, StableSystemPrefix, ToolCallBuffer, ToolCallError,
    ToolChoice, ToolUsePolicy,
};
use ditto_protocol::{EventActor, EventRecord, event_kind};
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::Value;

use crate::normalize_input_text;

#[path = "sort_replay.rs"]
mod sort_replay;
use super::sort::{ReplayedSortCall, SortGrant};

#[path = "fetch_replay.rs"]
mod fetch_replay;
use super::fetch::ReplayedFetchCall;

#[path = "recall_replay.rs"]
mod recall_replay;
use super::recall::ReplayedRecallCall;

#[path = "memory_write_replay.rs"]
mod memory_write_replay;

#[path = "search_replay.rs"]
mod search_replay;
use super::memory_write::{FORGET_ID, REMEMBER_ID, ReplayedMemoryWrite};
use super::search::ReplayedSearchCall;

use super::run::turn_signature;
use super::shared::{
    Checkpoint, HistoryExchange, MAX_HISTORY_CANDIDATES, ReadyCall, RequestInputs, agent_run_text,
    append_assistant_text, bounded_turn_failure_message, history_messages, latest_user_text,
    model_request, presented_context, request_sha256, select_history_stepped, system_prefix,
    turn_failure_code_for_model,
};
use super::types::{
    ArtifactReadTurnOutcome, ArtifactReadTurnReplay, ArtifactReadTurnStatus,
    CapabilitiesSelectedPayload, CapabilitiesSelectedRefPayload, CapabilityRequestedPayload,
    ContextCompiledPayload, ExecutionOutputPayload, ExecutionStartedPayload,
    MAX_ASSISTANT_TEXT_BYTES, MAX_MODEL_EVENTS_PER_REQUEST, MAX_MODEL_OUTPUT_BYTES_PER_REQUEST,
    MAX_MODEL_OUTPUT_EVENT_BYTES, MAX_MODEL_REQUESTS, MAX_TURN_DURATION,
    MAX_TURN_FAILURE_MESSAGE_BYTES, ModelOutputPayload, ModelRequestDigestPayload,
    ModelRequestedPayload, ReplayError, ReplayedArtifactReadCall, ReplayedReadOnlyTurn,
    TURN_PAYLOAD_VERSION, TurnFailedPayload, TurnFailure, TurnFailureCode, TurnFailureEvidence,
    TurnFailureReason, TurnFinishedPayload, TurnSequenceSpan,
};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct InputPayload {
    text: String,
    #[serde(default)]
    agent_run: Option<crate::agent_run::AgentRunMetadata>,
}

/// Validate recorded context with the complete-set selection contract on the
/// request text (ADR 0021).
fn validate_compiled_context_payload(
    compiled: &CompiledContext,
    capsule: &ContextCapsule,
    input_text: &str,
    accepted_at: DateTime<Utc>,
) -> Result<(), ReplayError> {
    ContextCompiler::default()
        .validate_compiled_with(
            ContextSelection::CompleteSet,
            &turn_signature(input_text),
            compiled,
            capsule,
            None,
            accepted_at,
        )
        .map_err(|error| replay_invalid(error.to_string()))
}
fn validate_execution_result(
    normalized: &Result<ArtifactReadResource, ArtifactReadError>,
    result: &ArtifactReadResult,
    authorized: bool,
) -> Result<(), ReplayError> {
    match normalized {
        Err(error) => {
            if result != &ArtifactReadResult::error(error.clone()) {
                return Err(replay_invalid(
                    "execution output does not equal the deterministic normalization error",
                ));
            }
        }
        Ok(resource) => {
            if let Some(success) = result.success_projection() {
                if !authorized {
                    return Err(replay_invalid(
                        "artifact success has no authorized same-scope root",
                    ));
                }
                let data = success
                    .decoded_data()
                    .map_err(|_| replay_invalid("artifact result contains invalid base64"))?;
                let returned = u64::try_from(data.len())
                    .map_err(|_| replay_invalid("artifact result length cannot be represented"))?;
                if success.reference() != resource.reference()
                    || success.offset() != resource.offset()
                    || success.requested_bytes() != resource.length()
                    || success.returned_bytes() != returned
                    || success.returned_bytes() > resource.length()
                    || success.offset() > success.total_bytes()
                    || success.returned_bytes()
                        != resource
                            .length()
                            .min(success.total_bytes().saturating_sub(resource.offset()))
                    || success.eof()
                        != (success.offset().saturating_add(success.returned_bytes())
                            == success.total_bytes())
                {
                    return Err(replay_invalid(
                        "artifact success result contradicts the normalized resource",
                    ));
                }
            } else {
                let error = result
                    .error_projection()
                    .ok_or_else(|| replay_invalid("artifact result has no projection"))?;
                let expected_message = match error.code() {
                    "range_out_of_bounds" => "artifact offset is beyond the end of the artifact",
                    "artifact_unavailable" => "artifact is unavailable",
                    "integrity_failure" => "artifact integrity verification failed",
                    "unauthorized_reference" => {
                        "artifact reference is not authorized for this turn"
                    }
                    _ => {
                        return Err(replay_invalid(
                            "normalized artifact execution emitted an impossible error code",
                        ));
                    }
                };
                if error.reference() != Some(resource.reference())
                    || error.message() != expected_message
                    || (error.code() == "unauthorized_reference") == authorized
                {
                    return Err(replay_invalid(
                        "artifact error result contradicts the normalized resource",
                    ));
                }
            }
        }
    }
    Ok(())
}

/// Reconstruct and verify a complete Task 003 turn from durable records only.
///
/// This function is deliberately pure with respect to providers and artifact
/// storage. `snapshot` is an ordered durable scope snapshot that contains the
/// selected turn plus any earlier source/root events needed to verify it.
pub fn replay_artifact_read_turn(
    snapshot: &[EventRecord],
    turn_id: &str,
) -> Result<ReplayedReadOnlyTurn, ReplayError> {
    CancellationId::new(turn_id.to_owned()).map_err(|error| replay_invalid(error.to_string()))?;
    validate_ordered_snapshot(snapshot)?;
    let turn_events = snapshot
        .iter()
        .filter(|event| event.correlation_id.as_deref() == Some(turn_id))
        .cloned()
        .collect::<Vec<_>>();
    let target_task_id = turn_events
        .iter()
        .find(|event| event.kind == event_kind::INPUT_RECEIVED)
        .and_then(|event| event.task_id.as_deref())
        .ok_or_else(|| replay_invalid("requested turn has no scoped input event"))?;
    if snapshot.iter().any(|event| {
        event.kind == event_kind::TASK_COMPLETED && event.task_id.as_deref() == Some(target_task_id)
    }) {
        return Err(replay_invalid(
            "artifact read task snapshot must not contain task.completed",
        ));
    }
    if turn_events
        .iter()
        .any(|event| event.kind == event_kind::TASK_COMPLETED)
    {
        return Err(replay_invalid(
            "artifact read turns must never contain task.completed",
        ));
    }
    let mut projector = ReplayProjector::new(&turn_events, snapshot, turn_id)?;
    let terminal = projector.replay()?;
    Ok(projector.finish_projection(terminal))
}

struct ReplayProjector<'turn, 'snapshot> {
    /// Turn payload version, fixed by the first versioned payload; every later
    /// payload of the turn must carry the same version.
    version: Option<u16>,
    agent_run: bool,
    sort: Option<SortGrant>,
    sort_claimed: bool,
    sort_calls: Vec<ReplayedSortCall>,
    /// URLs the user's message grants to `web.fetch`, recomputed from it.
    fetch_grant: Vec<String>,
    /// Whether the recorded selection paged `web.fetch`.
    fetch_selected: bool,
    fetch_claims: u32,
    fetch_calls: Vec<ReplayedFetchCall>,
    /// Whether the recorded selection paged `memory.search`.
    memory_selected: bool,
    /// Version 8: the memories `memory.search` read, rebuilt at the first
    /// search.
    recall_space: Option<Vec<ditto_context::ContextNode>>,
    recall_calls: Vec<ReplayedRecallCall>,
    /// Version 10: whether the selection paged `memory.remember` and
    /// `memory.forget`, whether a tool returned a web page or file content
    /// yet, and the memory writes made (ADR 0031).
    remember_selected: bool,
    forget_selected: bool,
    read_external_content: bool,
    memory_write_count: u32,
    memory_writes: Vec<ReplayedMemoryWrite>,
    /// Version 11: whether the selection paged `web.search`, and its calls.
    search_selected: bool,
    search_claims: u32,
    search_calls: Vec<ReplayedSearchCall>,
    events: &'turn [EventRecord],
    snapshot: &'snapshot [EventRecord],
    index: usize,
    turn_id: String,
    session_id: String,
    task_id: String,
    context: Option<ContextCapsule>,
    schemas: Option<Vec<CapabilitySchema>>,
    execution_epoch_id: Option<ExecutionEpochId>,
    conversation: Vec<ConversationItem>,
    all_call_ids: BTreeSet<ProviderCallId>,
    total_text_bytes: usize,
    tool_call_count: u8,
    request_count: u8,
    deadline: Option<DateTime<Utc>>,
    input_recorded_at: DateTime<Utc>,
    input_text: String,
    context_payload: Option<ContextCompiledPayload>,
    /// System instructions recomputed from the recorded context payload.
    system_prefix: Option<StableSystemPrefix>,
    capabilities_payload: Option<CapabilitiesSelectedPayload>,
    requests: Vec<ModelRequestedPayload>,
    outputs: Vec<ModelOutputPayload>,
    calls: Vec<ReplayedArtifactReadCall>,
}

impl<'turn, 'snapshot> ReplayProjector<'turn, 'snapshot> {
    fn new(
        events: &'turn [EventRecord],
        snapshot: &'snapshot [EventRecord],
        requested_turn_id: &str,
    ) -> Result<Self, ReplayError> {
        let first = events
            .first()
            .ok_or_else(|| replay_invalid("event slice is empty"))?;
        if events
            .iter()
            .any(|event| event.kind == event_kind::TASK_COMPLETED)
        {
            return Err(replay_invalid(
                "artifact read turns must never contain task.completed",
            ));
        }
        if first.kind != event_kind::INPUT_RECEIVED
            || first.actor != EventActor::User
            || first.span_id.is_some()
        {
            return Err(replay_invalid(
                "turn must begin with trusted input.received",
            ));
        }
        let turn_id = first
            .correlation_id
            .clone()
            .filter(|value| !value.is_empty())
            .ok_or_else(|| replay_invalid("turn correlation id is missing"))?;
        if turn_id != requested_turn_id {
            return Err(replay_invalid("requested turn id does not match its input"));
        }
        let session_id = first
            .session_id
            .clone()
            .ok_or_else(|| replay_invalid("turn session id is missing"))?;
        if snapshot
            .iter()
            .any(|event| event.session_id.as_deref() != Some(session_id.as_str()))
        {
            return Err(replay_invalid(
                "replay snapshot contains records outside the turn session",
            ));
        }
        let task_id = first
            .task_id
            .clone()
            .ok_or_else(|| replay_invalid("turn task id is missing"))?;

        let mut event_ids = BTreeSet::new();
        let mut previous_seq = None;
        let mut previous_id: Option<&str> = None;
        for event in events {
            if previous_seq.is_some_and(|sequence| event.seq <= sequence) {
                return Err(replay_invalid("event sequence is duplicated or reordered"));
            }
            if !event_ids.insert(event.event_id.as_str()) {
                return Err(replay_invalid("event id is duplicated"));
            }
            if event.session_id.as_deref() != Some(session_id.as_str())
                || event.task_id.as_deref() != Some(task_id.as_str())
                || event.correlation_id.as_deref() != Some(turn_id.as_str())
            {
                return Err(replay_invalid("event scope or correlation changed"));
            }
            if event.causation_id.as_deref() != previous_id {
                return Err(replay_invalid("event causation chain is broken"));
            }
            previous_seq = Some(event.seq);
            previous_id = Some(event.event_id.as_str());
        }

        let input: InputPayload = decode_payload(first)?;
        let agent_run = input.agent_run.is_some();
        if let Some(metadata) = &input.agent_run {
            crate::agent_run::validate_agent_input(
                first,
                &ditto_protocol::AgentRunQuery {
                    request_id: metadata.request_id.clone(),
                    session_id: session_id.clone(),
                },
            )
            .map_err(|_| replay_invalid("agent-run input metadata is invalid"))?;
        }
        let normalized_input =
            normalize_input_text(&input.text).map_err(|error| replay_invalid(error.to_string()))?;
        if normalized_input != input.text {
            return Err(replay_invalid("recorded input text is not normalized"));
        }
        let sort = input.agent_run.and_then(|metadata| metadata.sort);
        let input_text = input.text;
        let conversation = super::sort::initial_conversation(input_text.clone(), sort.as_ref());
        let fetch_grant = if agent_run {
            super::fetch::grant(&input_text)
        } else {
            Vec::new()
        };
        Ok(Self {
            version: None,
            agent_run,
            sort,
            sort_claimed: false,
            sort_calls: Vec::new(),
            fetch_grant,
            fetch_selected: false,
            fetch_claims: 0,
            fetch_calls: Vec::new(),
            memory_selected: false,
            recall_space: None,
            recall_calls: Vec::new(),
            remember_selected: false,
            forget_selected: false,
            read_external_content: false,
            memory_write_count: 0,
            memory_writes: Vec::new(),
            search_selected: false,
            search_claims: 0,
            search_calls: Vec::new(),
            events,
            snapshot,
            index: 1,
            turn_id,
            session_id,
            task_id,
            context: None,
            schemas: None,
            execution_epoch_id: None,
            conversation,
            all_call_ids: BTreeSet::new(),
            total_text_bytes: 0,
            tool_call_count: 0,
            request_count: 0,
            deadline: None,
            input_recorded_at: first.recorded_at,
            input_text,
            context_payload: None,
            system_prefix: None,
            capabilities_payload: None,
            requests: Vec::new(),
            outputs: Vec::new(),
            calls: Vec::new(),
        })
    }

    fn replay(&mut self) -> Result<ArtifactReadTurnReplay, ReplayError> {
        if let Some(failure) = self.take_initial_stage_failure()? {
            return Ok(ArtifactReadTurnReplay::Failed { failure });
        }
        let context_event = self.take(event_kind::CONTEXT_COMPILED, EventActor::System)?;
        let context: ContextCompiledPayload = self.decode_versioned(context_event)?;
        self.require_turn_id(&context.turn_id)?;
        if context_event.span_id.is_some() {
            return Err(replay_invalid("context.compiled must not carry a span id"));
        }
        // The compiled context is recorded alone; the capsule derives.
        let capsule = ContextCapsule::from(&context.compiled);
        validate_compiled_context_payload(
            &context.compiled,
            &capsule,
            &self.input_text,
            self.input_recorded_at,
        )?;
        self.validate_context_sources(&context, context_event)?;
        let history = self.conversation_history(&context)?;
        let latest = latest_user_text(
            &self.input_text,
            self.input_recorded_at,
            context.utc_offset_minutes,
        )
        .ok_or_else(|| replay_invalid("context.compiled time offset is out of range"))?;
        let mut conversation = history_messages(&history);
        conversation.extend(super::sort::initial_conversation(
            latest,
            self.sort.as_ref(),
        ));
        self.conversation = conversation;
        self.system_prefix = Some(system_prefix());
        self.context = Some(presented_context(&capsule));
        self.context_payload = Some(context);

        if let Some(failure) = self.take_capability_stage_failure()? {
            return Ok(ArtifactReadTurnReplay::Failed { failure });
        }
        let selected_event = self.take(event_kind::CAPABILITIES_SELECTED, EventActor::System)?;
        let recorded: CapabilitiesSelectedRefPayload = self.decode_versioned(selected_event)?;
        let selected = rebuild_selection(recorded)?;
        self.require_turn_id(&selected.turn_id)?;
        if selected_event.span_id.is_some() {
            return Err(replay_invalid(
                "capabilities.selected must not carry a span id",
            ));
        }
        validate_artifact_read_manifest(&selected.manifest)
            .map_err(|error| replay_invalid(error.to_string()))?;
        let expected_schema = capability_schema();
        let mut expected_schemas = vec![expected_schema.clone()];
        let mut expected_cards = vec![CapabilityCard::from(&selected.manifest)];
        let mut expected_revisions = vec![
            CapabilityRevision::from_contract(
                &selected.manifest,
                &expected_schema,
                ArtifactReadDeriver::default().revision().clone(),
            )
            .map_err(|error| replay_invalid(error.to_string()))?,
        ];
        match (&self.sort, &selected.sort_manifest) {
            (Some(grant), Some(manifest)) => {
                if !self
                    .snapshot
                    .iter()
                    .any(|root| grant.matches_root(root, &self.events[0]))
                {
                    return Err(replay_invalid("sort permission source is unavailable"));
                }
                ditto_artifact_sort::validate_manifest(manifest)
                    .map_err(|_| replay_invalid("sort manifest changed"))?;
                let schema = ditto_artifact_sort::schema();
                expected_revisions.push(
                    CapabilityRevision::from_contract(
                        manifest,
                        &schema,
                        ditto_artifact_sort::SortDeriver::default()
                            .revision()
                            .clone(),
                    )
                    .map_err(|error| replay_invalid(error.to_string()))?,
                );
                expected_schemas.push(schema);
                expected_cards.push(CapabilityCard::from(manifest));
            }
            (None, None) => {}
            _ => {
                return Err(replay_invalid(
                    "selected sort contract contradicts permission",
                ));
            }
        }
        if let Some(manifest) = &selected.fetch_manifest {
            // Part of every agent run's stable tool surface while enabled.
            if !self.agent_run || !ditto_web_fetch::validate_manifest(manifest) {
                return Err(replay_invalid(
                    "selected fetch contract contradicts the user's message",
                ));
            }
            let schema = ditto_web_fetch::schema();
            expected_revisions.push(
                CapabilityRevision::from_contract(
                    manifest,
                    &schema,
                    ditto_web_fetch::FetchDeriver::default().revision().clone(),
                )
                .map_err(|error| replay_invalid(error.to_string()))?,
            );
            expected_schemas.push(schema);
            expected_cards.push(CapabilityCard::from(manifest));
            self.fetch_selected = true;
        }
        if let Some(manifest) = &selected.search_manifest {
            // Offered to agent runs while a search endpoint is set.
            if !self.agent_run || !ditto_web_fetch::search::validate_manifest(manifest) {
                return Err(replay_invalid("selected search is not an agent run's"));
            }
            let schema = ditto_web_fetch::search::schema();
            expected_revisions.push(
                CapabilityRevision::from_contract(
                    manifest,
                    &schema,
                    ditto_web_fetch::search::deriver_revision(),
                )
                .map_err(|error| replay_invalid(error.to_string()))?,
            );
            expected_schemas.push(schema);
            expected_cards.push(CapabilityCard::from(manifest));
            self.search_selected = true;
        }
        if let Some(manifest) = &selected.memory_manifest {
            // Offered to every agent run.
            if !self.agent_run || !super::recall::validate_manifest(manifest) {
                return Err(replay_invalid(
                    "selected memory search is not an agent run's",
                ));
            }
            let schema = super::recall::schema();
            expected_revisions.push(
                CapabilityRevision::from_contract(
                    manifest,
                    &schema,
                    super::recall::RecallDeriver::default().revision().clone(),
                )
                .map_err(|error| replay_invalid(error.to_string()))?,
            );
            expected_schemas.push(schema);
            expected_cards.push(CapabilityCard::from(manifest));
            self.memory_selected = true;
        }
        for manifest in [&selected.remember_manifest, &selected.forget_manifest]
            .into_iter()
            .flatten()
        {
            // Offered to every agent run.
            if !self.agent_run || !super::memory_write::validate_manifest(manifest) {
                return Err(replay_invalid(
                    "selected memory write is not an agent run's",
                ));
            }
            let schema = super::memory_write::schema(&manifest.id);
            expected_revisions.push(
                CapabilityRevision::from_contract(
                    manifest,
                    &schema,
                    super::memory_write::MemoryWriteDeriver::for_capability(&manifest.id)
                        .revision()
                        .clone(),
                )
                .map_err(|error| replay_invalid(error.to_string()))?,
            );
            expected_schemas.push(schema);
            expected_cards.push(CapabilityCard::from(manifest));
        }
        self.remember_selected = selected.remember_manifest.is_some();
        self.forget_selected = selected.forget_manifest.is_some();
        if selected.epoch.invocation_revisions() != expected_revisions {
            return Err(replay_invalid("selected invocation revisions changed"));
        }
        let selected_epoch_id = ExecutionEpochId::new(selected.epoch.id().to_owned())
            .map_err(|error| replay_invalid(error.to_string()))?;
        if selected.epoch.max_working_set() != expected_cards.len()
            || serde_json::to_value(selected.epoch.capabilities()).ok()
                != serde_json::to_value(expected_cards).ok()
        {
            return Err(replay_invalid(
                "selected epoch cards contradict the installed contracts",
            ));
        }
        self.execution_epoch_id = Some(selected_epoch_id);
        self.schemas = Some(expected_schemas);
        self.capabilities_payload = Some(selected);

        let mut request_index = 0_usize;
        let mut request_ids = BTreeSet::new();
        loop {
            if let Some(failure) = self.take_pre_request_stage_failure(request_index)? {
                return Ok(ArtifactReadTurnReplay::Failed { failure });
            }
            if request_index >= MAX_MODEL_REQUESTS {
                return Err(replay_invalid("model request bound was exceeded"));
            }
            let request_event = self.take(event_kind::MODEL_REQUESTED, EventActor::System)?;
            let recorded: ModelRequestDigestPayload = self.decode_versioned(request_event)?;
            let persisted = self.rebuild_request(recorded, request_index)?;
            self.require_turn_id(&persisted.turn_id)?;
            if persisted.request_index as usize != request_index {
                return Err(replay_invalid("model request index is not contiguous"));
            }
            if request_event.span_id.as_deref() != Some(persisted.request.request_id.as_str()) {
                return Err(replay_invalid("model.requested span id is inconsistent"));
            }
            if !request_ids.insert(persisted.request.request_id.clone()) {
                return Err(replay_invalid("model request id is duplicated"));
            }
            self.request_count = (request_index + 1) as u8;
            let post_request_contract_failure =
                self.validate_request(&persisted.request, request_index, request_event)?;
            self.requests.push(persisted.clone());
            if post_request_contract_failure.is_some() {
                let failure_event_time = self
                    .events
                    .get(self.index)
                    .ok_or_else(|| replay_invalid("post-request contract failure is truncated"))?
                    .recorded_at;
                let failure = self
                    .take_failure()?
                    .ok_or_else(|| replay_invalid("post-request contract failure is missing"))?;
                let valid = match failure.code {
                    TurnFailureCode::DriverContract => self.valid_reasoned_failure(
                        &failure,
                        &[TurnFailureReason::RequestInvalidAtDispatch],
                    ),
                    _ => self.valid_checkpoint_failure(
                        &failure,
                        failure_event_time,
                        &[Checkpoint::AfterModelRequestPersisted],
                    ),
                };
                if !valid
                    || failure.request_index != Some(request_index as u8)
                    || failure.call_id.is_some()
                {
                    return Err(replay_invalid(
                        "turn.failed is not valid after the durable model request",
                    ));
                }
                return Ok(ArtifactReadTurnReplay::Failed { failure });
            }

            let mut expected_sequence = 0_u64;
            let mut event_count = 0_usize;
            let mut model_output_bytes = 0_usize;
            let mut previous_admitted_at: Option<DateTime<Utc>> = None;
            let mut tool_buffer = ToolCallBuffer::default();
            let mut ready_call: Option<ReadyCall> = None;
            let mut request_text = String::new();
            let terminal;
            loop {
                if event_count == MAX_MODEL_EVENTS_PER_REQUEST {
                    let failure = self.take_exact_failure(
                        TurnFailureCode::BoundExceeded,
                        format!(
                            "model request exceeded {MAX_MODEL_EVENTS_PER_REQUEST} events without a terminal"
                        ),
                        Some(request_index as u8),
                        None,
                    )?;
                    return Ok(ArtifactReadTurnReplay::Failed { failure });
                }
                if let Some(failure) =
                    self.take_awaiting_output_failure(request_index, event_count)?
                {
                    return Ok(ArtifactReadTurnReplay::Failed { failure });
                }
                let output_event = self.take(event_kind::MODEL_OUTPUT, EventActor::Model)?;
                let encoded_output_bytes = serde_json::to_vec(&output_event.payload)
                    .map_err(|error| replay_invalid(error.to_string()))?
                    .len();
                if encoded_output_bytes > MAX_MODEL_OUTPUT_EVENT_BYTES
                    || model_output_bytes.saturating_add(encoded_output_bytes)
                        > MAX_MODEL_OUTPUT_BYTES_PER_REQUEST
                {
                    return Err(replay_invalid(
                        "persisted model output exceeds the durable byte bounds",
                    ));
                }
                model_output_bytes = model_output_bytes.saturating_add(encoded_output_bytes);
                let output: ModelOutputPayload = self.decode_versioned(output_event)?;
                self.require_turn_id(&output.turn_id)?;
                let deadline = self.deadline.expect("first request set the deadline");
                if output.admitted_at < request_event.recorded_at
                    || previous_admitted_at.is_some_and(|previous| output.admitted_at < previous)
                    || output.admitted_at >= deadline
                    || output.admitted_at
                        > output_event.recorded_at + ChronoDuration::milliseconds(1)
                {
                    return Err(replay_invalid(
                        "model.output admission timestamp is inconsistent",
                    ));
                }
                previous_admitted_at = Some(output.admitted_at);
                if output_event.span_id.as_deref() != Some(output.request_id.as_str()) {
                    return Err(replay_invalid("model.output span id is inconsistent"));
                }
                if output.request_index as usize != request_index
                    || output.request_id != persisted.request.request_id
                {
                    return Err(replay_invalid(
                        "model output is correlated to the wrong request",
                    ));
                }
                if output.stream_event.sequence != expected_sequence {
                    return Err(replay_invalid("model stream sequence is not contiguous"));
                }
                // Consecutive text deltas are journaled as one chunk.
                let covered = match output.through_sequence {
                    None => 1,
                    Some(through)
                        if matches!(output.stream_event.event, ModelEvent::TextDelta { .. })
                            && through > output.stream_event.sequence =>
                    {
                        through - output.stream_event.sequence + 1
                    }
                    Some(_) => {
                        return Err(replay_invalid("model output chunk covers no text"));
                    }
                };
                let covered = usize::try_from(covered)
                    .ok()
                    .filter(|covered| event_count + covered <= MAX_MODEL_EVENTS_PER_REQUEST)
                    .ok_or_else(|| {
                        replay_invalid("model output chunk passes the request's event bound")
                    })?;
                output
                    .stream_event
                    .validate()
                    .map_err(|error| replay_invalid(error.to_string()))?;
                expected_sequence = expected_sequence.saturating_add(covered as u64);
                event_count += covered;
                self.outputs.push(output.clone());

                match &output.stream_event.event {
                    ModelEvent::TextDelta { text } => {
                        let prospective = self.total_text_bytes.saturating_add(text.len());
                        if prospective > MAX_ASSISTANT_TEXT_BYTES {
                            return Err(replay_invalid(
                                "overflowing assistant text was durably appended",
                            ));
                        }
                        self.total_text_bytes = prospective;
                        request_text.push_str(text);
                        append_assistant_text(&mut self.conversation, text);
                    }
                    ModelEvent::ToolCallStarted {
                        call_id,
                        capability_id,
                    } => {
                        if capability_id != ARTIFACT_READ_ID
                            && !(self.sort.is_some() && capability_id == ditto_artifact_sort::ID)
                            && !(self.fetch_selected && capability_id == ditto_web_fetch::ID)
                            && !(self.search_selected
                                && capability_id == ditto_web_fetch::search::ID)
                            && !(self.memory_selected && capability_id == super::recall::ID)
                            && !(self.remember_selected && capability_id == REMEMBER_ID)
                            && !(self.forget_selected && capability_id == FORGET_ID)
                        {
                            let failure = self.take_exact_failure(
                                TurnFailureCode::Protocol,
                                format!("unknown capability {capability_id}"),
                                Some(request_index as u8),
                                Some(call_id.clone()),
                            )?;
                            return Ok(ArtifactReadTurnReplay::Failed { failure });
                        }
                        if !self.all_call_ids.insert(call_id.clone()) {
                            let failure = self.take_exact_failure(
                                TurnFailureCode::Protocol,
                                format!("duplicate epoch-wide tool call id {call_id}"),
                                Some(request_index as u8),
                                Some(call_id.clone()),
                            )?;
                            return Ok(ArtifactReadTurnReplay::Failed { failure });
                        }
                        let rebuilt = tool_buffer.start(call_id.clone(), capability_id.clone());
                        let rebuilt = match rebuilt {
                            Ok(rebuilt) => rebuilt,
                            Err(error) => {
                                let failure = self.take_tool_call_failure(&error, request_index)?;
                                return Ok(ArtifactReadTurnReplay::Failed { failure });
                            }
                        };
                        if rebuilt != output.stream_event.event {
                            return Err(replay_invalid("tool-call start does not reconstruct"));
                        }
                    }
                    ModelEvent::ToolCallArgumentDelta { call_id, delta } => {
                        let rebuilt = match tool_buffer.push_arguments(call_id, delta) {
                            Ok(rebuilt) => rebuilt,
                            Err(error) => {
                                let failure = self.take_tool_call_failure(&error, request_index)?;
                                return Ok(ArtifactReadTurnReplay::Failed { failure });
                            }
                        };
                        if rebuilt != output.stream_event.event {
                            return Err(replay_invalid("tool-call arguments do not reconstruct"));
                        }
                    }
                    ModelEvent::ToolCallReady {
                        call_id,
                        capability_id,
                        arguments,
                    } => {
                        if capability_id != ARTIFACT_READ_ID
                            && !(self.sort.is_some() && capability_id == ditto_artifact_sort::ID)
                            && !(self.fetch_selected && capability_id == ditto_web_fetch::ID)
                            && !(self.search_selected
                                && capability_id == ditto_web_fetch::search::ID)
                            && !(self.memory_selected && capability_id == super::recall::ID)
                            && !(self.remember_selected && capability_id == REMEMBER_ID)
                            && !(self.forget_selected && capability_id == FORGET_ID)
                        {
                            let failure = self.take_exact_failure(
                                TurnFailureCode::Protocol,
                                format!("unknown capability {capability_id}"),
                                Some(request_index as u8),
                                Some(call_id.clone()),
                            )?;
                            return Ok(ArtifactReadTurnReplay::Failed { failure });
                        }
                        if ready_call.is_some() {
                            let failure = self.take_exact_failure(
                                TurnFailureCode::Protocol,
                                "a model request produced more than one ready tool call",
                                Some(request_index as u8),
                                Some(call_id.clone()),
                            )?;
                            return Ok(ArtifactReadTurnReplay::Failed { failure });
                        }
                        let rebuilt = match tool_buffer.finish(call_id) {
                            Ok(rebuilt) => rebuilt,
                            Err(error) => {
                                let failure = self.take_tool_call_failure(&error, request_index)?;
                                return Ok(ArtifactReadTurnReplay::Failed { failure });
                            }
                        };
                        if rebuilt != output.stream_event.event {
                            let failure = self.take_exact_failure(
                                TurnFailureCode::Protocol,
                                "ready tool call does not match accumulated arguments",
                                Some(request_index as u8),
                                Some(call_id.clone()),
                            )?;
                            return Ok(ArtifactReadTurnReplay::Failed { failure });
                        }
                        ready_call = Some(ReadyCall {
                            call_id: call_id.clone(),
                            capability_id: capability_id.clone(),
                            arguments: arguments.clone(),
                        });
                        self.conversation.push(ConversationItem::ToolCall {
                            call_id: call_id.clone(),
                            capability_id: capability_id.clone(),
                            arguments: arguments.clone(),
                        });
                    }
                    ModelEvent::ReasoningItemStarted { .. }
                    | ModelEvent::ReasoningDelta { .. }
                    | ModelEvent::ReasoningItemReady { .. } => {
                        let failure = self.take_exact_failure(
                            TurnFailureCode::Protocol,
                            "reasoning events are not permitted in this turn loop",
                            Some(request_index as u8),
                            None,
                        )?;
                        return Ok(ArtifactReadTurnReplay::Failed { failure });
                    }
                    ModelEvent::StructuredOutput { .. } => {
                        let failure = self.take_exact_failure(
                            TurnFailureCode::Protocol,
                            "text-constrained turn received structured output",
                            Some(request_index as u8),
                            None,
                        )?;
                        return Ok(ArtifactReadTurnReplay::Failed { failure });
                    }
                    ModelEvent::Completed { .. } | ModelEvent::Failed { .. } => {
                        terminal = output.stream_event.event;
                        break;
                    }
                    ModelEvent::UsageUpdate { .. } | ModelEvent::ProviderWarning { .. } => {}
                }
            }

            if let ModelEvent::Failed {
                failure: model_failure,
            } = &terminal
            {
                let failure_event_time = self
                    .events
                    .get(self.index)
                    .ok_or_else(|| replay_invalid("model failure lacks turn.failed"))?
                    .recorded_at;
                let failure = self
                    .take_failure()?
                    .ok_or_else(|| replay_invalid("model failure lacks turn.failed"))?;
                if failure.code != turn_failure_code_for_model(model_failure.kind)
                    || failure.message != bounded_turn_failure_message(&model_failure.message)
                    || failure.request_index != Some(request_index as u8)
                    || failure.call_id != model_failure.call_id
                    || failure.reason.is_some()
                    || (failure.code == TurnFailureCode::DeadlineExceeded
                        && !self.valid_deadline_failure(&failure, failure_event_time))
                    || (failure.code != TurnFailureCode::DeadlineExceeded
                        && failure.evidence.is_some())
                {
                    return Err(replay_invalid(
                        "turn.failed contradicts the persisted model failure",
                    ));
                }
                return Ok(ArtifactReadTurnReplay::Failed { failure });
            }

            if tool_buffer.has_active_calls() {
                let failure = self.take_exact_failure(
                    TurnFailureCode::Protocol,
                    "model terminal has an unfinished tool call",
                    Some(request_index as u8),
                    None,
                )?;
                return Ok(ArtifactReadTurnReplay::Failed { failure });
            }

            match terminal {
                ModelEvent::Failed { .. } => {
                    unreachable!("model failures are handled before completed semantics")
                }
                ModelEvent::Completed {
                    finish_reason: FinishReason::ToolCalls,
                    continuation,
                } => {
                    if continuation.is_some() {
                        let failure = self.take_exact_failure(
                            TurnFailureCode::Protocol,
                            "provider-managed continuation is not permitted in this turn loop",
                            Some(request_index as u8),
                            None,
                        )?;
                        return Ok(ArtifactReadTurnReplay::Failed { failure });
                    }
                    let Some(call) = ready_call else {
                        let failure = self.take_exact_failure(
                            TurnFailureCode::Protocol,
                            "tool-call terminal contained no ready tool call",
                            Some(request_index as u8),
                            None,
                        )?;
                        return Ok(ArtifactReadTurnReplay::Failed { failure });
                    };
                    if request_index + 1 >= MAX_MODEL_REQUESTS {
                        let failure = self.take_exact_failure(
                            TurnFailureCode::BoundExceeded,
                            format!("turn exhausted the {MAX_MODEL_REQUESTS}-request limit"),
                            Some(request_index as u8),
                            Some(call.call_id.clone()),
                        )?;
                        return Ok(ArtifactReadTurnReplay::Failed { failure });
                    }
                    if self.next_is_failure() {
                        let failure_event_time = self
                            .events
                            .get(self.index)
                            .expect("next failure event is present")
                            .recorded_at;
                        let failure = self
                            .take_failure()?
                            .expect("next event was checked as turn.failed");
                        let valid = self.valid_checkpoint_failure(
                            &failure,
                            failure_event_time,
                            &[Checkpoint::BeforeCapabilityRequest],
                        );
                        if !valid
                            || failure.request_index != Some(request_index as u8)
                            || failure.call_id.as_ref() != Some(&call.call_id)
                        {
                            return Err(replay_invalid(
                                "turn.failed is not valid before capability request",
                            ));
                        }
                        return Ok(ArtifactReadTurnReplay::Failed { failure });
                    }

                    if call.capability_id == ditto_web_fetch::ID
                        || call.capability_id == ditto_web_fetch::search::ID
                        || call.capability_id == ditto_artifact_sort::ID
                        || call.capability_id == ARTIFACT_READ_ID
                    {
                        self.read_external_content = true;
                    }
                    if call.capability_id == ditto_web_fetch::search::ID {
                        if let Some(failure) =
                            self.replay_search_call(request_index as u8, &call)?
                        {
                            return Ok(ArtifactReadTurnReplay::Failed { failure });
                        }
                        request_index += 1;
                        continue;
                    }
                    if call.capability_id == ditto_web_fetch::ID {
                        if let Some(failure) = self.replay_fetch_call(request_index as u8, &call)? {
                            return Ok(ArtifactReadTurnReplay::Failed { failure });
                        }
                        request_index += 1;
                        continue;
                    }
                    if call.capability_id == ditto_artifact_sort::ID {
                        if let Some(failure) = self.replay_sort_call(request_index as u8, &call)? {
                            return Ok(ArtifactReadTurnReplay::Failed { failure });
                        }
                        request_index += 1;
                        continue;
                    }
                    if call.capability_id == super::recall::ID {
                        self.replay_recall_call(request_index as u8, &call)?;
                        request_index += 1;
                        continue;
                    }
                    if call.capability_id == REMEMBER_ID || call.capability_id == FORGET_ID {
                        self.replay_memory_write(request_index as u8, &call)?;
                        request_index += 1;
                        continue;
                    }

                    let capability_event =
                        self.take(event_kind::CAPABILITY_REQUESTED, EventActor::Model)?;
                    let capability: CapabilityRequestedPayload =
                        self.decode_versioned(capability_event)?;
                    self.require_turn_id(&capability.turn_id)?;
                    if capability_event.span_id.as_deref() != Some(capability.call_id.as_str()) {
                        return Err(replay_invalid(
                            "capability.requested span id is inconsistent",
                        ));
                    }
                    if capability.request_index as usize != request_index
                        || capability.execution_epoch_id
                            != self
                                .execution_epoch_id
                                .as_ref()
                                .expect("selected epoch is set")
                                .clone()
                        || capability.call_id != call.call_id
                        || capability.capability_id != ARTIFACT_READ_ID
                        || capability.capability_version != ARTIFACT_READ_VERSION
                        || capability.arguments != call.arguments
                    {
                        return Err(replay_invalid("capability request is not correlated"));
                    }
                    let normalized =
                        ditto_artifact_read::ArtifactReadNormalizer.normalize(&call.arguments);
                    match (&normalized, &capability.normalized) {
                        (Ok(expected), Some(actual)) if expected == actual => {}
                        (Err(_), None) => {}
                        _ => {
                            return Err(replay_invalid(
                                "capability request normalization is inconsistent",
                            ));
                        }
                    }

                    let call_projection_index = self.calls.len();
                    self.calls.push(ReplayedArtifactReadCall {
                        requested: capability.clone(),
                        started: None,
                        output: None,
                    });

                    if self.next_is_failure() {
                        let failure_event_time = self
                            .events
                            .get(self.index)
                            .expect("next failure event is present")
                            .recorded_at;
                        let failure = self
                            .take_failure()?
                            .expect("next event was checked as turn.failed");
                        let valid = self.valid_checkpoint_failure(
                            &failure,
                            failure_event_time,
                            &[Checkpoint::AfterCapabilityRequest],
                        );
                        if !valid
                            || failure.request_index != Some(request_index as u8)
                            || failure.call_id.as_ref() != Some(&call.call_id)
                        {
                            return Err(replay_invalid(
                                "turn.failed is not valid after capability request",
                            ));
                        }
                        return Ok(ArtifactReadTurnReplay::Failed { failure });
                    }

                    let started_event =
                        self.take(event_kind::EXECUTION_STARTED, EventActor::Capability)?;
                    let started: ExecutionStartedPayload = self.decode_versioned(started_event)?;
                    self.require_turn_id(&started.turn_id)?;
                    if started_event.span_id.as_deref() != Some(started.call_id.as_str()) {
                        return Err(replay_invalid("execution.started span id is inconsistent"));
                    }
                    if started.request_index as usize != request_index
                        || started.call_id != call.call_id
                        || started.capability_id != ARTIFACT_READ_ID
                        || started.capability_version != ARTIFACT_READ_VERSION
                        || started.authorization_through_seq < capability_event.seq
                        || started.authorization_through_seq >= started_event.seq
                    {
                        return Err(replay_invalid("execution start is not correlated"));
                    }
                    match (&normalized, &started.resource) {
                        (Ok(expected), Some(actual)) if expected == actual => {}
                        (Err(_), None) => {}
                        _ => {
                            return Err(replay_invalid(
                                "execution resource does not match strict normalization",
                            ));
                        }
                    }
                    self.calls[call_projection_index].started = Some(started.clone());

                    if self.next_is_failure() {
                        let failure_event_time = self
                            .events
                            .get(self.index)
                            .expect("next failure event is present")
                            .recorded_at;
                        let failure = self
                            .take_failure()?
                            .expect("next event was checked as turn.failed");
                        let valid_checkpoint_failure = self.valid_checkpoint_failure(
                            &failure,
                            failure_event_time,
                            &[
                                Checkpoint::AfterExecutionStarted,
                                Checkpoint::AfterArtifactRead,
                            ],
                        );
                        if !valid_checkpoint_failure
                            || failure.request_index != Some(request_index as u8)
                            || failure.call_id.as_ref() != Some(&call.call_id)
                        {
                            return Err(replay_invalid(
                                "turn.failed is not valid at the execution checkpoint",
                            ));
                        }
                        return Ok(ArtifactReadTurnReplay::Failed { failure });
                    }
                    let output_event =
                        self.take(event_kind::EXECUTION_OUTPUT, EventActor::Capability)?;
                    let output: ExecutionOutputPayload = self.decode_versioned(output_event)?;
                    self.require_turn_id(&output.turn_id)?;
                    if output_event.span_id.as_deref() != Some(output.call_id.as_str()) {
                        return Err(replay_invalid("execution.output span id is inconsistent"));
                    }
                    if output.request_index as usize != request_index
                        || output.call_id != call.call_id
                        || output.capability_id != ARTIFACT_READ_ID
                        || output.capability_version != ARTIFACT_READ_VERSION
                    {
                        return Err(replay_invalid("execution output is not correlated"));
                    }
                    let authorized = normalized.as_ref().is_ok_and(|resource| {
                        self.artifact_is_authorized_in_snapshot(
                            resource,
                            started.authorization_through_seq,
                        )
                    });
                    validate_execution_result(&normalized, &output.result, authorized)?;
                    self.calls[call_projection_index].output = Some(output.clone());
                    let result_value = serde_json::to_value(&output.result)
                        .map_err(|error| replay_invalid(error.to_string()))?;
                    self.conversation.push(ConversationItem::ToolResult {
                        call_id: call.call_id,
                        content: vec![ContentPart::Structured {
                            value: result_value,
                        }],
                        is_error: output.result.is_error(),
                    });
                    self.tool_call_count = self.tool_call_count.saturating_add(1);
                    request_index += 1;
                }
                ModelEvent::Completed {
                    finish_reason: _,
                    continuation,
                } => {
                    if continuation.is_some() {
                        let failure = self.take_exact_failure(
                            TurnFailureCode::Protocol,
                            "provider-managed continuation is not permitted in this turn loop",
                            Some(request_index as u8),
                            None,
                        )?;
                        return Ok(ArtifactReadTurnReplay::Failed { failure });
                    }
                    if let Some(call) = ready_call {
                        let failure = self.take_exact_failure(
                            TurnFailureCode::Protocol,
                            "non-tool terminal followed a ready tool call",
                            Some(request_index as u8),
                            Some(call.call_id),
                        )?;
                        return Ok(ArtifactReadTurnReplay::Failed { failure });
                    }
                    if self.tool_call_count == 0 && !self.agent_run {
                        let failure = self.take_exact_failure(
                            TurnFailureCode::Protocol,
                            "turn ended before executing artifact.read",
                            Some(request_index as u8),
                            None,
                        )?;
                        return Ok(ArtifactReadTurnReplay::Failed { failure });
                    }
                    if request_text.is_empty() {
                        let failure = self.take_exact_failure(
                            TurnFailureCode::Protocol,
                            "final model request produced no assistant text",
                            Some(request_index as u8),
                            None,
                        )?;
                        return Ok(ArtifactReadTurnReplay::Failed { failure });
                    }
                    if self.next_is_failure() {
                        let failure_event_time = self
                            .events
                            .get(self.index)
                            .expect("next failure event is present")
                            .recorded_at;
                        let failure = self
                            .take_failure()?
                            .expect("next event was checked as turn.failed");
                        let valid = self.valid_checkpoint_failure(
                            &failure,
                            failure_event_time,
                            &[Checkpoint::AfterFinalOutput],
                        );
                        if !valid
                            || failure.request_index != Some(request_index as u8)
                            || failure.call_id.is_some()
                        {
                            return Err(replay_invalid(
                                "turn.failed is not valid before turn completion",
                            ));
                        }
                        return Ok(ArtifactReadTurnReplay::Failed { failure });
                    }
                    let finished_event =
                        self.take(event_kind::TURN_FINISHED, EventActor::System)?;
                    let finished: TurnFinishedPayload = self.decode_versioned(finished_event)?;
                    self.require_turn_id(&finished.turn_id)?;
                    if finished_event.span_id.is_some() {
                        return Err(replay_invalid("turn.finished must not carry a span id"));
                    }
                    let expected = ArtifactReadTurnOutcome {
                        turn_id: self.turn_id.clone(),
                        session_id: self.session_id.clone(),
                        task_id: self.task_id.clone(),
                        execution_epoch_id: self
                            .execution_epoch_id
                            .clone()
                            .expect("selected epoch is set"),
                        response: request_text,
                        status: ArtifactReadTurnStatus::Unverified,
                        request_count: (request_index + 1) as u8,
                        tool_call_count: self.tool_call_count,
                    };
                    if finished.outcome != expected {
                        return Err(replay_invalid("turn.finished outcome is inconsistent"));
                    }
                    if self.index != self.events.len() {
                        return Err(replay_invalid("events follow the turn terminal"));
                    }
                    return Ok(ArtifactReadTurnReplay::Finished {
                        outcome: finished.outcome,
                    });
                }
                ModelEvent::TextDelta { .. }
                | ModelEvent::ToolCallStarted { .. }
                | ModelEvent::ToolCallArgumentDelta { .. }
                | ModelEvent::ToolCallReady { .. }
                | ModelEvent::StructuredOutput { .. }
                | ModelEvent::UsageUpdate { .. }
                | ModelEvent::ProviderWarning { .. }
                | ModelEvent::ReasoningItemStarted { .. }
                | ModelEvent::ReasoningDelta { .. }
                | ModelEvent::ReasoningItemReady { .. } => {
                    unreachable!("only model terminal events leave the replay stream loop")
                }
            }
        }
    }

    fn validate_context_sources(
        &self,
        context: &ContextCompiledPayload,
        context_event: &EventRecord,
    ) -> Result<(), ReplayError> {
        if context.provenance_through_seq < self.events[0].seq
            || context.provenance_through_seq >= context_event.seq
        {
            return Err(replay_invalid(
                "context provenance cutoff is outside the accepted input window",
            ));
        }
        for source_id in context
            .compiled
            .nodes
            .iter()
            .flat_map(|node| &node.source_event_ids)
        {
            let source = self
                .snapshot
                .iter()
                .find(|event| event.event_id == *source_id)
                .ok_or_else(|| replay_invalid(format!("context source {source_id} is missing")))?;
            if source.seq > context.provenance_through_seq
                || source.session_id.as_deref() != Some(self.session_id.as_str())
                || source
                    .task_id
                    .as_deref()
                    .is_some_and(|task_id| task_id != self.task_id)
            {
                return Err(replay_invalid(format!(
                    "context source {source_id} is later, cross-scope, or untrusted"
                )));
            }
        }
        Ok(())
    }

    /// Agent runs replay the thread's recent exchanges. Recompute them from
    /// the snapshot with the runtime's rule and require the recorded turn IDs
    /// to match exactly.
    fn conversation_history(
        &self,
        context: &ContextCompiledPayload,
    ) -> Result<Vec<HistoryExchange>, ReplayError> {
        let history = if self.agent_run {
            let (thread_len, newest_first) = self.recompute_thread()?;
            select_history_stepped(thread_len, newest_first)
        } else {
            Vec::new()
        };
        if context.history_turn_ids
            != history
                .iter()
                .map(|exchange| exchange.turn_id.clone())
                .collect::<Vec<_>>()
        {
            return Err(replay_invalid(
                "context history does not match the conversation thread",
            ));
        }
        Ok(history)
    }

    /// The thread before this turn, recomputed from the snapshot: its length
    /// in finished `run_*` turns (the stepped window's anchor) and its newest
    /// exchanges, newest first.
    fn recompute_thread(&self) -> Result<(usize, Vec<HistoryExchange>), ReplayError> {
        let input_seq = self.events[0].seq;
        let boundary = self
            .snapshot
            .iter()
            .filter(|event| event.seq < input_seq && event.kind == event_kind::CONVERSATION_RESET)
            .map(|event| event.seq)
            .max()
            .unwrap_or(0);
        let in_thread = |event: &&EventRecord| {
            event.seq < input_seq && event.seq > boundary && event.kind == event_kind::TURN_FINISHED
        };
        let thread_len = self
            .snapshot
            .iter()
            .filter(in_thread)
            .filter(|event| {
                event
                    .task_id
                    .as_deref()
                    .is_some_and(|task| task.starts_with("run_"))
            })
            .count();
        let mut exchanges = Vec::new();
        for finished in self
            .snapshot
            .iter()
            .rev()
            .filter(in_thread)
            .take(MAX_HISTORY_CANDIDATES)
        {
            let payload: TurnFinishedPayload = decode_payload(finished)?;
            let task = finished
                .task_id
                .as_deref()
                .ok_or_else(|| replay_invalid("history turn has no task"))?;
            let input = self
                .snapshot
                .iter()
                .find(|event| {
                    payload.turn_id.starts_with("turn_")
                        && event.task_id.as_deref() == Some(task)
                        && event.correlation_id.as_deref() == Some(payload.turn_id.as_str())
                })
                .ok_or_else(|| replay_invalid("history turn has no input"))?;
            if let Some(user) = agent_run_text(input) {
                exchanges.push(HistoryExchange {
                    turn_id: payload.turn_id,
                    user: user.to_owned(),
                    assistant: payload.outcome.response,
                });
            }
        }
        Ok((thread_len, exchanges))
    }

    fn artifact_is_authorized_in_snapshot(
        &self,
        resource: &ArtifactReadResource,
        authorization_through_seq: i64,
    ) -> bool {
        let reference = resource.reference().to_string();
        self.snapshot.iter().any(|event| {
            event.seq <= authorization_through_seq
                && event.kind == event_kind::ARTIFACT_CREATED
                && event.actor == EventActor::System
                && event.session_id.as_deref() == Some(self.session_id.as_str())
                && event
                    .task_id
                    .as_deref()
                    .is_none_or(|task_id| task_id == self.task_id)
                && event.payload.get("reference").and_then(Value::as_str)
                    == Some(reference.as_str())
        })
    }

    fn finish_projection(self, terminal: ArtifactReadTurnReplay) -> ReplayedReadOnlyTurn {
        ReplayedReadOnlyTurn {
            turn_id: self.turn_id,
            session_id: self.session_id,
            task_id: self.task_id,
            context: self.context_payload,
            capabilities: self.capabilities_payload,
            requests: self.requests,
            outputs: self.outputs,
            calls: self.calls,
            sort_calls: self.sort_calls,
            fetch_calls: self.fetch_calls,
            recall_calls: self.recall_calls,
            memory_writes: self.memory_writes,
            search_calls: self.search_calls,
            terminal,

            sequence_span: TurnSequenceSpan {
                first_seq: self.events[0].seq,
                last_seq: self
                    .events
                    .last()
                    .expect("a replayed turn always has an input")
                    .seq,
            },
        }
    }

    /// Rebuild a version-7 request from the turn's durable state and require
    /// the recorded digest: the request itself was never journaled.
    fn rebuild_request(
        &self,
        recorded: ModelRequestDigestPayload,
        request_index: usize,
    ) -> Result<ModelRequestedPayload, ReplayError> {
        let request = model_request(RequestInputs {
            request_id: recorded.request_id,
            execution_epoch_id: self
                .execution_epoch_id
                .clone()
                .expect("selected epoch is set"),
            system_prefix: self.system_prefix.clone().expect("system prefix is set"),
            context: self.context.clone().expect("compiled context is set"),
            tools: self.schemas.clone().expect("selected schemas are set"),
            conversation: self.conversation.clone(),
            first_tool_required: request_index == 0 && !self.agent_run,
            turn_id: self.turn_id.clone(),
            deadline: recorded.deadline,
        })
        .map_err(replay_invalid)?;
        if request_sha256(&request) != recorded.request_sha256 {
            return Err(replay_invalid(
                "model request digest does not match the durable turn state",
            ));
        }
        Ok(ModelRequestedPayload {
            event_version: recorded.event_version,
            turn_id: recorded.turn_id,
            request_index: recorded.request_index,
            request,
        })
    }

    fn validate_request(
        &mut self,
        request: &ModelRequest,
        request_index: usize,
        event: &EventRecord,
    ) -> Result<Option<String>, ReplayError> {
        if request.execution_epoch_id
            != self
                .execution_epoch_id
                .as_ref()
                .expect("selected epoch is set")
                .clone()
            || Some(&request.stable_system_prefix) != self.system_prefix.as_ref()
            || request.tools
                != self
                    .schemas
                    .as_ref()
                    .expect("selected schemas are set")
                    .clone()
            || request.turn.context
                != self
                    .context
                    .as_ref()
                    .expect("compiled context is set")
                    .clone()
            || request.turn.conversation != self.conversation
            || request.turn.output != OutputConstraint::Text
            || request.continuation.is_some()
        {
            return Err(replay_invalid("model request changed stable turn state"));
        }
        let expected_features = [ModelFeature::Text, ModelFeature::ToolCalls]
            .into_iter()
            .collect::<BTreeSet<_>>();
        let expected_generation = GenerationControls {
            reasoning: None,
            prompt_cache: Default::default(),
            tool_use: ToolUsePolicy {
                choice: if request_index == 0 && !self.agent_run {
                    ToolChoice::Required
                } else {
                    ToolChoice::Auto
                },
                parallel_calls: ParallelToolCalls::Forbid,
            },
        };
        if request.features.required != expected_features
            || !request.features.preferred.is_empty()
            || request.generation != expected_generation
            || request
                .control
                .cancellation_id
                .as_ref()
                .map(|id| id.as_str())
                != Some(self.turn_id.as_str())
        {
            return Err(replay_invalid("model request controls are inconsistent"));
        }
        let request_deadline = request
            .control
            .deadline
            .ok_or_else(|| replay_invalid("model request deadline is missing"))?;
        if request_deadline.timestamp_subsec_nanos() % 1_000_000 != 0 {
            return Err(replay_invalid(
                "model request deadline is not millisecond-canonical",
            ));
        }
        if request_deadline
            > self.input_recorded_at
                + ChronoDuration::from_std(MAX_TURN_DURATION)
                    .expect("five-minute turn ceiling fits chrono duration")
        {
            return Err(replay_invalid(
                "model request deadline exceeds the hard ceiling",
            ));
        }
        match self.deadline {
            Some(deadline) if deadline != request_deadline => {
                return Err(replay_invalid(
                    "model request deadline changed within the turn",
                ));
            }
            None => self.deadline = Some(request_deadline),
            Some(_) => {}
        }
        if event.recorded_at >= request_deadline {
            return if self.next_is_failure() {
                Ok(None)
            } else {
                Err(replay_invalid(
                    "model output followed a request durably recorded after its deadline",
                ))
            };
        }
        match request.validate_at(event.recorded_at) {
            Ok(()) => Ok(None),
            Err(error) if self.next_is_failure() => Ok(Some(error.to_string())),
            Err(error) => Err(replay_invalid(error.to_string())),
        }
    }

    fn take(&mut self, kind: &str, actor: EventActor) -> Result<&'turn EventRecord, ReplayError> {
        let event = self
            .events
            .get(self.index)
            .ok_or_else(|| replay_invalid(format!("turn is truncated before {kind}")))?;
        if event.kind != kind || event.actor != actor {
            return Err(replay_invalid(format!(
                "expected {actor}/{kind}, found {}/{}",
                event.actor, event.kind
            )));
        }
        self.index += 1;
        Ok(event)
    }

    fn take_failure(&mut self) -> Result<Option<TurnFailure>, ReplayError> {
        let Some(event) = self.events.get(self.index) else {
            return Ok(None);
        };
        if event.kind != event_kind::TURN_FAILED {
            return Ok(None);
        }
        if event.actor != EventActor::System {
            return Err(replay_invalid("turn.failed has an untrusted actor"));
        }
        if event.span_id.is_some() {
            return Err(replay_invalid("turn.failed must not carry a span id"));
        }
        self.index += 1;
        let payload: TurnFailedPayload = self.decode_versioned(event)?;
        if payload.turn_id != self.turn_id
            || payload.failure.turn_id != self.turn_id
            || payload.failure.session_id != self.session_id
            || payload.failure.task_id != self.task_id
            || payload.status != ArtifactReadTurnStatus::Unverified
            || payload.request_count != self.request_count
            || payload.tool_call_count != self.tool_call_count
            || payload.failure.message.len() > MAX_TURN_FAILURE_MESSAGE_BYTES
        {
            return Err(replay_invalid("turn.failed scope is inconsistent"));
        }
        if payload
            .failure
            .reason
            .is_some_and(|reason| reason.code() != payload.failure.code)
        {
            return Err(replay_invalid("turn.failed reason contradicts its code"));
        }
        if self.index != self.events.len() {
            return Err(replay_invalid("events follow turn.failed"));
        }
        Ok(Some(payload.failure))
    }

    fn take_initial_stage_failure(&mut self) -> Result<Option<TurnFailure>, ReplayError> {
        if !self.next_is_failure() {
            return Ok(None);
        }
        let failure_event_time = self
            .events
            .get(self.index)
            .expect("next failure event is present")
            .recorded_at;
        let failure = self
            .take_failure()?
            .expect("next event was checked as turn.failed");
        let valid = match failure.code {
            TurnFailureCode::ContextCompilation => {
                let mut allowed = vec![
                    TurnFailureReason::DuplicateContextCandidate,
                    TurnFailureReason::EmptyPolicyReason,
                    TurnFailureReason::InvalidRequiredContext,
                    TurnFailureReason::RequiredContextOverBudget,
                    TurnFailureReason::MissingContextProvenance,
                    TurnFailureReason::UnresolvedContextProvenance,
                ];
                if self.agent_run {
                    allowed.push(TurnFailureReason::SessionContextUnavailable);
                    allowed.push(TurnFailureReason::ConversationHistoryUnavailable);
                }
                self.valid_reasoned_failure(&failure, &allowed)
            }
            _ => self.valid_checkpoint_failure(
                &failure,
                failure_event_time,
                &[Checkpoint::BeforeContextCompilation],
            ),
        };
        if !valid || failure.request_index.is_some() || failure.call_id.is_some() {
            return Err(replay_invalid(
                "turn.failed is not valid before context compilation",
            ));
        }
        Ok(Some(failure))
    }

    fn take_capability_stage_failure(&mut self) -> Result<Option<TurnFailure>, ReplayError> {
        if !self.next_is_failure() {
            return Ok(None);
        }
        let failure = self
            .take_failure()?
            .expect("next event was checked as turn.failed");
        let mut allowed = vec![
            TurnFailureReason::ArtifactReadUnavailable,
            TurnFailureReason::ArtifactReadPackageUnverified,
            TurnFailureReason::ArtifactReadManifestMismatch,
            TurnFailureReason::ArtifactReadSchemaMismatch,
            TurnFailureReason::ArtifactReadSelectionFailed,
        ];
        if self.sort.is_some() {
            allowed.extend([
                TurnFailureReason::SortPermissionSourceUnavailable,
                TurnFailureReason::SortContractUnavailable,
            ]);
        }
        let valid = self.valid_reasoned_failure(&failure, &allowed);
        if !valid || failure.request_index.is_some() || failure.call_id.is_some() {
            return Err(replay_invalid(
                "turn.failed is not valid during capability selection",
            ));
        }
        Ok(Some(failure))
    }

    fn take_pre_request_stage_failure(
        &mut self,
        request_index: usize,
    ) -> Result<Option<TurnFailure>, ReplayError> {
        if !self.next_is_failure() {
            return Ok(None);
        }
        let failure_event_time = self
            .events
            .get(self.index)
            .expect("next failure event is present")
            .recorded_at;
        let failure = self
            .take_failure()?
            .expect("next event was checked as turn.failed");
        let valid = match failure.code {
            TurnFailureCode::DriverContract => self.valid_reasoned_failure(
                &failure,
                &[
                    TurnFailureReason::DriverFeaturesUnsupported,
                    TurnFailureReason::DriverToolChoiceUnsupported,
                    TurnFailureReason::DriverParallelCallsUnsupported,
                ],
            ),
            _ => self.valid_checkpoint_failure(
                &failure,
                failure_event_time,
                &[Checkpoint::BeforeModelRequest],
            ),
        };
        if !valid || failure.request_index != Some(request_index as u8) || failure.call_id.is_some()
        {
            return Err(replay_invalid(
                "turn.failed is not valid before the next model request",
            ));
        }
        Ok(Some(failure))
    }

    fn take_awaiting_output_failure(
        &mut self,
        request_index: usize,
        event_count: usize,
    ) -> Result<Option<TurnFailure>, ReplayError> {
        if !self.next_is_failure() {
            return Ok(None);
        }
        let failure_event_time = self
            .events
            .get(self.index)
            .expect("next failure event is present")
            .recorded_at;
        let failure = self
            .take_failure()?
            .expect("next event was checked as turn.failed");
        // Before the first output, cancellation can also win the checkpoint
        // between the durable request and driver invocation.
        let checkpoints: &[Checkpoint] = if event_count == 0 {
            &[
                Checkpoint::AwaitingModelOutput,
                Checkpoint::AfterModelRequestPersisted,
            ]
        } else {
            &[Checkpoint::AwaitingModelOutput]
        };
        let valid = match failure.code {
            TurnFailureCode::BoundExceeded => {
                matches!(
                    failure.message.as_str(),
                    message if message == format!(
                        "assistant text exceeded {MAX_ASSISTANT_TEXT_BYTES} bytes"
                    ) || message == format!(
                        "model output exceeded {MAX_MODEL_OUTPUT_EVENT_BYTES} encoded bytes"
                    ) || message == format!(
                        "model request output exceeded {MAX_MODEL_OUTPUT_BYTES_PER_REQUEST} encoded bytes"
                    )
                ) && failure.evidence.is_none()
                    && failure.reason.is_none()
            }
            _ => self.valid_checkpoint_failure(&failure, failure_event_time, checkpoints),
        };
        if !valid || failure.request_index != Some(request_index as u8) || failure.call_id.is_some()
        {
            return Err(replay_invalid(
                "turn.failed is not valid while awaiting model output",
            ));
        }
        Ok(Some(failure))
    }

    /// Whether `failure` is the exact cancellation or deadline terminal of one
    /// of `checkpoints`.
    fn valid_checkpoint_failure(
        &self,
        failure: &TurnFailure,
        failure_event_time: DateTime<Utc>,
        checkpoints: &[Checkpoint],
    ) -> bool {
        if failure.reason.is_some() {
            return false;
        }
        match failure.code {
            TurnFailureCode::Cancelled => {
                failure.evidence.is_none()
                    && checkpoints
                        .iter()
                        .any(|checkpoint| failure.message == checkpoint.cancelled_message())
            }
            TurnFailureCode::DeadlineExceeded => {
                checkpoints
                    .iter()
                    .any(|checkpoint| failure.message == checkpoint.deadline_message())
                    && self.valid_deadline_failure(failure, failure_event_time)
            }
            _ => false,
        }
    }

    fn valid_deadline_failure(
        &self,
        failure: &TurnFailure,
        failure_event_time: DateTime<Utc>,
    ) -> bool {
        let Some(TurnFailureEvidence::Deadline { deadline }) = &failure.evidence else {
            return false;
        };
        let hard_deadline = self.input_recorded_at
            + ChronoDuration::from_std(MAX_TURN_DURATION)
                .expect("five-minute turn ceiling fits chrono duration");
        deadline.timestamp_subsec_nanos() % 1_000_000 == 0
            && *deadline <= hard_deadline
            && self.deadline.is_none_or(|expected| expected == *deadline)
            && failure_event_time >= *deadline
    }

    fn next_is_failure(&self) -> bool {
        self.events
            .get(self.index)
            .is_some_and(|event| event.kind == event_kind::TURN_FAILED)
    }

    fn take_exact_failure(
        &mut self,
        code: TurnFailureCode,
        message: impl AsRef<str>,
        request_index: Option<u8>,
        call_id: Option<ProviderCallId>,
    ) -> Result<TurnFailure, ReplayError> {
        let failure = self
            .take_failure()?
            .ok_or_else(|| replay_invalid("expected an adjacent turn.failed terminal"))?;
        if failure.code != code
            || failure.message != message.as_ref()
            || failure.request_index != request_index
            || failure.call_id != call_id
            || failure.evidence.is_some()
            || failure.reason.is_some()
        {
            return Err(replay_invalid(
                "turn.failed contradicts the deterministic runtime failure",
            ));
        }
        Ok(failure)
    }

    /// Decode a turn payload of the one contract this build replays
    /// (ADR 0034).
    fn decode_versioned<T: DeserializeOwned + PayloadVersion>(
        &mut self,
        event: &EventRecord,
    ) -> Result<T, ReplayError> {
        let payload: T = decode_payload(event)?;
        if payload.version() != TURN_PAYLOAD_VERSION {
            return Err(replay_invalid(format!(
                "{} payload was recorded under another turn contract",
                event.kind
            )));
        }
        self.version = Some(TURN_PAYLOAD_VERSION);
        Ok(payload)
    }

    /// Validate a validator-derived failure: its typed reason must be in the
    /// stage's closed set and imply its code.
    fn valid_reasoned_failure(&self, failure: &TurnFailure, allowed: &[TurnFailureReason]) -> bool {
        failure.evidence.is_none()
            && failure
                .reason
                .is_some_and(|reason| allowed.contains(&reason) && reason.code() == failure.code)
    }

    /// A deterministic tool-call lifecycle error must be the adjacent terminal.
    fn take_tool_call_failure(
        &mut self,
        error: &ToolCallError,
        request_index: usize,
    ) -> Result<TurnFailure, ReplayError> {
        let failure = self
            .take_failure()?
            .ok_or_else(|| replay_invalid("expected an adjacent turn.failed terminal"))?;
        if failure.code != TurnFailureCode::Protocol
            || failure.request_index != Some(request_index as u8)
            || failure.call_id.as_ref() != Some(error.call_id())
            || !self.valid_reasoned_failure(&failure, &[TurnFailureReason::ToolCallLifecycle])
        {
            return Err(replay_invalid(
                "turn.failed contradicts the deterministic runtime failure",
            ));
        }
        Ok(failure)
    }

    fn require_turn_id(&self, turn_id: &str) -> Result<(), ReplayError> {
        if turn_id == self.turn_id {
            Ok(())
        } else {
            Err(replay_invalid("payload turn id changed"))
        }
    }
}

fn decode_payload<T: DeserializeOwned>(event: &EventRecord) -> Result<T, ReplayError> {
    serde_json::from_value(event.payload.clone()).map_err(|error| {
        replay_invalid(format!("{} payload cannot be decoded: {error}", event.kind))
    })
}

trait PayloadVersion {
    fn version(&self) -> u16;
}

macro_rules! impl_payload_version {
    ($($type:ty),+ $(,)?) => {
        $(
            impl PayloadVersion for $type {
                fn version(&self) -> u16 {
                    self.event_version
                }
            }
        )+
    };
}

impl_payload_version!(
    ContextCompiledPayload,
    CapabilitiesSelectedPayload,
    CapabilitiesSelectedRefPayload,
    ModelRequestedPayload,
    ModelRequestDigestPayload,
    ModelOutputPayload,
    CapabilityRequestedPayload,
    ExecutionStartedPayload,
    ExecutionOutputPayload,
    TurnFinishedPayload,
    TurnFailedPayload,
);

/// Version 9 records the selection by reference (ADR 0030): rebuild it from
/// the packaged contracts, in the recorded page order. The selection checks
/// then compare every recorded digest with its package's.
fn rebuild_selection(
    recorded: CapabilitiesSelectedRefPayload,
) -> Result<CapabilitiesSelectedPayload, ReplayError> {
    let mut manifests = recorded
        .contracts
        .iter()
        .map(|contract| match contract.capability_id() {
            ARTIFACT_READ_ID => Ok(ditto_artifact_read::manifest()),
            ditto_artifact_sort::ID => Ok(ditto_artifact_sort::manifest()),
            ditto_web_fetch::ID => Ok(ditto_web_fetch::manifest()),
            ditto_web_fetch::search::ID => Ok(ditto_web_fetch::search::manifest()),
            super::recall::ID => Ok(super::recall::manifest()),
            REMEMBER_ID => Ok(super::memory_write::remember_manifest()),
            FORGET_ID => Ok(super::memory_write::forget_manifest()),
            _ => Err(replay_invalid("selected contract is not a builtin")),
        })
        .collect::<Result<Vec<_>, _>>()?;
    let cards = manifests
        .iter()
        .map(CapabilityCard::from)
        .collect::<Vec<_>>();
    let epoch = serde_json::from_value(serde_json::json!({
        "id": recorded.epoch_id,
        "max_working_set": cards.len(),
        "capabilities": cards,
        "invocation_revisions": recorded.contracts,
    }))
    .map_err(|error| replay_invalid(format!("selected epoch cannot be rebuilt: {error}")))?;
    let mut take = |id: &str| {
        manifests
            .iter()
            .position(|manifest| manifest.id == id)
            .map(|index| manifests.remove(index))
    };
    Ok(CapabilitiesSelectedPayload {
        event_version: recorded.event_version,
        turn_id: recorded.turn_id,
        manifest: take(ARTIFACT_READ_ID)
            .ok_or_else(|| replay_invalid("artifact.read was not selected"))?,
        sort_manifest: take(ditto_artifact_sort::ID),
        fetch_manifest: take(ditto_web_fetch::ID),
        search_manifest: take(ditto_web_fetch::search::ID),
        memory_manifest: take(super::recall::ID),
        remember_manifest: take(REMEMBER_ID),
        forget_manifest: take(FORGET_ID),
        epoch,
    })
}

fn replay_invalid(message: impl Into<String>) -> ReplayError {
    ReplayError::Invalid(message.into())
}

fn validate_ordered_snapshot(snapshot: &[EventRecord]) -> Result<(), ReplayError> {
    let mut previous_seq = None;
    let mut event_ids = BTreeSet::new();
    for event in snapshot {
        if event.event_id.trim().is_empty()
            || previous_seq.is_some_and(|sequence| event.seq <= sequence)
            || !event_ids.insert(event.event_id.as_str())
        {
            return Err(replay_invalid(
                "scope snapshot has an empty/duplicate id or non-increasing sequence",
            ));
        }
        previous_seq = Some(event.seq);
    }
    Ok(())
}
