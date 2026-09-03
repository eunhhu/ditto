use std::collections::BTreeSet;

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use ditto_artifact_read::{
    ARTIFACT_READ_ID, ARTIFACT_READ_VERSION, ArtifactReadDeriver, ArtifactReadError,
    ArtifactReadResource, ArtifactReadResult, capability_schema, validate_artifact_read_manifest,
};
use ditto_capability::{CapabilityCard, CapabilityDeriver, CapabilityRevision, CapabilitySchema};
use ditto_context::{CompiledContext, ContextCapsule, ContextCompiler, TaskSignature};
use ditto_model::{
    CancellationId, ContentPart, ConversationItem, ExecutionEpochId, FinishReason,
    GenerationControls, MessageRole, ModelEvent, ModelFeature, ModelRequest, OutputConstraint,
    ParallelToolCalls, ProviderCallId, ToolCallBuffer, ToolChoice, ToolUsePolicy,
};
use ditto_protocol::{EventActor, EventRecord, event_kind};
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::Value;

use crate::normalize_input_text;

use super::shared::{
    ReadyCall, append_assistant_text, bounded_turn_failure_message, stable_system_prefix,
    turn_failure_code_for_model,
};
use super::types::{
    ArtifactReadTurnOutcome, ArtifactReadTurnReplay, ArtifactReadTurnStatus,
    CapabilitiesSelectedPayload, CapabilityRequestedPayload, ContextCompiledPayload,
    ExecutionOutputPayload, ExecutionStartedPayload, MAX_ASSISTANT_TEXT_BYTES,
    MAX_MODEL_EVENTS_PER_REQUEST, MAX_MODEL_OUTPUT_BYTES_PER_REQUEST, MAX_MODEL_OUTPUT_EVENT_BYTES,
    MAX_MODEL_REQUESTS, MAX_TURN_DURATION, MAX_TURN_FAILURE_MESSAGE_BYTES, ModelOutputPayload,
    ModelRequestedPayload, ReplayError, ReplayedArtifactReadCall, ReplayedReadOnlyTurn,
    TURN_PAYLOAD_VERSION, TurnFailedPayload, TurnFailure, TurnFailureCode, TurnFailureEvidence,
    TurnFinishedPayload, TurnSequenceSpan,
};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct InputPayload {
    text: String,
}

