use std::{cell::Cell, collections::BTreeSet};

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use ditto_artifact_read::{
    ARTIFACT_READ_ID, ARTIFACT_READ_VERSION, ArtifactReadAuthority, ArtifactReadDeriver,
    ArtifactReadNormalizer, ArtifactReadResource, ArtifactReadResult, capability_schema,
    validate_artifact_read_manifest,
};
use ditto_capability::{
    CapabilityDeriver, CapabilitySchema, ExecutionEpoch, InvocationCompiler, InvocationError,
    UntrustedToolCall,
};
use ditto_context::{
    CompiledContext, ContextCandidate, ContextCapsule, ContextCompiler, TaskSignature,
};
use ditto_model::{
    CancellationId, CancellationToken, ContentPart, ConversationItem, ExecutionEpochId,
    FeatureRequest, FinishReason, GenerationControls, MessageRole, ModelDriver, ModelEvent,
    ModelFeature, ModelRequest, ModelRequestId, ModelTurn, OutputConstraint, ParallelToolCalls,
    ProviderCallId, RequestControl, ToolCallBuffer, ToolChoice, ToolUsePolicy,
};
use ditto_policy::{AuthorizationOutcome, PolicyError, StaticPolicy};
use ditto_protocol::{
    EventActor, EventQuery, EventRecord, NewEvent, SubmitInputCommand, event_kind,
};
use futures_util::StreamExt;
use serde::Serialize;
use serde_json::Value;
use ulid::Ulid;

use crate::{DittoKernel, KernelError, normalize_identifier, normalize_input_text};

use super::shared::{
    ReadyCall, append_assistant_text, bounded_turn_failure_message, stable_system_prefix,
    turn_failure_code_for_model,
};
use super::types::{
    ArtifactReadTurnOutcome, ArtifactReadTurnStatus, CapabilitiesSelectedPayload,
    CapabilityRequestedPayload, ContextCompiledPayload, ExecutionOutputPayload,
    ExecutionStartedPayload, MAX_ASSISTANT_TEXT_BYTES, MAX_MODEL_EVENTS_PER_REQUEST,
    MAX_MODEL_OUTPUT_BYTES_PER_REQUEST, MAX_MODEL_OUTPUT_EVENT_BYTES, MAX_MODEL_REQUESTS,
    MAX_TURN_DURATION, ModelOutputPayload, ModelRequestedPayload, ReadOnlyTurnControl,
    TURN_PAYLOAD_VERSION, TurnFailedPayload, TurnFailure, TurnFailureCode, TurnFailureEvidence,
    TurnFinishedPayload, TurnRunError,
};

#[derive(Clone)]
struct TurnScope {
    turn_id: String,
    session_id: String,
    task_id: String,
    request_count: Cell<u8>,
    tool_call_count: Cell<u8>,
    effective_deadline: Cell<Option<DateTime<Utc>>>,
}

enum ContextProvenanceError {
    Kernel(KernelError),
    Invalid(String),
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
        let text = normalize_input_text(&command.text)?;

        let scope = TurnScope {
            turn_id: format!("turn_{}", Ulid::new()),
            session_id: normalize_identifier(command.session_id, "session")?,
            task_id: normalize_identifier(command.task_id, "task")?,
            request_count: Cell::new(0),
            tool_call_count: Cell::new(0),
            effective_deadline: Cell::new(None),
        };
        if self.task_is_completed(&scope.session_id, &scope.task_id)? {
            return Err(TurnRunError::Kernel(KernelError::InvalidCommand(format!(
                "task {} is already completed",
                scope.task_id
            ))));
        }
        let mut input = NewEvent::user_input(
            scope.session_id.clone(),
            Some(scope.task_id.clone()),
            text.clone(),
        );
        input.correlation_id = Some(scope.turn_id.clone());
        let input_event = self.append_and_publish(input)?;
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
        let mut cause = input_event.event_id;

        if cancellation.is_cancelled() {
            return Err(self.persist_turn_failure(
                &scope,
                &cause,
                TurnFailureCode::Cancelled,
                "turn was cancelled before context compilation",
                None,
                None,
            ));
        }
        if deadline_expired(deadline) {
            return Err(self.persist_turn_failure(
                &scope,
                &cause,
                TurnFailureCode::DeadlineExceeded,
                "turn deadline elapsed before context compilation",
                None,
                None,
            ));
        }

