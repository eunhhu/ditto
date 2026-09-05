use super::super::sort::{
    SortToolError, SortToolOutput, SortToolRequested, SortToolResult, SortToolStarted,
};
use super::*;
use ditto_artifact_sort::{self as worker, SortArguments, SortError};
use ditto_capability::InvocableCapabilityBinding;

impl DittoKernel {
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn run_sort_tool(
        &self,
        scope: &mut TurnScope,
        cause: &mut String,
        request_index: u8,
        call: &ReadyCall,
        binding: &InvocableCapabilityBinding,
        authorizer: &InvocationAuthorizer,
        cancellation: CancellationToken,
        deadline: DateTime<Utc>,
    ) -> Result<SortToolResult, TurnRunError> {
        let grant = scope
            .sort
            .as_ref()
            .ok_or(TurnRunError::Internal("missing sort permission"))?;
        let canonical = match UntrustedToolCall::new(
            call.call_id.to_string(),
            worker::ID,
            call.arguments.clone(),
        ) {
            Ok(raw) => {
                match InvocationCompiler::compile(binding, raw, &worker::SortDeriver::default()) {
                    Ok(invocation) => Some(invocation),
                    Err(InvocationError::ArgumentsSchema {
                        stage: ditto_capability::ArgumentStage::Raw,
                        ..
                    }) => None,
                    Err(_) => {
                        return Err(TurnRunError::Internal("sort invocation compilation failed"));
                    }
                }
            }
            Err(_) => None,
        };
        let normalized: Option<SortArguments> = canonical
            .as_ref()
            .map(|invocation| serde_json::from_value(invocation.normalized_arguments().clone()))
            .transpose()
            .map_err(|_| TurnRunError::Internal("invalid normalized sort arguments"))?;
        let requested = self.append_turn_payload(
            scope,
            EventActor::Model,
            event_kind::AGENT_SORT_REQUESTED,
            &SortToolRequested {
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
                "sort stopped before authorization",
                Some(request_index),
                Some(call.call_id.clone()),
            ));
        }
        let mut denied = match &normalized {
            None => Some(SortToolError::InvalidArguments),
            Some(args) if !grant.permits(args) => Some(SortToolError::PermissionDenied),
            Some(_) => None,
        };
        let dispatch = if denied.is_none() {
            let invocation =
                canonical.ok_or(TurnRunError::Internal("missing canonical sort invocation"))?;
            match authorizer.authorize_with_lease(&invocation, "agent-sort", Utc::now()) {
                Ok(AuthorizationOutcome::Permitted(permit)) => {
                    match authorizer.claim_execution(permit, &invocation, Utc::now()) {
                        Ok(claim) => Some((invocation, claim)),
                        Err(PolicyError::PermitExpired | PolicyError::EpochExpired) => {
                            denied = Some(SortToolError::LeaseExpired);
                            None
                        }
                        Err(_) => {
                            return Err(TurnRunError::Internal("sort execution claim failed"));
                        }
                    }
                }
                Err(PolicyError::CallBudgetExhausted) => {
                    denied = Some(SortToolError::LeaseExhausted);
                    None
                }
                Err(PolicyError::Expired | PolicyError::EpochExpired) => {
                    denied = Some(SortToolError::LeaseExpired);
                    None
                }
                _ => {
                    return Err(TurnRunError::Internal(
                        "sort authorization contradicted permission",
                    ));
                }
            }
        } else {
            None
        };
        let mut started_event_id = None;
        let execution = if let Some((invocation, claim)) = dispatch {
            let started = self.append_turn_payload(
                scope,
                EventActor::Capability,
                event_kind::AGENT_SORT_STARTED,
                &SortToolStarted {
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
                        .ok_or(TurnRunError::Internal("missing sort arguments"))?,
                },
                Some(cause.clone()),
                Some(call.call_id.to_string()),
            )?;
            *cause = started.event_id.clone();
            started_event_id = Some(started.event_id);
            tokio::task::yield_now().await;
            let reference = ArtifactRef::new(&grant.reference)
                .map_err(|_| TurnRunError::Internal("invalid sort reference"))?;
            match self.inner.artifacts.read_verified_range_with_object_limit(
                &reference,
                0,
                worker::MAX_INPUT_BYTES,
                worker::MAX_INPUT_BYTES as u64,
            ) {
                Ok(input) => {
                    worker::execute(invocation, claim, input.bytes(), cancellation.clone())
                        .await
                        .map_err(|error| match error {
                            SortError::Cancelled => SortToolError::Cancelled,
                            SortError::Deadline => SortToolError::ProcessDeadline,
                            SortError::Verification => SortToolError::VerificationFailed,
                            _ => SortToolError::ProcessFailed,
                        })
                }
                Err(_) => Err(SortToolError::InputUnavailable),
            }
        } else {
            Err(denied.ok_or(TurnRunError::Internal("missing sort denial"))?)
        };

        // No await under the admission/cancellation gate. A cancelled result
        // cannot publish a new verified artifact after cancellation wins.
        let _gate = self
            .inner
            .agent_runs
            .lock()
            .map_err(|_| TurnRunError::Internal("run slot is unavailable"))?;
        let claimed = started_event_id.is_some();
        let execution = if claimed && cancellation.is_cancelled() {
            Err(SortToolError::Cancelled)
        } else if claimed && deadline_expired(deadline) {
            Err(SortToolError::ProcessDeadline)
        } else {
            execution
        };
        let (result, artifact_event_id) = match execution {
            Ok(output) => {
                let stored = self.store_artifact(
                    output.bytes(),
                    crate::ArtifactWriteContext {
                        session_id: Some(scope.session_id.clone()),
                        task_id: Some(scope.task_id.clone()),
                        producer_event_id: started_event_id,
                        mime: Some("text/plain; charset=utf-8".into()),
                        purpose: Some("verified model-directed sort output".into()),
                    },
                )?;
                (
                    SortToolResult::Verified {
                        reference: stored.metadata.reference.to_string(),
                        verifier: worker::VERIFIER.into(),
                        input_lines: output.input_lines(),
                        output_lines: output.output_lines(),
                    },
                    Some(stored.event.event_id),
                )
            }
            Err(code) => (SortToolResult::Error { code }, None),
        };
        let event = self.append_turn_payload(
            scope,
            EventActor::Capability,
            event_kind::AGENT_SORT_OUTPUT,
            &SortToolOutput {
                event_version: 1,
                turn_id: scope.turn_id.clone(),
                request_index,
                call_id: call.call_id.clone(),
                claimed,
                result: result.clone(),
                artifact_event_id,
            },
            Some(cause.clone()),
            Some(call.call_id.to_string()),
        )?;
        *cause = event.event_id;
        Ok(result)
    }
}
