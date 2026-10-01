use super::super::search::{
    LEASE_ID, SearchToolError, SearchToolOutput, SearchToolRequested, SearchToolResult,
    SearchToolStarted, refusal,
};
use super::*;
use ditto_capability::InvocableCapabilityBinding;
use ditto_web_fetch::search::{self as web, SearchArguments, SearchDeriver};

impl DittoKernel {
    /// Normalize, authorize and run one `web.search` call at the operator's
    /// endpoint (ADR 0033), journaling the request, the claimed start and the
    /// result.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn run_search_tool(
        &self,
        scope: &mut TurnScope,
        cause: &mut String,
        request_index: u8,
        call: &ReadyCall,
        binding: &InvocableCapabilityBinding,
        authorizer: &InvocationAuthorizer,
        cancellation: CancellationToken,
        deadline: DateTime<Utc>,
    ) -> Result<SearchToolResult, TurnRunError> {
        let endpoint = self
            .inner
            .web_search
            .clone()
            .ok_or(TurnRunError::Internal("web search is not configured"))?;
        let canonical =
            match UntrustedToolCall::new(call.call_id.to_string(), web::ID, call.arguments.clone())
            {
                Ok(raw) => match InvocationCompiler::compile(
                    binding,
                    raw,
                    &SearchDeriver::new(endpoint.clone()),
                ) {
                    Ok(invocation) => Some(invocation),
                    Err(InvocationError::ArgumentsSchema { .. } | InvocationError::Deriver(_)) => {
                        None
                    }
                    Err(_) => {
                        return Err(TurnRunError::Internal(
                            "search invocation compilation failed",
                        ));
                    }
                },
                Err(_) => None,
            };
        let normalized: Option<SearchArguments> = canonical
            .as_ref()
            .map(|invocation| serde_json::from_value(invocation.normalized_arguments().clone()))
            .transpose()
            .map_err(|_| TurnRunError::Internal("invalid normalized search arguments"))?;
        let requested = self.append_turn_payload(
            scope,
            EventActor::Model,
            event_kind::AGENT_SEARCH_REQUESTED,
            &SearchToolRequested {
                event_version: 1,
                turn_id: scope.turn_id.clone(),
                request_index,
                call_id: call.call_id.clone(),
                arguments: call.arguments.clone(),
                normalized: normalized.clone(),
            },
            Some(cause.clone()),
            Some(call.call_id.to_string()),
        )?;
        *cause = requested.event_id;
        tokio::task::yield_now().await;
        if cancellation.is_cancelled() || deadline_expired(deadline) {
            return Err(self.persist_turn_failure(
                scope,
                cause,
                if cancellation.is_cancelled() {
                    TurnFailureCode::Cancelled
                } else {
                    TurnFailureCode::DeadlineExceeded
                },
                "search stopped before authorization",
                Some(request_index),
                Some(call.call_id.clone()),
            ));
        }
        let mut denied = refusal(normalized.as_ref());
        let dispatch = if denied.is_none() {
            let invocation = canonical.ok_or(TurnRunError::Internal(
                "missing canonical search invocation",
            ))?;
            match authorizer.authorize_with_lease(&invocation, LEASE_ID, Utc::now()) {
                Ok(AuthorizationOutcome::Permitted(permit)) => {
                    match authorizer.claim_execution(permit, &invocation, Utc::now()) {
                        Ok(claim) => Some((invocation, claim)),
                        Err(PolicyError::PermitExpired | PolicyError::EpochExpired) => {
                            denied = Some(SearchToolError::LeaseExpired);
                            None
                        }
                        Err(_) => {
                            return Err(TurnRunError::Internal("search execution claim failed"));
                        }
                    }
                }
                Err(PolicyError::CallBudgetExhausted) => {
                    denied = Some(SearchToolError::LeaseExhausted);
                    None
                }
                Err(PolicyError::Expired | PolicyError::EpochExpired) => {
                    denied = Some(SearchToolError::LeaseExpired);
                    None
                }
                _ => {
                    return Err(TurnRunError::Internal(
                        "search authorization contradicted its lease",
                    ));
                }
            }
        } else {
            None
        };
        let claimed = dispatch.is_some();
        let result = if let Some((invocation, claim)) = dispatch {
            let arguments = normalized
                .clone()
                .ok_or(TurnRunError::Internal("missing search arguments"))?;
            let request_url = endpoint
                .request_url(&arguments.query)
                .map_err(|_| TurnRunError::Internal("search request URL is invalid"))?;
            let started = self.append_turn_payload(
                scope,
                EventActor::Capability,
                event_kind::AGENT_SEARCH_STARTED,
                &SearchToolStarted {
                    event_version: 1,
                    turn_id: scope.turn_id.clone(),
                    request_index,
                    call_id: call.call_id.clone(),
                    epoch_id: invocation.epoch_id().into(),
                    invocation_digest: invocation.digest().to_string(),
                    claim_id: claim.claim_id().into(),
                    permit_id: claim.permit_id().as_str().into(),
                    claimed_at: claim.claimed_at(),
                    expires_at: claim.expires_at(),
                    normalized: arguments,
                    request_url,
                },
                Some(cause.clone()),
                Some(call.call_id.to_string()),
            )?;
            *cause = started.event_id;
            SearchToolResult::from_search(
                web::execute(invocation, claim, &endpoint, cancellation.clone()).await,
            )
        } else {
            SearchToolResult::Error {
                code: denied.ok_or(TurnRunError::Internal("missing search denial"))?,
                status: None,
            }
        };
        // A search that outlived cancellation or the turn deadline reports that.
        let result = match result {
            _ if claimed && cancellation.is_cancelled() => SearchToolResult::Error {
                code: SearchToolError::Cancelled,
                status: None,
            },
            _ if claimed && deadline_expired(deadline) => SearchToolResult::Error {
                code: SearchToolError::Deadline,
                status: None,
            },
            result => result,
        };
        let event = self.append_turn_payload(
            scope,
            EventActor::Capability,
            event_kind::AGENT_SEARCH_OUTPUT,
            &SearchToolOutput {
                event_version: 1,
                turn_id: scope.turn_id.clone(),
                request_index,
                call_id: call.call_id.clone(),
                claimed,
                result: result.clone(),
            },
            Some(cause.clone()),
            Some(call.call_id.to_string()),
        )?;
        *cause = event.event_id;
        Ok(result)
    }
}
