//! Running agent tool calls on their shared lifecycle (ADR 0036): journal
//! `tool.requested`, give cancellation a checkpoint, claim a leased call and
//! journal `tool.started`, then journal `tool.output`.
use super::super::memory as memory_tool;
use super::super::memory::{MemoryAction, MemoryDeriver, MemoryRefusal, MemoryResult};
use super::super::sort::{SortToolError, SortToolResult};
use super::super::tool::{LeaseRefusal, STOPPED, ToolOutput, ToolRequested, ToolStarted};
use super::super::web::{self, WebResult, WebToolError};
use super::*;
use crate::TrustedContextNodeDraft;
use ditto_artifact_sort::{self as worker, SortArguments, SortError};
use ditto_capability::{CanonicalInvocation, InvocableCapabilityBinding};
use ditto_policy::ExecutionClaim;
use ditto_web_fetch::{WebAccess, WebRequest};

/// A model call compiled against its binding; `None` when its arguments break
/// the schema or the tool's normalization, which the tool answers as invalid.
fn compile_call(
    binding: &InvocableCapabilityBinding,
    call: &ReadyCall,
    deriver: &dyn CapabilityDeriver,
) -> Result<Option<CanonicalInvocation>, TurnRunError> {
    let Ok(raw) = UntrustedToolCall::new(
        call.call_id.to_string(),
        call.capability_id.clone(),
        call.arguments.clone(),
    ) else {
        return Ok(None);
    };
    match InvocationCompiler::compile(binding, raw, deriver) {
        Ok(invocation) => Ok(Some(invocation)),
        Err(InvocationError::ArgumentsSchema { .. } | InvocationError::Deriver(_)) => Ok(None),
        Err(_) => Err(TurnRunError::Internal("tool invocation compilation failed")),
    }
}

/// The normalized arguments of a compiled call, typed.
fn normalized<T: serde::de::DeserializeOwned>(
    invocation: Option<&CanonicalInvocation>,
) -> Result<Option<T>, TurnRunError> {
    invocation
        .map(|invocation| serde_json::from_value(invocation.normalized_arguments().clone()))
        .transpose()
        .map_err(|_| TurnRunError::Internal("normalized tool arguments are untyped"))
}

/// Authorize `invocation` under `lease` and claim its one execution; an
/// exhausted or expired lease is a refusal the model reads.
fn claim(
    authorizer: &InvocationAuthorizer,
    invocation: &CanonicalInvocation,
    lease: &str,
) -> Result<Result<ExecutionClaim, LeaseRefusal>, TurnRunError> {
    match authorizer.authorize_with_lease(invocation, lease, Utc::now()) {
        Ok(AuthorizationOutcome::Permitted(permit)) => {
            match authorizer.claim_execution(permit, invocation, Utc::now()) {
                Ok(claim) => Ok(Ok(claim)),
                Err(PolicyError::PermitExpired | PolicyError::EpochExpired) => {
                    Ok(Err(LeaseRefusal::Expired))
                }
                Err(_) => Err(TurnRunError::Internal("tool execution claim failed")),
            }
        }
        Err(PolicyError::CallBudgetExhausted) => Ok(Err(LeaseRefusal::Exhausted)),
        Err(PolicyError::Expired | PolicyError::EpochExpired) => Ok(Err(LeaseRefusal::Expired)),
        _ => Err(TurnRunError::Internal(
            "tool authorization contradicted its lease",
        )),
    }
}

impl DittoKernel {
    /// What this deployment lets `web.browse` do.
    pub(super) fn web_access(&self) -> WebAccess {
        WebAccess {
            read: self.inner.web_fetch,
            search: self.inner.web_search.clone(),
        }
    }

    fn tool_requested(
        &self,
        scope: &TurnScope,
        cause: &mut String,
        request_index: u8,
        call: &ReadyCall,
        invocation: Option<&CanonicalInvocation>,
    ) -> Result<EventRecord, TurnRunError> {
        let event = self.append_turn_payload(
            scope,
            EventActor::Model,
            event_kind::TOOL_REQUESTED,
            &ToolRequested {
                event_version: 1,
                turn_id: scope.turn_id.clone(),
                request_index,
                call_id: call.call_id.clone(),
                capability_id: call.capability_id.clone(),
                arguments: call.arguments.clone(),
                normalized: invocation.map(|invocation| invocation.normalized_arguments().clone()),
            },
            Some(cause.clone()),
            Some(call.call_id.to_string()),
        )?;
        *cause = event.event_id.clone();
        Ok(event)
    }

