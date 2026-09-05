use super::super::sort::*;
use super::*;

impl ReplayProjector<'_, '_> {
    pub(super) fn replay_sort_call(
        &mut self,
        request_index: u8,
        call: &ReadyCall,
    ) -> Result<Option<TurnFailure>, ReplayError> {
        let grant = self
            .sort
            .clone()
            .ok_or_else(|| replay_invalid("sort has no user permission"))?;
        let requested = self.take(event_kind::AGENT_SORT_REQUESTED, EventActor::Model)?;
        let request: SortToolRequested = decode_payload(requested)?;
        if !valid_requested(&request, requested, &self.events[0])
            || request.request_index != request_index
            || request.call_id != call.call_id
            || request.arguments != call.arguments
        {
            return Err(replay_invalid("sort request contradicts model call"));
        }
        let index = self.sort_calls.len();
        self.sort_calls.push(ReplayedSortCall {
            requested: request.clone(),
            started: None,
            output: None,
        });
        if self.next_is_failure() {
            let time = self.events[self.index].recorded_at;
            let failure = self.take_failure()?.expect("checked failure");
            if failure.message != "sort stopped before authorization"
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
                    "invalid sort authorization checkpoint failure",
                ));
            }
            return Ok(Some(failure));
        }
        let started = if self
            .events
            .get(self.index)
            .is_some_and(|e| e.kind == event_kind::AGENT_SORT_STARTED)
        {
            let event = self.take(event_kind::AGENT_SORT_STARTED, EventActor::Capability)?;
            let payload: SortToolStarted = decode_payload(event)?;
            if self.sort_claimed
                || !valid_started(&payload, event, &self.events[0], &grant)
                || !start_matches_request(&payload, event, &request, requested)
                || Some(payload.epoch_id.as_str())
                    != self.execution_epoch_id.as_ref().map(|id| id.as_str())
                || Some(payload.expires_at) != self.deadline
            {
                return Err(replay_invalid(
                    "sort dispatch contradicts permission or lease",
                ));
            }
            self.sort_calls[index].started = Some(payload);
            Some(event)
        } else {
            None
        };
        let event = self.take(event_kind::AGENT_SORT_OUTPUT, EventActor::Capability)?;
        let output: SortToolOutput = decode_payload(event)?;
        if !valid_output(
            &output,
            event,
            &self.events[0],
            &grant,
            &request,
            requested,
            started,
            self.sort_claimed,
            self.deadline
                .ok_or_else(|| replay_invalid("sort has no turn deadline"))?,
        ) {
            return Err(replay_invalid("sort output contradicts dispatch"));
        }
        if matches!(
            output.result,
            SortToolResult::Error {
                code: SortToolError::LeaseExpired
            }
        ) && self
            .deadline
            .is_none_or(|deadline| event.recorded_at < deadline)
        {
            return Err(replay_invalid("sort lease expired before its deadline"));
        }
        if let SortToolResult::Verified { .. } = &output.result {
            let started = started.ok_or_else(|| replay_invalid("sort output has no start"))?;
            if !self
                .snapshot
                .iter()
                .any(|root| valid_output_root(root, &output, event, started))
            {
                return Err(replay_invalid("sort output has no matching artifact root"));
            }
        }
        self.sort_claimed |= started.is_some();
        self.conversation.push(ConversationItem::ToolResult {
            call_id: call.call_id.clone(),
            content: vec![ContentPart::Structured {
                value: output.result.model_value(),
            }],
            is_error: output.result.is_error(),
        });
        self.sort_calls[index].output = Some(output);
        self.tool_call_count += 1;
        Ok(None)
    }
}