        let signature = TaskSignature {
            request: text.clone(),
            active_goal: None,
            entities: Vec::new(),
            constraints: Vec::new(),
            expected_effect: Some("local content read".into()),
        };
        let compiled = match ContextCompiler::default().compile(
            &signature,
            context_candidates,
            None,
            accepted_at,
        ) {
            Ok(compiled) => compiled,
            Err(error) => {
                return Err(self.persist_turn_failure(
                    &scope,
                    &cause,
                    TurnFailureCode::ContextCompilation,
                    error.to_string(),
                    None,
                    None,
                ));
            }
        };
        let capsule = ContextCapsule::from(&compiled);
        let provenance_through_seq = self.latest_event_seq()?;
        if let Err(error) =
            self.validate_compiled_context_provenance(&scope, &compiled, provenance_through_seq)
        {
            return match error {
                ContextProvenanceError::Kernel(error) => Err(TurnRunError::Kernel(error)),
                ContextProvenanceError::Invalid(message) => Err(self.persist_turn_failure(
                    &scope,
                    &cause,
                    TurnFailureCode::ContextCompilation,
                    message,
                    None,
                    None,
                )),
            };
        }
        let context_event = self.append_turn_payload(
            &scope,
            EventActor::System,
            event_kind::CONTEXT_COMPILED,
            &ContextCompiledPayload {
                event_version: TURN_PAYLOAD_VERSION,
                turn_id: scope.turn_id.clone(),
                provenance_through_seq,
                compiled: compiled.clone(),
                capsule: capsule.clone(),
            },
            Some(cause),
            None,
        )?;
        cause = context_event.event_id;

        let Some(manifest) = self.inner.capabilities.get(ARTIFACT_READ_ID) else {
            return Err(self.persist_turn_failure(
                &scope,
                &cause,
                TurnFailureCode::CapabilityUnavailable,
                "installed artifact.read capability is unavailable",
                None,
                None,
            ));
        };
        if let Err(error) = validate_artifact_read_manifest(manifest) {
            return Err(self.persist_turn_failure(
                &scope,
                &cause,
                TurnFailureCode::CapabilityContract,
                error.to_string(),
                None,
                None,
            ));
        }

        let schema = capability_schema();
        if schema.id != manifest.id || schema.version != manifest.version {
            return Err(self.persist_turn_failure(
                &scope,
                &cause,
                TurnFailureCode::CapabilityContract,
                "artifact.read level-2 schema does not match the installed manifest",
                None,
                None,
            ));
        }
        if let Err(error) = schema.validate() {
            return Err(self.persist_turn_failure(
                &scope,
                &cause,
                TurnFailureCode::CapabilityContract,
                error.to_string(),
                None,
                None,
            ));
        }

        let manifest = manifest.clone();
        let deriver = ArtifactReadDeriver::default();
        let mut epoch = ExecutionEpoch::new(1);
        if epoch
            .page_in_invocable(&manifest, &schema, deriver.revision().clone())
            .map_err(|error| {
                self.persist_turn_failure(
                    &scope,
                    &cause,
                    TurnFailureCode::CapabilityContract,
                    error.to_string(),
                    None,
                    None,
                )
            })?
            != 1
            || epoch.capabilities().len() != 1
            || epoch.capabilities()[0].id != ARTIFACT_READ_ID
        {
            return Err(self.persist_turn_failure(
                &scope,
                &cause,
                TurnFailureCode::CapabilityContract,
                "artifact.read could not be selected as the sole execution capability",
                None,
                None,
            ));
        }
        let execution_epoch_id = match ExecutionEpochId::new(epoch.id.clone()) {
            Ok(id) => id,
            Err(error) => {
                return Err(self.persist_turn_failure(
                    &scope,
                    &cause,
                    TurnFailureCode::CapabilityContract,
                    error.to_string(),
                    None,
                    None,
                ));
            }
        };
        let schemas = vec![schema.clone()];
        let selected_event = self.append_turn_payload(
            &scope,
            EventActor::System,
            event_kind::CAPABILITIES_SELECTED,
            &CapabilitiesSelectedPayload {
                event_version: TURN_PAYLOAD_VERSION,
                turn_id: scope.turn_id.clone(),
                manifest: manifest.clone(),
                epoch: epoch.clone(),
                schemas: schemas.clone(),
            },
            Some(cause),
            None,
        )?;
        cause = selected_event.event_id;

        let authority = ArtifactReadAuthority::new(self.inner.artifacts.clone());
        let mut conversation = vec![ConversationItem::Message {
            role: MessageRole::User,
            content: vec![ContentPart::Text { text }],
        }];
        let mut all_call_ids = BTreeSet::new();
        let mut total_text_bytes = 0_usize;
        let mut tool_call_count = 0_u8;

