use super::super::memory_write::{
    self as write, MemoryWrite, MemoryWriteDeriver, MemoryWriteOutput, MemoryWriteRefusal,
    MemoryWriteRequested, MemoryWriteResult, MemoryWrittenPayload,
};
use super::*;
use crate::TrustedContextNodeDraft;
use ditto_capability::{CanonicalInvocation, InvocableCapabilityBinding};

impl DittoKernel {
    /// Normalize, check and perform one `memory.remember` or `memory.forget`
    /// call (ADR 0031), journaling the request and the result in the turn.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn run_memory_write(
        &self,
        scope: &TurnScope,
        cause: &mut String,
        request_index: u8,
        call: &ReadyCall,
        binding: &InvocableCapabilityBinding,
        authorizer: &InvocationAuthorizer,
        read_external_content: bool,
        writes: &mut u32,
    ) -> Result<MemoryWriteResult, TurnRunError> {
        let deriver = MemoryWriteDeriver::for_capability(&call.capability_id);
        let canonical = match UntrustedToolCall::new(
            call.call_id.to_string(),
            call.capability_id.clone(),
            call.arguments.clone(),
        ) {
            Ok(raw) => match InvocationCompiler::compile(binding, raw, &deriver) {
                Ok(invocation) => Some(invocation),
                Err(InvocationError::ArgumentsSchema { .. } | InvocationError::Deriver(_)) => None,
                Err(_) => {
                    return Err(TurnRunError::Internal(
                        "memory write invocation compilation failed",
                    ));
                }
            },
            Err(_) => None,
        };
        let normalized = canonical
            .as_ref()
            .map(|invocation| {
                write::write_from_normalized(&call.capability_id, invocation.normalized_arguments())
                    .ok_or(TurnRunError::Internal(
                        "normalized memory write is incomplete",
                    ))
            })
            .transpose()?;
        let requested = self.append_turn_payload(
            scope,
            EventActor::Model,
            event_kind::AGENT_MEMORY_WRITE_REQUESTED,
            &MemoryWriteRequested {
                event_version: 1,
                turn_id: scope.turn_id.clone(),
                request_index,
                call_id: call.call_id.clone(),
                capability_id: call.capability_id.clone(),
                arguments: call.arguments.clone(),
                write: normalized.clone(),
            },
            Some(cause.clone()),
            Some(call.call_id.to_string()),
        )?;
        *cause = requested.event_id.clone();
        let result = match (canonical, normalized) {
            (Some(invocation), Some(memory)) => self.commit_memory_write(
                scope,
                &requested,
                call,
                &invocation,
                &memory,
                authorizer,
                read_external_content,
                writes,
            )?,
            _ => MemoryWriteResult::Refused {
                code: MemoryWriteRefusal::InvalidArguments,
            },
        };
        let event = self.append_turn_payload(
            scope,
            EventActor::Capability,
            event_kind::AGENT_MEMORY_WRITE_OUTPUT,
            &MemoryWriteOutput {
                event_version: 1,
                turn_id: scope.turn_id.clone(),
                request_index,
                call_id: call.call_id.clone(),
                result: result.clone(),
            },
            Some(requested.event_id.clone()),
            Some(call.call_id.to_string()),
        )?;
        *cause = event.event_id;
        Ok(result)
    }

    /// Check a write under the context admission gate and, when no rule
    /// refuses it, record it: a session-scoped `memory.written` event and the
    /// context node it sources.
    #[allow(clippy::too_many_arguments)]
    fn commit_memory_write(
        &self,
        scope: &TurnScope,
        requested: &EventRecord,
        call: &ReadyCall,
        invocation: &CanonicalInvocation,
        memory: &MemoryWrite,
        authorizer: &InvocationAuthorizer,
        read_external_content: bool,
        writes: &mut u32,
    ) -> Result<MemoryWriteResult, TurnRunError> {
        let session = scope.session_id.as_str();
        let _gate = self
            .inner
            .context_admission_gate
            .lock()
            .map_err(|_| KernelError::ContextAdmissionGatePoisoned)?;
        let high_water = self.inner.events.latest_seq().map_err(KernelError::from)?;
        self.inner
            .context_projection
            .synchronize_through(&self.inner.events, high_water)
            .map_err(KernelError::from)?;
        let target_active = match memory.target() {
            Some(target) => self
                .memory_snapshot_locked(session, high_water)?
                .candidates()
                .iter()
                .any(|node| node.id == target && write::is_memory(node)),
            None => true,
        };
        if let Some(code) = write::refusal(Some(memory), read_external_content, *writes, |_| {
            target_active
        }) {
            return Ok(MemoryWriteResult::Refused { code });
        }
        let now = Utc::now();
        let permit = match authorizer.authorize_with_lease(invocation, write::LEASE_ID, now) {
            Ok(AuthorizationOutcome::Permitted(permit)) => permit,
            _ => {
                return Err(TurnRunError::Internal(
                    "memory write lease contradicted its contract",
                ));
            }
        };
        authorizer
            .claim_execution(permit, invocation, now)
            .map_err(|_| TurnRunError::Internal("memory write claim failed"))?;
        let written = self.append_and_publish(NewEvent {
            session_id: Some(session.to_owned()),
            task_id: None,
            actor: EventActor::Model,
            kind: event_kind::MEMORY_WRITTEN.to_owned(),
            payload: serde_json::to_value(MemoryWrittenPayload {
                event_version: 1,
                turn_id: scope.turn_id.clone(),
                call_id: call.call_id.clone(),
                write: memory.clone(),
            })?,
            causation_id: Some(requested.event_id.clone()),
            correlation_id: Some(session.to_owned()),
            span_id: Some(call.call_id.to_string()),
        })?;
        // Other sessions' runs may have appended in between; no context node,
        // since this gate orders every admission.
        self.inner
            .context_projection
            .synchronize_through(&self.inner.events, written.seq)
            .map_err(KernelError::from)?;
        let node = write::memory_node(&written.event_id, memory);
        let validated = self
            .inner
            .context_projection
            .validate_draft(
                &self.inner.events,
                written.seq,
                &TrustedContextNodeDraft::session(session, node.clone()),
            )
            .map_err(KernelError::from)?;
        match self.commit_context_node(&validated) {
            Ok(_) | Err(KernelError::CommittedButProjectionUnavailable { .. }) => {}
            Err(error) => return Err(error.into()),
        }
        *writes += 1;
        Ok(MemoryWriteResult::written(memory, node.id))
    }
}
