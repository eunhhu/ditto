use super::super::recall::{
    RecallDeriver, RecallToolOutput, RecallToolRequested, RecallToolResult,
};
use super::*;
use ditto_capability::InvocableCapabilityBinding;

impl DittoKernel {
    /// Normalize, authorize and run one `memory.search` call over the turn's
    /// searchable memories, journaling the request and the result.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn run_recall_tool(
        &self,
        scope: &TurnScope,
        cause: &mut String,
        request_index: u8,
        call: &ReadyCall,
        binding: &InvocableCapabilityBinding,
        authorizer: &InvocationAuthorizer,
        space: &[ditto_context::ContextNode],
    ) -> Result<RecallToolResult, TurnRunError> {
        let canonical = match UntrustedToolCall::new(
            call.call_id.to_string(),
            super::super::recall::ID,
            call.arguments.clone(),
        ) {
            Ok(raw) => match InvocationCompiler::compile(binding, raw, &RecallDeriver::default()) {
                Ok(invocation) => Some(invocation),
                Err(InvocationError::ArgumentsSchema { .. } | InvocationError::Deriver(_)) => None,
                Err(_) => {
                    return Err(TurnRunError::Internal(
                        "memory search invocation compilation failed",
                    ));
                }
            },
            Err(_) => None,
        };
        let query = canonical
            .as_ref()
            .map(|invocation| {
                invocation.normalized_arguments()["query"]
                    .as_str()
                    .map(str::to_owned)
                    .ok_or(TurnRunError::Internal(
                        "normalized memory search has no query",
                    ))
            })
            .transpose()?;
        let requested = self.append_turn_payload(
            scope,
            EventActor::Model,
            event_kind::AGENT_MEMORY_REQUESTED,
            &RecallToolRequested {
                event_version: 1,
                turn_id: scope.turn_id.clone(),
                request_index,
                call_id: call.call_id.clone(),
                arguments: call.arguments.clone(),
                query: query.clone(),
            },
            Some(cause.clone()),
            Some(call.call_id.to_string()),
        )?;
        *cause = requested.event_id;
        let result = match (canonical, query) {
            (Some(invocation), Some(query)) => {
                let now = Utc::now();
                match authorizer.authorize_static(&invocation, &StaticPolicy::memory_search(), now)
                {
                    Ok(AuthorizationOutcome::Permitted(permit))
                        if permit.validate(&invocation, now).is_ok() =>
                    {
                        RecallToolResult::search(&query, space)
                    }
                    _ => {
                        return Err(TurnRunError::Internal(
                            "memory search static policy contradicted its contract",
                        ));
                    }
                }
            }
            _ => RecallToolResult::InvalidArguments,
        };
        let event = self.append_turn_payload(
            scope,
            EventActor::Capability,
            event_kind::AGENT_MEMORY_OUTPUT,
            &RecallToolOutput {
                event_version: 1,
                turn_id: scope.turn_id.clone(),
                request_index,
                call_id: call.call_id.clone(),
                result: result.clone(),
            },
            Some(cause.clone()),
            Some(call.call_id.to_string()),
        )?;
        *cause = event.event_id;
        Ok(result)
    }
}
