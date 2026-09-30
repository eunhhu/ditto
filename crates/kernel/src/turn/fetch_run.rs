use super::super::fetch::{
    FetchToolError, FetchToolOutput, FetchToolRequested, FetchToolResult, FetchToolStarted,
};
use super::*;
use ditto_capability::InvocableCapabilityBinding;
use ditto_web_fetch::{self as web, FetchArguments};

impl DittoKernel {
    /// Normalize, authorize and execute one `web.fetch` call, journaling the
    /// request, the claimed start and the result.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn run_fetch_tool(
        &self,
        scope: &mut TurnScope,
        cause: &mut String,
        request_index: u8,
        call: &ReadyCall,
        binding: &InvocableCapabilityBinding,
        authorizer: &InvocationAuthorizer,
        cancellation: CancellationToken,
        deadline: DateTime<Utc>,
    ) -> Result<FetchToolResult, TurnRunError> {
        let canonical =
            match UntrustedToolCall::new(call.call_id.to_string(), web::ID, call.arguments.clone())
            {
                Ok(raw) => {
                    match InvocationCompiler::compile(binding, raw, &web::FetchDeriver::default()) {
                        Ok(invocation) => Some(invocation),
                        Err(
                            InvocationError::ArgumentsSchema { .. } | InvocationError::Deriver(_),
                        ) => None,
                        Err(_) => {
                            return Err(TurnRunError::Internal(
                                "fetch invocation compilation failed",
                            ));
                        }
                    }
                }
                Err(_) => None,
            };
        let normalized: Option<FetchArguments> = canonical
            .as_ref()
            .map(|invocation| serde_json::from_value(invocation.normalized_arguments().clone()))
            .transpose()
            .map_err(|_| TurnRunError::Internal("invalid normalized fetch arguments"))?;
        let requested = self.append_turn_payload(
            scope,
            EventActor::Model,
            event_kind::AGENT_FETCH_REQUESTED,
            &FetchToolRequested {
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
                "fetch stopped before authorization",
                Some(request_index),
                Some(call.call_id.clone()),
            ));
        }
        let mut denied = match &normalized {
            None => Some(FetchToolError::InvalidArguments),
            Some(arguments) if !scope.fetch.contains(&arguments.url) => {
                Some(FetchToolError::PermissionDenied)
            }
            Some(_) => None,
        };
        let dispatch = if denied.is_none() {
            let invocation =
                canonical.ok_or(TurnRunError::Internal("missing canonical fetch invocation"))?;
            match authorizer.authorize_with_lease(&invocation, "agent-fetch", Utc::now()) {
                Ok(AuthorizationOutcome::Permitted(permit)) => {
                    match authorizer.claim_execution(permit, &invocation, Utc::now()) {
                        Ok(claim) => Some((invocation, claim)),
                        Err(PolicyError::PermitExpired | PolicyError::EpochExpired) => {
                            denied = Some(FetchToolError::LeaseExpired);
                            None
                        }
                        Err(_) => {
                            return Err(TurnRunError::Internal("fetch execution claim failed"));
                        }
                    }
                }
                Err(PolicyError::CallBudgetExhausted) => {
                    denied = Some(FetchToolError::LeaseExhausted);
                    None
                }
                Err(PolicyError::Expired | PolicyError::EpochExpired) => {
                    denied = Some(FetchToolError::LeaseExpired);
                    None
                }
                _ => {
                    return Err(TurnRunError::Internal(
                        "fetch authorization contradicted its grant",
                    ));
                }
            }
        } else {
            None
        };
        let claimed = dispatch.is_some();
        let result = if let Some((invocation, claim)) = dispatch {
            let started = self.append_turn_payload(
                scope,
                EventActor::Capability,
                event_kind::AGENT_FETCH_STARTED,
                &FetchToolStarted {
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
                    normalized: normalized
                        .clone()
                        .ok_or(TurnRunError::Internal("missing fetch arguments"))?,
                },
                Some(cause.clone()),
                Some(call.call_id.to_string()),
            )?;
            *cause = started.event_id;
            let policy = self
                .inner
                .web_fetch
                .ok_or(TurnRunError::Internal("web fetch is disabled"))?;
            FetchToolResult::from_fetch(
                web::execute(invocation, claim, policy, cancellation.clone()).await,
            )
        } else {
            FetchToolResult::Error {
                code: denied.ok_or(TurnRunError::Internal("missing fetch denial"))?,
                status: None,
            }
        };
        // A fetch that outlived cancellation or the turn deadline reports that.
        let result = match result {
            _ if claimed && cancellation.is_cancelled() => FetchToolResult::Error {
                code: FetchToolError::Cancelled,
                status: None,
            },
            _ if claimed && deadline_expired(deadline) => FetchToolResult::Error {
                code: FetchToolError::Deadline,
                status: None,
            },
            result => result,
        };
        let event = self.append_turn_payload(
            scope,
            EventActor::Capability,
            event_kind::AGENT_FETCH_OUTPUT,
            &FetchToolOutput {
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