fn validate_compiled_context_payload(
    compiled: &CompiledContext,
    capsule: &ContextCapsule,
    input_text: &str,
    accepted_at: DateTime<Utc>,
) -> Result<(), ReplayError> {
    let signature = TaskSignature {
        request: input_text.to_owned(),
        active_goal: None,
        entities: Vec::new(),
        constraints: Vec::new(),
        expected_effect: Some("local content read".into()),
    };
    ContextCompiler::default()
        .validate_compiled(&signature, compiled, capsule, None, accepted_at)
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
        let normalized_input =
            normalize_input_text(&input.text).map_err(|error| replay_invalid(error.to_string()))?;
        if normalized_input != input.text {
            return Err(replay_invalid("recorded input text is not normalized"));
        }
        let input_text = input.text;
        Ok(Self {
            events,
            snapshot,
            index: 1,
            turn_id,
            session_id,
            task_id,
            context: None,
            schemas: None,
            execution_epoch_id: None,
            conversation: vec![ConversationItem::Message {
                role: MessageRole::User,
                content: vec![ContentPart::Text {
                    text: input_text.clone(),
                }],
            }],
            all_call_ids: BTreeSet::new(),
            total_text_bytes: 0,
            tool_call_count: 0,
            request_count: 0,
            deadline: None,
            input_recorded_at: first.recorded_at,
            input_text,
            context_payload: None,
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
        let context: ContextCompiledPayload = decode_versioned(context_event)?;
        self.require_turn_id(&context.turn_id)?;
        if context_event.span_id.is_some() {
            return Err(replay_invalid("context.compiled must not carry a span id"));
        }
        validate_compiled_context_payload(
            &context.compiled,
            &context.capsule,
            &self.input_text,
            self.input_recorded_at,
        )?;
        self.validate_context_sources(&context, context_event)?;
        self.context = Some(context.capsule.clone());
        self.context_payload = Some(context);

        if let Some(failure) = self.take_capability_stage_failure()? {
            return Ok(ArtifactReadTurnReplay::Failed { failure });
        }
        let selected_event = self.take(event_kind::CAPABILITIES_SELECTED, EventActor::System)?;
        let selected: CapabilitiesSelectedPayload = decode_versioned(selected_event)?;
        self.require_turn_id(&selected.turn_id)?;
        if selected_event.span_id.is_some() {
            return Err(replay_invalid(
                "capabilities.selected must not carry a span id",
            ));
        }
        validate_artifact_read_manifest(&selected.manifest)
            .map_err(|error| replay_invalid(error.to_string()))?;
        let expected_schema = capability_schema();
        if selected.schemas != vec![expected_schema.clone()]
            || selected.schemas[0].id != selected.manifest.id
            || selected.schemas[0].version != selected.manifest.version
        {
            return Err(replay_invalid(
                "selected schemas do not equal the exact artifact.read contract",
            ));
        }
        if !selected.epoch.invocation_revisions().is_empty() {
            let deriver = ArtifactReadDeriver::default();
            let expected_revision = CapabilityRevision::from_contract(
                &selected.manifest,
                &expected_schema,
                deriver.revision().clone(),
            )
            .map_err(|error| replay_invalid(error.to_string()))?;
            if selected.epoch.invocation_revisions() != [expected_revision] {
                return Err(replay_invalid(
                    "selected invocation revision does not equal the exact artifact.read contract",
                ));
            }
        }
        let selected_epoch_id = ExecutionEpochId::new(selected.epoch.id().to_owned())
            .map_err(|error| replay_invalid(error.to_string()))?;
        let expected_card = CapabilityCard::from(&selected.manifest);
        let selected_cards = selected.epoch.capabilities();
        if selected.epoch.max_working_set() != 1 || selected_cards.len() != 1 {
            return Err(replay_invalid(
                "selected epoch is not the single installed artifact.read capability",
            ));
        }
        let actual_card = &selected_cards[0];
        if actual_card.id != expected_card.id
            || actual_card.namespace != expected_card.namespace
            || actual_card.kind != expected_card.kind
            || actual_card.summary != expected_card.summary
            || actual_card.minimum_effect != expected_card.minimum_effect
            || actual_card.maximum_effect != expected_card.maximum_effect
            || actual_card.placement_modes != expected_card.placement_modes
        {
            return Err(replay_invalid(
                "selected epoch card contradicts the validated artifact.read manifest",
            ));
        }
        self.execution_epoch_id = Some(selected_epoch_id);
        self.schemas = Some(selected.schemas.clone());
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
            let persisted: ModelRequestedPayload = decode_versioned(request_event)?;
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
            if let Some(message) = post_request_contract_failure {
                let failure_event_time = self
                    .events
                    .get(self.index)
                    .ok_or_else(|| replay_invalid("post-request contract failure is truncated"))?
                    .recorded_at;
                let failure = self
                    .take_failure()?
                    .ok_or_else(|| replay_invalid("post-request contract failure is missing"))?;
                let valid = match failure.code {
                    TurnFailureCode::Cancelled => {
                        failure.message
                            == "turn was cancelled after persisting a model request and before driver invocation"
                            && failure.evidence.is_none()
                    }
                    TurnFailureCode::DeadlineExceeded => {
                        failure.message
                            == "turn deadline elapsed after persisting a model request and before driver invocation"
                            && self.valid_deadline_failure(&failure, failure_event_time)
                    }
                    TurnFailureCode::DriverContract => {
                        failure.message == bounded_turn_failure_message(&message)
                            && failure.evidence.is_none()
                    }
                    _ => false,
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
                let output: ModelOutputPayload = decode_versioned(output_event)?;
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
                output
                    .stream_event
                    .validate()
                    .map_err(|error| replay_invalid(error.to_string()))?;
                expected_sequence = expected_sequence.saturating_add(1);
                event_count += 1;
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
                        if capability_id != ARTIFACT_READ_ID {
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
                                let failure = self.take_exact_failure(
                                    TurnFailureCode::Protocol,
                                    error.to_string(),
                                    Some(request_index as u8),
                                    Some(error.call_id().clone()),
                                )?;
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
                                let failure = self.take_exact_failure(
                                    TurnFailureCode::Protocol,
                                    error.to_string(),
                                    Some(request_index as u8),
                                    Some(error.call_id().clone()),
                                )?;
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
                        if capability_id != ARTIFACT_READ_ID {
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
                                let failure = self.take_exact_failure(
                                    TurnFailureCode::Protocol,
                                    error.to_string(),
                                    Some(request_index as u8),
                                    Some(error.call_id().clone()),
                                )?;
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
                        let valid = match failure.code {
                            TurnFailureCode::Cancelled => {
                                failure.message == "turn was cancelled before capability request"
                                    && failure.evidence.is_none()
                            }
                            TurnFailureCode::DeadlineExceeded => {
                                failure.message
                                    == "turn deadline elapsed before capability execution"
                                    && self.valid_deadline_failure(&failure, failure_event_time)
                            }
                            _ => false,
                        };
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

                    let capability_event =
                        self.take(event_kind::CAPABILITY_REQUESTED, EventActor::Model)?;
                    let capability: CapabilityRequestedPayload =
                        decode_versioned(capability_event)?;
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
                        let valid = match failure.code {
                            TurnFailureCode::Cancelled => {
                                failure.message
                                    == "turn was cancelled after capability request and before execution started"
                                    && failure.evidence.is_none()
                            }
                            TurnFailureCode::DeadlineExceeded => {
                                failure.message
                                    == "turn deadline elapsed after capability request and before execution started"
                                    && self.valid_deadline_failure(&failure, failure_event_time)
                            }
                            _ => false,
                        };
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
                    let started: ExecutionStartedPayload = decode_versioned(started_event)?;
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
                        let valid_checkpoint_failure = match failure.code {
                            TurnFailureCode::Cancelled => {
                                matches!(
                                    failure.message.as_str(),
                                    "turn was cancelled after execution started and before its result"
                                        | "turn was cancelled after the artifact read and before its result"
                                ) && failure.evidence.is_none()
                            }
                            TurnFailureCode::DeadlineExceeded => {
                                matches!(
                                    failure.message.as_str(),
                                    "turn deadline elapsed after execution started and before its result"
                                        | "turn deadline elapsed after the artifact read and before its result"
                                ) && self.valid_deadline_failure(&failure, failure_event_time)
                            }
                            _ => false,
                        };
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
                    let output: ExecutionOutputPayload = decode_versioned(output_event)?;
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
                    if self.tool_call_count == 0 {
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
                        let valid = match failure.code {
                            TurnFailureCode::Cancelled => {
                                failure.message
                                    == "turn was cancelled after final model output and before turn completion"
                                    && failure.evidence.is_none()
                            }
                            TurnFailureCode::DeadlineExceeded => {
                                failure.message
                                    == "turn deadline elapsed after final model output and before turn completion"
                                    && self.valid_deadline_failure(&failure, failure_event_time)
                            }
                            _ => false,
                        };
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
                    let finished: TurnFinishedPayload = decode_versioned(finished_event)?;
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
            || request.stable_system_prefix != stable_system_prefix()
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
                choice: if request_index == 0 {
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
        let payload: TurnFailedPayload = decode_versioned(event)?;
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
            TurnFailureCode::Cancelled => {
                failure.message == "turn was cancelled before context compilation"
                    && failure.evidence.is_none()
            }
            TurnFailureCode::DeadlineExceeded => {
                failure.message == "turn deadline elapsed before context compilation"
                    && self.valid_deadline_failure(&failure, failure_event_time)
            }
            TurnFailureCode::ContextCompilation => {
                valid_context_compilation_failure_message(&failure.message)
                    && failure.evidence.is_none()
            }
            _ => false,
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
        let valid = (failure.code == TurnFailureCode::CapabilityUnavailable
            && failure.message == "installed artifact.read capability is unavailable"
            && failure.evidence.is_none())
            || (failure.code == TurnFailureCode::CapabilityContract
                && valid_capability_contract_failure_message(&failure.message)
                && failure.evidence.is_none());
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
            TurnFailureCode::Cancelled => {
                failure.message == "turn was cancelled before a model request"
                    && failure.evidence.is_none()
            }
            TurnFailureCode::DeadlineExceeded => {
                failure.message == "turn deadline elapsed before a model request"
                    && self.valid_deadline_failure(&failure, failure_event_time)
            }
            TurnFailureCode::DriverContract => {
                valid_driver_contract_failure_message(&failure.message, request_index)
                    && failure.evidence.is_none()
            }
            _ => false,
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
        let valid = match failure.code {
            TurnFailureCode::Cancelled => {
                failure.evidence.is_none()
                    && (failure.message == "turn was cancelled while awaiting model output"
                        || (event_count == 0
                            && failure.message
                                == "turn was cancelled after persisting a model request and before driver invocation"))
            }
            TurnFailureCode::DeadlineExceeded => {
                (failure.message == "turn deadline elapsed while awaiting model output"
                    || (event_count == 0
                        && failure.message
                            == "turn deadline elapsed after persisting a model request and before driver invocation"))
                    && self.valid_deadline_failure(&failure, failure_event_time)
            }
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
            }
            _ => false,
        };
        if !valid || failure.request_index != Some(request_index as u8) || failure.call_id.is_some()
        {
            return Err(replay_invalid(
                "turn.failed is not valid while awaiting model output",
            ));
        }
        Ok(Some(failure))
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

fn decode_versioned<T: DeserializeOwned + PayloadVersion>(
    event: &EventRecord,
) -> Result<T, ReplayError> {
    let payload: T = decode_payload(event)?;
    if payload.version() != TURN_PAYLOAD_VERSION {
        return Err(replay_invalid(format!(
            "{} payload version is unsupported",
            event.kind
        )));
    }
    Ok(payload)
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
    ModelRequestedPayload,
    ModelOutputPayload,
    CapabilityRequestedPayload,
    ExecutionStartedPayload,
    ExecutionOutputPayload,
    TurnFinishedPayload,
    TurnFailedPayload,
);

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

fn valid_context_compilation_failure_message(message: &str) -> bool {
    let truncated = message.ends_with("...[truncated]");
    (message.starts_with("context candidate ")
        && (truncated || message.ends_with(" appears more than once")))
        || (message.starts_with("policy-required context ")
            && (truncated || message.ends_with(" has an empty reason")))
        || (message.starts_with("required context ")
            && (truncated
                || message.contains(" is invalid: ")
                || (message.contains(" costs ")
                    && message.contains("exceeding the absolute ceiling"))))
        || (message.starts_with("included context node ")
            && (truncated || message.ends_with(" has no source event provenance")))
        || (message
            .starts_with("included context provenance does not resolve in the current scope: ")
            && (truncated || message.len() <= MAX_TURN_FAILURE_MESSAGE_BYTES))
}

fn valid_capability_contract_failure_message(message: &str) -> bool {
    const MANIFEST_FIELDS: [&str; 21] = [
        "id",
        "version",
        "namespace",
        "kind",
        "summary",
        "runtime.type",
        "runtime.lazy",
        "runtime.command",
        "runtime.idle_ttl_ms",
        "placement.modes",
        "placement.requires",
        "effects.minimum",
        "effects.maximum",
        "effects.resources",
        "policy.approval",
        "policy.secret_handles",
        "verification.default",
        "retrieval.intents",
        "retrieval.negative_examples",
        "retrieval.aliases",
        "retrieval.complements",
    ];
    message == "artifact.read level-2 schema does not match the installed manifest"
        || message == "artifact.read could not be selected as the sole execution capability"
        || MANIFEST_FIELDS.iter().any(|field| {
            message
                == format!("artifact.read manifest does not match the builtin contract ({field})")
        })
}

fn valid_driver_contract_failure_message(message: &str, request_index: usize) -> bool {
    message
        == format!(
            "driver does not support generation control tool_use.choice: {}",
            if request_index == 0 {
                "Required"
            } else {
                "Auto"
            }
        )
        || message == "driver does not support generation control tool_use.parallel_calls: Forbid"
        || (message.starts_with("driver ")
            && message.contains(" does not emit required features ")
            && (message.ends_with("[Text]")
                || message.ends_with("[ToolCalls]")
                || message.ends_with("[Text, ToolCalls]")
                || message.ends_with("[ToolCalls, Text]")))
}