    /// Give cancellation a checkpoint, then stop the turn with a journaled
    /// failure if it was cancelled or is out of time.
    async fn tool_checkpoint(
        &self,
        scope: &mut TurnScope,
        cause: &str,
        cancellation: &CancellationToken,
        deadline: DateTime<Utc>,
        request_index: u8,
        call: &ReadyCall,
    ) -> Result<(), TurnRunError> {
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
                STOPPED,
                Some(request_index),
                Some(call.call_id.clone()),
            ));
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn tool_started(
        &self,
        scope: &TurnScope,
        cause: &mut String,
        request_index: u8,
        call: &ReadyCall,
        invocation: &CanonicalInvocation,
        claim: &ExecutionClaim,
        request_url: Option<String>,
    ) -> Result<EventRecord, TurnRunError> {
        let event = self.append_turn_payload(
            scope,
            EventActor::Capability,
            event_kind::TOOL_STARTED,
            &ToolStarted {
                event_version: 1,
                turn_id: scope.turn_id.clone(),
                request_index,
                call_id: call.call_id.clone(),
                capability_id: call.capability_id.clone(),
                epoch_id: invocation.epoch_id().into(),
                invocation_digest: invocation.digest().to_string(),
                claim_id: claim.claim_id().into(),
                permit_id: claim.permit_id().as_str().into(),
                claimed_at: claim.claimed_at(),
                expires_at: claim.expires_at(),
                request_url,
            },
            Some(cause.clone()),
            Some(call.call_id.to_string()),
        )?;
        *cause = event.event_id.clone();
        Ok(event)
    }

    #[allow(clippy::too_many_arguments)]
    fn tool_output(
        &self,
        scope: &TurnScope,
        cause: &mut String,
        request_index: u8,
        call: &ReadyCall,
        claimed: bool,
        result: &impl Serialize,
        artifact_event_id: Option<String>,
    ) -> Result<(), TurnRunError> {
        let event = self.append_turn_payload(
            scope,
            EventActor::Capability,
            event_kind::TOOL_OUTPUT,
            &ToolOutput {
                event_version: 1,
                turn_id: scope.turn_id.clone(),
                request_index,
                call_id: call.call_id.clone(),
                capability_id: call.capability_id.clone(),
                claimed,
                result: serde_json::to_value(result)?,
                artifact_event_id,
            },
            Some(cause.clone()),
            Some(call.call_id.to_string()),
        )?;
        *cause = event.event_id;
        Ok(())
    }

    /// One `web.browse` call: read a page the user linked, or search at the
    /// operator's endpoint, each under its own lease.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn run_web_call(
        &self,
        scope: &mut TurnScope,
        cause: &mut String,
        request_index: u8,
        call: &ReadyCall,
        binding: &InvocableCapabilityBinding,
        authorizer: &InvocationAuthorizer,
        cancellation: CancellationToken,
        deadline: DateTime<Utc>,
    ) -> Result<WebResult, TurnRunError> {
        let access = self.web_access();
        let invocation = compile_call(binding, call, &access.deriver())?;
        let request: Option<WebRequest> = normalized(invocation.as_ref())?;
        self.tool_requested(scope, cause, request_index, call, invocation.as_ref())?;
        self.tool_checkpoint(scope, cause, &cancellation, deadline, request_index, call)
            .await?;
        let claimed = match (
            web::refusal(request.as_ref(), &scope.web_grant),
            invocation,
            request,
        ) {
            (Some(code), ..) => Err(code),
            (None, Some(invocation), Some(request)) => {
                match claim(
                    authorizer,
                    &invocation,
                    web::lease(&request, &scope.web_grant).0,
                )? {
                    Ok(claim) => Ok((invocation, claim, request)),
                    Err(LeaseRefusal::Exhausted) => Err(WebToolError::LeaseExhausted),
                    Err(LeaseRefusal::Expired) => Err(WebToolError::LeaseExpired),
                }
            }
            _ => return Err(TurnRunError::Internal("allowed web call has no invocation")),
        };
        let (started, result) = match claimed {
            Err(code) => (false, WebResult::error(code)),
            Ok((invocation, claim, request)) => {
                let request_url = match (&request, &access.search) {
                    (WebRequest::Search { query }, Some(endpoint)) => {
                        Some(endpoint.request_url(query))
                    }
                    _ => None,
                };
                self.tool_started(
                    scope,
                    cause,
                    request_index,
                    call,
                    &invocation,
                    &claim,
                    request_url,
                )?;
                let result = WebResult::from_output(
                    ditto_web_fetch::execute(invocation, claim, &access, cancellation.clone())
                        .await,
                );
                // A call that outlived cancellation or the turn deadline
                // reports that.
                let result = if cancellation.is_cancelled() {
                    WebResult::error(WebToolError::Cancelled)
                } else if deadline_expired(deadline) {
                    WebResult::error(WebToolError::Deadline)
                } else {
                    result
                };
                (true, result)
            }
        };
        self.tool_output(scope, cause, request_index, call, started, &result, None)?;
        Ok(result)
    }

    /// One `memory.manage` call: a search over the turn's searchable
    /// memories, or a write checked and recorded under the context admission
    /// gate.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn run_memory_call(
        &self,
        scope: &mut TurnScope,
        cause: &mut String,
        request_index: u8,
        call: &ReadyCall,
        binding: &InvocableCapabilityBinding,
        authorizer: &InvocationAuthorizer,
        space: &[ditto_context::ContextNode],
        read_external_content: bool,
        writes: &mut u32,
        cancellation: CancellationToken,
        deadline: DateTime<Utc>,
    ) -> Result<MemoryResult, TurnRunError> {
        let invocation = compile_call(binding, call, &MemoryDeriver::default())?;
        let action: Option<MemoryAction> = normalized(invocation.as_ref())?;
        let requested =
            self.tool_requested(scope, cause, request_index, call, invocation.as_ref())?;
        self.tool_checkpoint(scope, cause, &cancellation, deadline, request_index, call)
            .await?;
        let result = match (invocation, action) {
            (Some(invocation), Some(MemoryAction::Search { query })) => {
                let now = Utc::now();
                match authorizer.authorize_static(&invocation, &StaticPolicy::memory_search(), now)
                {
                    Ok(AuthorizationOutcome::Permitted(permit))
                        if permit.validate(&invocation, now).is_ok() =>
                    {
                        MemoryResult::search(&query, space)
                    }
                    _ => {
                        return Err(TurnRunError::Internal(
                            "memory search static policy contradicted its contract",
                        ));
                    }
                }
            }
            (Some(invocation), Some(action)) => self.commit_memory_write(
                scope,
                &requested,
                call,
                &invocation,
                &action,
                authorizer,
                read_external_content,
                writes,
            )?,
            _ => MemoryResult::Refused {
                code: MemoryRefusal::InvalidArguments,
            },
        };
        self.tool_output(scope, cause, request_index, call, false, &result, None)?;
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
        action: &MemoryAction,
        authorizer: &InvocationAuthorizer,
        read_external_content: bool,
        writes: &mut u32,
    ) -> Result<MemoryResult, TurnRunError> {
        let write = action
            .write()
            .ok_or(TurnRunError::Internal("a search is not a memory write"))?;
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
        let target_active = match write.target() {
            Some(target) => self
                .memory_snapshot_locked(session, high_water)?
                .candidates()
                .iter()
                .any(|node| node.id == target && memory_tool::is_memory(node)),
            None => true,
        };
        if let Some(code) =
            memory_tool::refusal(Some(action), read_external_content, *writes, |_| {
                target_active
            })
        {
            return Ok(MemoryResult::Refused { code });
        }
        if claim(authorizer, invocation, memory_tool::LEASE_ID)?.is_err() {
            return Err(TurnRunError::Internal(
                "memory write lease contradicted its contract",
            ));
        }
        let written = self.append_and_publish(NewEvent {
            session_id: Some(session.to_owned()),
            task_id: None,
            actor: EventActor::Model,
            kind: event_kind::MEMORY_WRITTEN.to_owned(),
            payload: serde_json::to_value(memory_tool::MemoryWrittenPayload {
                event_version: 1,
                turn_id: scope.turn_id.clone(),
                call_id: call.call_id.clone(),
                write: write.clone(),
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
        let node = memory_tool::memory_node(&written.event_id, &write);
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
        Ok(MemoryResult::written(&write, node.id))
    }

    /// One `artifact.sort` call on the attachment the user permitted: the
    /// closed process profile, its output verified and stored.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn run_sort_call(
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
            .clone()
            .ok_or(TurnRunError::Internal("missing sort permission"))?;
        let invocation = compile_call(binding, call, &worker::SortDeriver::default())?;
        let arguments: Option<SortArguments> = normalized(invocation.as_ref())?;
        self.tool_requested(scope, cause, request_index, call, invocation.as_ref())?;
        self.tool_checkpoint(scope, cause, &cancellation, deadline, request_index, call)
            .await?;
        let claimed = match (invocation, arguments) {
            (Some(invocation), Some(arguments)) if grant.permits(&arguments) => {
                match claim(authorizer, &invocation, "agent-sort")? {
                    Ok(claim) => Ok((invocation, claim)),
                    Err(LeaseRefusal::Exhausted) => Err(SortToolError::LeaseExhausted),
                    Err(LeaseRefusal::Expired) => Err(SortToolError::LeaseExpired),
                }
            }
            (Some(_), Some(_)) => Err(SortToolError::PermissionDenied),
            _ => Err(SortToolError::InvalidArguments),
        };
        let mut started_event_id = None;
        let execution = match claimed {
            Ok((invocation, claim)) => {
                let started = self.tool_started(
                    scope,
                    cause,
                    request_index,
                    call,
                    &invocation,
                    &claim,
                    None,
                )?;
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
            }
            Err(code) => Err(code),
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
        self.tool_output(
            scope,
            cause,
            request_index,
            call,
            claimed,
            &result,
            artifact_event_id,
        )?;
        Ok(result)
    }
}