        for request_index in 0..MAX_MODEL_REQUESTS {
            if cancellation.is_cancelled() {
                return Err(self.persist_turn_failure(
                    &scope,
                    &cause,
                    TurnFailureCode::Cancelled,
                    "turn was cancelled before a model request",
                    Some(request_index as u8),
                    None,
                ));
            }
            if deadline_expired(deadline) {
                return Err(self.persist_turn_failure(
                    &scope,
                    &cause,
                    TurnFailureCode::DeadlineExceeded,
                    "turn deadline elapsed before a model request",
                    Some(request_index as u8),
                    None,
                ));
            }

            let request = match build_model_request(
                &scope,
                request_index,
                execution_epoch_id.clone(),
                capsule.clone(),
                schemas.clone(),
                conversation.clone(),
                deadline,
            ) {
                Ok(request) => request,
                Err(error) => {
                    return Err(self.persist_turn_failure(
                        &scope,
                        &cause,
                        TurnFailureCode::DriverContract,
                        error,
                        Some(request_index as u8),
                        None,
                    ));
                }
            };
            if let Err(error) = request.validate_at(accepted_at) {
                return Err(self.persist_turn_failure(
                    &scope,
                    &cause,
                    TurnFailureCode::DriverContract,
                    error.to_string(),
                    Some(request_index as u8),
                    None,
                ));
            }
            if let Err(error) = request.validate_against(driver.descriptor()) {
                return Err(self.persist_turn_failure(
                    &scope,
                    &cause,
                    TurnFailureCode::DriverContract,
                    error.to_string(),
                    Some(request_index as u8),
                    None,
                ));
            }
            if deadline_expired(deadline) {
                return Err(self.persist_turn_failure(
                    &scope,
                    &cause,
                    TurnFailureCode::DeadlineExceeded,
                    "turn deadline elapsed before a model request",
                    Some(request_index as u8),
                    None,
                ));
            }

            let requested_event = self.append_turn_payload(
                &scope,
                EventActor::System,
                event_kind::MODEL_REQUESTED,
                &ModelRequestedPayload {
                    event_version: TURN_PAYLOAD_VERSION,
                    turn_id: scope.turn_id.clone(),
                    request_index: request_index as u8,
                    request: request.clone(),
                },
                Some(cause),
                Some(request.request_id.to_string()),
            )?;
            cause = requested_event.event_id;
            scope.request_count.set((request_index + 1) as u8);
            tokio::task::yield_now().await;
            if cancellation.is_cancelled() {
                return Err(self.persist_turn_failure(
                    &scope,
                    &cause,
                    TurnFailureCode::Cancelled,
                    "turn was cancelled after persisting a model request and before driver invocation",
                    Some(request_index as u8),
                    None,
                ));
            }
            if deadline_expired(deadline) {
                return Err(self.persist_turn_failure(
                    &scope,
                    &cause,
                    TurnFailureCode::DeadlineExceeded,
                    "turn deadline elapsed after persisting a model request and before driver invocation",
                    Some(request_index as u8),
                    None,
                ));
            }
            if let Err(error) = request.validate_at(requested_event.recorded_at) {
                return Err(self.persist_turn_failure(
                    &scope,
                    &cause,
                    TurnFailureCode::DriverContract,
                    error.to_string(),
                    Some(request_index as u8),
                    None,
                ));
            }

            // The complete request is durable before the driver receives it.
            let mut stream = driver.stream(request.clone(), cancellation.clone());
            let mut event_count = 0_usize;
            let mut model_output_bytes = 0_usize;
            let mut tool_buffer = ToolCallBuffer::default();
            let mut ready_call: Option<ReadyCall> = None;
            let mut request_text = String::new();
            let terminal;

            loop {
                if event_count == MAX_MODEL_EVENTS_PER_REQUEST {
                    return Err(self.persist_turn_failure(
                        &scope,
                        &cause,
                        TurnFailureCode::BoundExceeded,
                        format!(
                            "model request exceeded {MAX_MODEL_EVENTS_PER_REQUEST} events without a terminal"
                        ),
                        Some(request_index as u8),
                        None,
                    ));
                }
                let deadline_wait = tokio::time::sleep(duration_until(deadline));
                tokio::pin!(deadline_wait);
                let next = tokio::select! {
                    biased;
                    _ = cancellation.cancelled() => {
                        return Err(self.persist_turn_failure(
                            &scope,
                            &cause,
                            TurnFailureCode::Cancelled,
                            "turn was cancelled while awaiting model output",
                            Some(request_index as u8),
                            None,
                        ));
                    }
                    _ = &mut deadline_wait => {
                        return Err(self.persist_turn_failure(
                            &scope,
                            &cause,
                            TurnFailureCode::DeadlineExceeded,
                            "turn deadline elapsed while awaiting model output",
                            Some(request_index as u8),
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
                if cancellation.is_cancelled() {
                    return Err(self.persist_turn_failure(
                        &scope,
                        &cause,
                        TurnFailureCode::Cancelled,
                        "turn was cancelled while awaiting model output",
                        Some(request_index as u8),
                        None,
                    ));
                }
                if deadline_expired(deadline) {
                    return Err(self.persist_turn_failure(
                        &scope,
                        &cause,
                        TurnFailureCode::DeadlineExceeded,
                        "turn deadline elapsed while awaiting model output",
                        Some(request_index as u8),
                        None,
                    ));
                }
                if let ModelEvent::TextDelta { text } = &stream_event.event
                    && total_text_bytes.saturating_add(text.len()) > MAX_ASSISTANT_TEXT_BYTES
                {
                    return Err(self.persist_turn_failure(
                        &scope,
                        &cause,
                        TurnFailureCode::BoundExceeded,
                        format!("assistant text exceeded {MAX_ASSISTANT_TEXT_BYTES} bytes"),
                        Some(request_index as u8),
                        None,
                    ));
                }

                let mut output_payload = ModelOutputPayload {
                    event_version: TURN_PAYLOAD_VERSION,
                    turn_id: scope.turn_id.clone(),
                    request_index: request_index as u8,
                    request_id: request.request_id.clone(),
                    admitted_at: ceil_to_millis(Utc::now()),
                    stream_event: stream_event.clone(),
                };
                let mut encoded_output_bytes = serde_json::to_vec(&output_payload)?.len();
                if encoded_output_bytes > MAX_MODEL_OUTPUT_EVENT_BYTES {
                    return Err(self.persist_turn_failure(
                        &scope,
                        &cause,
                        TurnFailureCode::BoundExceeded,
                        format!(
                            "model output exceeded {MAX_MODEL_OUTPUT_EVENT_BYTES} encoded bytes"
                        ),
                        Some(request_index as u8),
                        None,
                    ));
                }
                if model_output_bytes.saturating_add(encoded_output_bytes)
                    > MAX_MODEL_OUTPUT_BYTES_PER_REQUEST
                {
                    return Err(self.persist_turn_failure(
                        &scope,
                        &cause,
                        TurnFailureCode::BoundExceeded,
                        format!(
                            "model request output exceeded {MAX_MODEL_OUTPUT_BYTES_PER_REQUEST} encoded bytes"
                        ),
                        Some(request_index as u8),
                        None,
                    ));
                }
                if cancellation.is_cancelled() {
                    return Err(self.persist_turn_failure(
                        &scope,
                        &cause,
                        TurnFailureCode::Cancelled,
                        "turn was cancelled while awaiting model output",
                        Some(request_index as u8),
                        None,
                    ));
                }
                let admitted_at = ceil_to_millis(Utc::now());
                if admitted_at >= deadline {
                    return Err(self.persist_turn_failure(
                        &scope,
                        &cause,
                        TurnFailureCode::DeadlineExceeded,
                        "turn deadline elapsed while awaiting model output",
                        Some(request_index as u8),
                        None,
                    ));
                }
                output_payload.admitted_at = admitted_at;
                encoded_output_bytes = serde_json::to_vec(&output_payload)?.len();
                if encoded_output_bytes > MAX_MODEL_OUTPUT_EVENT_BYTES {
                    return Err(self.persist_turn_failure(
                        &scope,
                        &cause,
                        TurnFailureCode::BoundExceeded,
                        format!(
                            "model output exceeded {MAX_MODEL_OUTPUT_EVENT_BYTES} encoded bytes"
                        ),
                        Some(request_index as u8),
                        None,
                    ));
                }
                if model_output_bytes.saturating_add(encoded_output_bytes)
                    > MAX_MODEL_OUTPUT_BYTES_PER_REQUEST
                {
                    return Err(self.persist_turn_failure(
                        &scope,
                        &cause,
                        TurnFailureCode::BoundExceeded,
                        format!(
                            "model request output exceeded {MAX_MODEL_OUTPUT_BYTES_PER_REQUEST} encoded bytes"
                        ),
                        Some(request_index as u8),
                        None,
                    ));
                }
                if cancellation.is_cancelled() {
                    return Err(self.persist_turn_failure(
                        &scope,
                        &cause,
                        TurnFailureCode::Cancelled,
                        "turn was cancelled while awaiting model output",
                        Some(request_index as u8),
                        None,
                    ));
                }
                if deadline_expired(deadline) {
                    return Err(self.persist_turn_failure(
                        &scope,
                        &cause,
                        TurnFailureCode::DeadlineExceeded,
                        "turn deadline elapsed while awaiting model output",
                        Some(request_index as u8),
                        None,
                    ));
                }
                event_count += 1;

                let output_event = self.append_turn_payload(
                    &scope,
                    EventActor::Model,
                    event_kind::MODEL_OUTPUT,
                    &output_payload,
                    Some(cause),
                    Some(request.request_id.to_string()),
                )?;
                cause = output_event.event_id;
                model_output_bytes = model_output_bytes.saturating_add(encoded_output_bytes);

                match &stream_event.event {
                    ModelEvent::TextDelta { text } => {
                        total_text_bytes += text.len();
                        request_text.push_str(text);
                        append_assistant_text(&mut conversation, text);
                    }
                    ModelEvent::ToolCallStarted {
                        call_id,
                        capability_id,
                    } => {
                        if capability_id != ARTIFACT_READ_ID {
                            return Err(self.persist_turn_failure(
                                &scope,
                                &cause,
                                TurnFailureCode::Protocol,
                                format!("unknown capability {capability_id}"),
                                Some(request_index as u8),
                                Some(call_id.clone()),
                            ));
                        }
                        if !all_call_ids.insert(call_id.clone()) {
                            return Err(self.persist_turn_failure(
                                &scope,
                                &cause,
                                TurnFailureCode::Protocol,
                                format!("duplicate epoch-wide tool call id {call_id}"),
                                Some(request_index as u8),
                                Some(call_id.clone()),
                            ));
                        }
                        if let Err(error) =
                            tool_buffer.start(call_id.clone(), capability_id.clone())
                        {
                            return Err(self.persist_turn_failure(
                                &scope,
                                &cause,
                                TurnFailureCode::Protocol,
                                error.to_string(),
                                Some(request_index as u8),
                                Some(error.call_id().clone()),
                            ));
                        }
                    }
                    ModelEvent::ToolCallArgumentDelta { call_id, delta } => {
                        if let Err(error) = tool_buffer.push_arguments(call_id, delta) {
                            return Err(self.persist_turn_failure(
                                &scope,
                                &cause,
                                TurnFailureCode::Protocol,
                                error.to_string(),
                                Some(request_index as u8),
                                Some(error.call_id().clone()),
                            ));
                        }
                    }
                    ModelEvent::ToolCallReady {
                        call_id,
                        capability_id,
                        arguments,
                    } => {
                        if capability_id != ARTIFACT_READ_ID {
                            return Err(self.persist_turn_failure(
                                &scope,
                                &cause,
                                TurnFailureCode::Protocol,
                                format!("unknown capability {capability_id}"),
                                Some(request_index as u8),
                                Some(call_id.clone()),
                            ));
                        }
                        if ready_call.is_some() {
                            return Err(self.persist_turn_failure(
                                &scope,
                                &cause,
                                TurnFailureCode::Protocol,
                                "a model request produced more than one ready tool call",
                                Some(request_index as u8),
                                Some(call_id.clone()),
                            ));
                        }
                        let rebuilt = match tool_buffer.finish(call_id) {
                            Ok(rebuilt) => rebuilt,
                            Err(error) => {
                                return Err(self.persist_turn_failure(
                                    &scope,
                                    &cause,
                                    TurnFailureCode::Protocol,
                                    error.to_string(),
                                    Some(request_index as u8),
                                    Some(error.call_id().clone()),
                                ));
                            }
                        };
                        if rebuilt != stream_event.event {
                            return Err(self.persist_turn_failure(
                                &scope,
                                &cause,
                                TurnFailureCode::Protocol,
                                "ready tool call does not match accumulated arguments",
                                Some(request_index as u8),
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
                        return Err(self.persist_turn_failure(
                            &scope,
                            &cause,
                            TurnFailureCode::Protocol,
                            "reasoning events are not permitted in this turn loop",
                            Some(request_index as u8),
                            None,
                        ));
                    }
                    ModelEvent::StructuredOutput { .. } => {
                        return Err(self.persist_turn_failure(
                            &scope,
                            &cause,
                            TurnFailureCode::Protocol,
                            "text-constrained turn received structured output",
                            Some(request_index as u8),
                            None,
                        ));
                    }
                    ModelEvent::Failed { failure } => {
                        let code = turn_failure_code_for_model(failure.kind);
                        return Err(self.persist_turn_failure(
                            &scope,
                            &cause,
                            code,
                            bounded_turn_failure_message(&failure.message),
                            Some(request_index as u8),
                            failure.call_id.clone(),
                        ));
                    }
                    ModelEvent::Completed { .. } => {
                        terminal = stream_event.event.clone();
                        break;
                    }
                    ModelEvent::UsageUpdate { .. } | ModelEvent::ProviderWarning { .. } => {}
                }
            }

            let ModelEvent::Completed {
                finish_reason,
                continuation,
            } = terminal
            else {
                unreachable!("the stream loop exits only for completed model events")
            };
            if tool_buffer.has_active_calls() {
                return Err(self.persist_turn_failure(
                    &scope,
                    &cause,
                    TurnFailureCode::Protocol,
                    "model terminal has an unfinished tool call",
                    Some(request_index as u8),
                    None,
                ));
            }
            if continuation.is_some() {
                return Err(self.persist_turn_failure(
                    &scope,
                    &cause,
                    TurnFailureCode::Protocol,
                    "provider-managed continuation is not permitted in this turn loop",
                    Some(request_index as u8),
                    None,
                ));
            }

            if finish_reason == FinishReason::ToolCalls {
                let Some(call) = ready_call else {
                    return Err(self.persist_turn_failure(
                        &scope,
                        &cause,
                        TurnFailureCode::Protocol,
                        "tool-call terminal contained no ready tool call",
                        Some(request_index as u8),
                        None,
                    ));
                };
                if request_index + 1 >= MAX_MODEL_REQUESTS {
                    return Err(self.persist_turn_failure(
                        &scope,
                        &cause,
                        TurnFailureCode::BoundExceeded,
                        format!("turn exhausted the {MAX_MODEL_REQUESTS}-request limit"),
                        Some(request_index as u8),
                        Some(call.call_id),
                    ));
                }

                // Give cancellation a deterministic checkpoint after the
                // provider terminal and before any capability request is
                // journaled.
                tokio::task::yield_now().await;
                if cancellation.is_cancelled() {
                    return Err(self.persist_turn_failure(
                        &scope,
                        &cause,
                        TurnFailureCode::Cancelled,
                        "turn was cancelled before capability request",
                        Some(request_index as u8),
                        Some(call.call_id.clone()),
                    ));
                }
                if deadline_expired(deadline) {
                    return Err(self.persist_turn_failure(
                        &scope,
                        &cause,
                        TurnFailureCode::DeadlineExceeded,
                        "turn deadline elapsed before capability execution",
                        Some(request_index as u8),
                        Some(call.call_id),
                    ));
                }
                let normalized = ArtifactReadNormalizer.normalize(&call.arguments);
                let untrusted_call = match UntrustedToolCall::new(
                    call.call_id.to_string(),
                    call.capability_id.clone(),
                    call.arguments.clone(),
                ) {
                    Ok(call) => call,
                    Err(error) => {
                        return Err(self.persist_turn_failure(
                            &scope,
                            &cause,
                            TurnFailureCode::CapabilityContract,
                            error.to_string(),
                            Some(request_index as u8),
                            Some(call.call_id),
                        ));
                    }
                };
                let canonical = match InvocationCompiler::compile(
                    &epoch,
                    &manifest,
                    &schema,
                    untrusted_call,
                    &deriver,
                ) {
                    Ok(invocation) => Some(invocation),
                    Err(InvocationError::ArgumentsSchema {
                        stage: ditto_capability::ArgumentStage::Raw,
                        ..
                    }) => None,
                    Err(error) => {
                        return Err(self.persist_turn_failure(
                            &scope,
                            &cause,
                            TurnFailureCode::CapabilityContract,
                            error.to_string(),
                            Some(request_index as u8),
                            Some(call.call_id),
                        ));
                    }
                };
                match (&canonical, &normalized) {
                    (Some(invocation), Ok(resource))
                        if invocation.normalized_arguments()
                            == &serde_json::to_value(resource)? => {}
                    (None, Err(_)) => {}
                    _ => {
                        return Err(self.persist_turn_failure(
                            &scope,
                            &cause,
                            TurnFailureCode::CapabilityContract,
                            "artifact.read schema validation and normalization disagree",
                            Some(request_index as u8),
                            Some(call.call_id),
                        ));
                    }
                }
                let capability_event = self.append_turn_payload(
                    &scope,
                    EventActor::Model,
                    event_kind::CAPABILITY_REQUESTED,
                    &CapabilityRequestedPayload {
                        event_version: TURN_PAYLOAD_VERSION,
                        turn_id: scope.turn_id.clone(),
                        request_index: request_index as u8,
                        execution_epoch_id: execution_epoch_id.clone(),
                        call_id: call.call_id.clone(),
                        capability_id: call.capability_id.clone(),
                        capability_version: ARTIFACT_READ_VERSION.into(),
                        arguments: call.arguments.clone(),
                        normalized: normalized.as_ref().ok().cloned(),
                    },
                    Some(cause),
                    Some(call.call_id.to_string()),
                )?;
                cause = capability_event.event_id;

                let authorization_through_seq = self.latest_event_seq()?;
                let authorized = match &normalized {
                    Ok(resource) => {
                        self.artifact_is_authorized(&scope, resource, authorization_through_seq)?
                    }
                    Err(_) => false,
                };
                let permit = if let Some(invocation) = canonical.as_ref() {
                    let policy_resource = authorized
                        .then(|| invocation.resources().iter().next().cloned())
                        .flatten();
                    let policy =
                        StaticPolicy::artifact_read_scope(policy_resource).map_err(|_| {
                            TurnRunError::Internal(
                                "artifact.read static policy construction failed",
                            )
                        })?;
                    match self.inner.invocation_authorizer.authorize_static(
                        invocation,
                        &policy,
                        Utc::now(),
                    ) {
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
                // Authorization is bounded by the captured high-water. Yield
                // once more so cancellation/deadline can stop the turn before
                // an execution.started claim is made.
                tokio::task::yield_now().await;
                if cancellation.is_cancelled() {
                    return Err(self.persist_turn_failure(
                        &scope,
                        &cause,
                        TurnFailureCode::Cancelled,
                        "turn was cancelled after capability request and before execution started",
                        Some(request_index as u8),
                        Some(call.call_id.clone()),
                    ));
                }
                if deadline_expired(deadline) {
                    return Err(self.persist_turn_failure(
                        &scope,
                        &cause,
                        TurnFailureCode::DeadlineExceeded,
                        "turn deadline elapsed after capability request and before execution started",
                        Some(request_index as u8),
                        Some(call.call_id.clone()),
                    ));
                }
                let started_event = self.append_turn_payload(
                    &scope,
                    EventActor::Capability,
                    event_kind::EXECUTION_STARTED,
                    &ExecutionStartedPayload {
                        event_version: TURN_PAYLOAD_VERSION,
                        turn_id: scope.turn_id.clone(),
                        request_index: request_index as u8,
                        call_id: call.call_id.clone(),
                        capability_id: ARTIFACT_READ_ID.into(),
                        capability_version: ARTIFACT_READ_VERSION.into(),
                        authorization_through_seq,
                        resource: normalized.as_ref().ok().cloned(),
                    },
                    Some(cause),
                    Some(call.call_id.to_string()),
                )?;
                cause = started_event.event_id;

                // This yield is an intentional, deterministic cancellation
                // checkpoint after the durable start and before any store read
                // or durable result.
                tokio::task::yield_now().await;
                if cancellation.is_cancelled() {
                    return Err(self.persist_turn_failure(
                        &scope,
                        &cause,
                        TurnFailureCode::Cancelled,
                        "turn was cancelled after execution started and before its result",
                        Some(request_index as u8),
                        Some(call.call_id),
                    ));
                }
                if deadline_expired(deadline) {
                    return Err(self.persist_turn_failure(
                        &scope,
                        &cause,
                        TurnFailureCode::DeadlineExceeded,
                        "turn deadline elapsed after execution started and before its result",
                        Some(request_index as u8),
                        Some(call.call_id),
                    ));
                }

                let result = match normalized {
                    Err(error) => ArtifactReadResult::error(error),
                    Ok(resource) if !authorized => ArtifactReadResult::error(
                        ditto_artifact_read::ArtifactReadError::not_authorized(
                            resource.reference().clone(),
                        ),
                    ),
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
                        authority.execute(&resource)
                    }
                };
                if cancellation.is_cancelled() {
                    return Err(self.persist_turn_failure(
                        &scope,
                        &cause,
                        TurnFailureCode::Cancelled,
                        "turn was cancelled after the artifact read and before its result",
                        Some(request_index as u8),
                        Some(call.call_id),
                    ));
                }
                if deadline_expired(deadline) {
                    return Err(self.persist_turn_failure(
                        &scope,
                        &cause,
                        TurnFailureCode::DeadlineExceeded,
                        "turn deadline elapsed after the artifact read and before its result",
                        Some(request_index as u8),
                        Some(call.call_id),
                    ));
                }
                let output_event = self.append_turn_payload(
                    &scope,
                    EventActor::Capability,
                    event_kind::EXECUTION_OUTPUT,
                    &ExecutionOutputPayload {
                        event_version: TURN_PAYLOAD_VERSION,
                        turn_id: scope.turn_id.clone(),
                        request_index: request_index as u8,
                        call_id: call.call_id.clone(),
                        capability_id: ARTIFACT_READ_ID.into(),
                        capability_version: ARTIFACT_READ_VERSION.into(),
                        result: result.clone(),
                    },
                    Some(cause),
                    Some(call.call_id.to_string()),
                )?;
                cause = output_event.event_id;
                let result_value = serde_json::to_value(&result)?;
                conversation.push(ConversationItem::ToolResult {
                    call_id: call.call_id,
                    content: vec![ContentPart::Structured {
                        value: result_value,
                    }],
                    is_error: result.is_error(),
                });
                tool_call_count = tool_call_count.saturating_add(1);
                scope.tool_call_count.set(tool_call_count);
                continue;
            }

            if ready_call.is_some() {
                return Err(self.persist_turn_failure(
                    &scope,
                    &cause,
                    TurnFailureCode::Protocol,
                    "non-tool terminal followed a ready tool call",
                    Some(request_index as u8),
                    ready_call.map(|call| call.call_id),
                ));
            }
            if tool_call_count == 0 {
                return Err(self.persist_turn_failure(
                    &scope,
                    &cause,
                    TurnFailureCode::Protocol,
                    "turn ended before executing artifact.read",
                    Some(request_index as u8),
                    None,
                ));
            }
            if request_text.is_empty() {
                return Err(self.persist_turn_failure(
                    &scope,
                    &cause,
                    TurnFailureCode::Protocol,
                    "final model request produced no assistant text",
                    Some(request_index as u8),
                    None,
                ));
            }

            tokio::task::yield_now().await;
            if cancellation.is_cancelled() {
                return Err(self.persist_turn_failure(
                    &scope,
                    &cause,
                    TurnFailureCode::Cancelled,
                    "turn was cancelled after final model output and before turn completion",
                    Some(request_index as u8),
                    None,
                ));
            }
            if deadline_expired(deadline) {
                return Err(self.persist_turn_failure(
                    &scope,
                    &cause,
                    TurnFailureCode::DeadlineExceeded,
                    "turn deadline elapsed after final model output and before turn completion",
                    Some(request_index as u8),
                    None,
                ));
            }

            let outcome = ArtifactReadTurnOutcome {
                turn_id: scope.turn_id.clone(),
                session_id: scope.session_id.clone(),
                task_id: scope.task_id.clone(),
                execution_epoch_id: execution_epoch_id.clone(),
                response: request_text,
                status: ArtifactReadTurnStatus::Unverified,
                request_count: (request_index + 1) as u8,
                tool_call_count,
            };
            self.append_turn_payload(
                &scope,
                EventActor::System,
                event_kind::TURN_FINISHED,
                &TurnFinishedPayload {
                    event_version: TURN_PAYLOAD_VERSION,
                    turn_id: scope.turn_id.clone(),
                    outcome: outcome.clone(),
                },
                Some(cause),
                None,
            )?;
            return Ok(outcome);
        }

        unreachable!("the bounded request loop returns from every terminal path")
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
        let evidence =
            (code == TurnFailureCode::DeadlineExceeded).then(|| TurnFailureEvidence::Deadline {
                deadline: scope
                    .effective_deadline
                    .get()
                    .expect("deadline is fixed before any durable turn failure"),
            });
        self.persist_turn_failure_with_evidence(
            scope,
            cause,
            code,
            message,
            request_index,
            call_id,
            evidence,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn persist_turn_failure_with_evidence(
        &self,
        scope: &TurnScope,
        cause: &str,
        code: TurnFailureCode,
        message: impl Into<String>,
        request_index: Option<u8>,
        call_id: Option<ProviderCallId>,
        evidence: Option<TurnFailureEvidence>,
    ) -> TurnRunError {
        let message = message.into();
        let failure = TurnFailure {
            turn_id: scope.turn_id.clone(),
            session_id: scope.session_id.clone(),
            task_id: scope.task_id.clone(),
            code,
            message: bounded_turn_failure_message(&message),
            request_index,
            call_id,
            evidence,
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
        let high_water = self.latest_event_seq()?;
        let mut query = EventQuery {
            session_id: Some(session_id.to_owned()),
            task_id: Some(task_id.to_owned()),
            limit: Some(1_000),
            ..EventQuery::default()
        };
        loop {
            let page = self.list_events_through(&query, high_water)?;
            if page
                .iter()
                .any(|event| event.kind == event_kind::TASK_COMPLETED)
            {
                return Ok(true);
            }
            let Some(last) = page.last() else {
                return Ok(false);
            };
            if last.seq >= high_water || page.len() < 1_000 {
                return Ok(false);
            }
            query.after_seq = Some(last.seq);
        }
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
                return Err(ContextProvenanceError::Invalid(format!(
                    "included context node {} has no source event provenance",
                    node.id
                )));
            }
            required.extend(node.source_event_ids.iter().cloned());
        }
        if required.is_empty() {
            return Ok(());
        }

        let mut query = EventQuery {
            session_id: Some(scope.session_id.clone()),
            limit: Some(1_000),
            ..EventQuery::default()
        };
        let mut found = BTreeSet::new();
        loop {
            let page = self
                .list_events_through(&query, high_water)
                .map_err(ContextProvenanceError::Kernel)?;
            if page.is_empty() {
                break;
            }
            for event in &page {
                if required.contains(&event.event_id)
                    && event
                        .task_id
                        .as_deref()
                        .is_none_or(|task_id| task_id == scope.task_id)
                {
                    found.insert(event.event_id.clone());
                }
            }
            if found.len() == required.len() {
                return Ok(());
            }
            let last_seq = page.last().map_or(0, |event| event.seq);
            if last_seq >= high_water || page.len() < 1_000 {
                break;
            }
            query.after_seq = Some(last_seq);
        }
        let missing = required.difference(&found).cloned().collect::<Vec<_>>();
        Err(ContextProvenanceError::Invalid(format!(
            "included context provenance does not resolve in the current scope: {}",
            missing.join(", ")
        )))
    }
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
            choice: if request_index == 0 {
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
