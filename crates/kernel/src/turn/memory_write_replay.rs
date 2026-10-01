use super::super::memory_write::*;
use super::*;

impl ReplayProjector<'_, '_> {
    /// Replay one `memory.remember` or `memory.forget` call: its request, the
    /// decision replay recomputes, and for a write the session-scoped
    /// `memory.written` event and context node that carry it.
    pub(super) fn replay_memory_write(
        &mut self,
        request_index: u8,
        call: &ReadyCall,
    ) -> Result<(), ReplayError> {
        let requested = self.take(event_kind::AGENT_MEMORY_WRITE_REQUESTED, EventActor::Model)?;
        let request: MemoryWriteRequested = decode_payload(requested)?;
        if !valid_requested(&request, requested, &self.events[0])
            || request.request_index != request_index
            || request.call_id != call.call_id
            || request.capability_id != call.capability_id
            || request.arguments != call.arguments
        {
            return Err(replay_invalid(
                "memory write request contradicts model call",
            ));
        }
        let event = self.take(
            event_kind::AGENT_MEMORY_WRITE_OUTPUT,
            EventActor::Capability,
        )?;
        let output: MemoryWriteOutput = decode_payload(event)?;
        if !valid_output(&output, event, &request, requested, &self.events[0]) {
            return Err(replay_invalid(
                "memory write output contradicts its request",
            ));
        }
        let snapshot = self.snapshot;
        let session = self.session_id.clone();
        let caused = snapshot
            .iter()
            .filter(|record| {
                record.kind == event_kind::MEMORY_WRITTEN
                    && record.causation_id.as_deref() == Some(&requested.event_id)
            })
            .count();
        let decided = |before_seq: i64, at| {
            refusal(
                request.write.as_ref(),
                self.read_external_content,
                self.memory_write_count,
                |target| active_memory_in(snapshot, &session, target, before_seq, at),
            )
        };
        let consistent = match &output.result {
            MemoryWriteResult::Refused { code } => {
                caused == 0 && decided(event.seq, event.recorded_at) == Some(*code)
            }
            MemoryWriteResult::Remembered { .. } | MemoryWriteResult::Forgotten { .. } => {
                caused == 1
                    && written_records(snapshot, &request, requested, &output, event).is_some_and(
                        |(written, _)| decided(written.seq, written.recorded_at).is_none(),
                    )
            }
        };
        if !consistent {
            return Err(replay_invalid(
                "memory write contradicts its rules or its records",
            ));
        }
        self.memory_write_count += u32::from(!output.result.is_error());
        self.conversation.push(ConversationItem::ToolResult {
            call_id: call.call_id.clone(),
            content: vec![ContentPart::Structured {
                value: output.result.model_value(),
            }],
            is_error: output.result.is_error(),
        });
        self.memory_writes.push(ReplayedMemoryWrite {
            requested: request,
            output,
        });
        self.tool_call_count += 1;
        Ok(())
    }
}
