use super::super::search::*;
use super::*;

impl ReplayProjector<'_, '_> {
    /// Replay one `web.search` call from its journaled request, optional
    /// claimed start and result, without network I/O.
    pub(super) fn replay_search_call(
        &mut self,
        request_index: u8,
        call: &ReadyCall,
    ) -> Result<Option<TurnFailure>, ReplayError> {
        let requested = self.take(event_kind::AGENT_SEARCH_REQUESTED, EventActor::Model)?;
        let request: SearchToolRequested = decode_payload(requested)?;
        if !valid_requested(&request, requested, &self.events[0])
            || request.request_index != request_index
            || request.call_id != call.call_id
            || request.arguments != call.arguments
        {
            return Err(replay_invalid("search request contradicts model call"));
        }
        let index = self.search_calls.len();
        self.search_calls.push(ReplayedSearchCall {
            requested: request.clone(),
            started: None,
            output: None,
        });
        if self.next_is_failure() {
            let time = self.events[self.index].recorded_at;
            let failure = self.take_failure()?.expect("checked failure");
            if failure.message != "search stopped before authorization"
                || failure.reason.is_some()
                || failure.request_index != Some(request_index)
                || failure.call_id.as_ref() != Some(&call.call_id)
                || !match failure.code {
                    TurnFailureCode::Cancelled => failure.evidence.is_none(),
                    TurnFailureCode::DeadlineExceeded => {
                        self.valid_deadline_failure(&failure, time)
                    }
                    _ => false,
                }
            {
                return Err(replay_invalid(
                    "invalid search authorization checkpoint failure",
                ));
            }
            return Ok(Some(failure));
        }
        let started = if self
            .events
            .get(self.index)
            .is_some_and(|event| event.kind == event_kind::AGENT_SEARCH_STARTED)
        {
            let event = self.take(event_kind::AGENT_SEARCH_STARTED, EventActor::Capability)?;
            let payload: SearchToolStarted = decode_payload(event)?;
            if self.search_claims >= ditto_web_fetch::search::MAX_SEARCHES
                || !valid_started(&payload, event, &request, requested, &self.events[0])
                || Some(payload.epoch_id.as_str())
                    != self.execution_epoch_id.as_ref().map(|id| id.as_str())
                || Some(payload.expires_at) != self.deadline
            {
                return Err(replay_invalid("search dispatch contradicts its lease"));
            }
            self.search_calls[index].started = Some(payload);
            Some(event)
        } else {
            None
        };
        let event = self.take(event_kind::AGENT_SEARCH_OUTPUT, EventActor::Capability)?;
        let output: SearchToolOutput = decode_payload(event)?;
        if !valid_output(
            &output,
            event,
            &self.events[0],
            &request,
            requested,
            started,
            self.search_claims,
            self.deadline
                .ok_or_else(|| replay_invalid("search has no turn deadline"))?,
        ) {
            return Err(replay_invalid("search output contradicts its dispatch"));
        }
        self.search_claims += u32::from(started.is_some());
        self.conversation.push(ConversationItem::ToolResult {
            call_id: call.call_id.clone(),
            content: vec![ContentPart::Structured {
                value: output.result.model_value(),
            }],
            is_error: output.result.is_error(),
        });
        self.search_calls[index].output = Some(output);
        self.tool_call_count += 1;
        Ok(None)
    }
}
