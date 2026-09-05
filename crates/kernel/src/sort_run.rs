use std::collections::BTreeSet;

use chrono::{Duration, Utc};
use ditto_artifact_sort::{self as sort, SortError};
use ditto_capability::{
    CanonicalResource, CapabilityDeriver, InvocationCompiler, LiveExecutionEpoch, UntrustedToolCall,
};
use ditto_model::CancellationToken;
use ditto_policy::{
    ApprovalRequirement, AuthorizationOutcome, CapabilityLease, InvocationAuthorizer, ResourceScope,
};
use ditto_protocol::{
    AgentRunQuery, EventActor, EventRecord, NewEvent, SortRunResponse, SortRunStatus,
    StartSortCommand, event_kind,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use ulid::Ulid;

use crate::{
    AgentRunError, ArtifactRef, ArtifactWriteContext, DittoKernel,
    agent_run::{ActiveGuard, ActiveRun, RunSlot, validate_query},
};

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SortInput {
    version: u16,
    request_id: String,
    reference: String,
    unique: bool,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SortCompletion {
    version: u16,
    capability_id: String,
    verifier: String,
    input_reference: String,
    output_reference: String,
    unique: bool,
    input_lines: usize,
    output_lines: usize,
    exit_code: i32,
    started_event_id: String,
    invocation_digest: String,
    claim_id: String,
}

impl DittoKernel {
    /// Explicit authorization for the closed sort profile; never calls a model.
    pub fn start_sort(&self, command: StartSortCommand) -> Result<SortRunResponse, AgentRunError> {
        let query = AgentRunQuery {
            request_id: command.request_id,
            session_id: command.session_id,
        };
        let task = sort_task(&query)?;
        sort::validate_input(command.text.as_bytes()).map_err(|_| {
            AgentRunError::Invalid("sort accepts at most 64 KiB / 4096 UTF-8 lines without NUL")
        })?;
        let reference = sort::input_reference(command.text.as_bytes());
        let runtime = tokio::runtime::Handle::try_current().map_err(|_| AgentRunError::Stopping)?;
        let mut slot = self
            .inner
            .agent_runs
            .lock()
            .map_err(|_| AgentRunError::Storage)?;
        if let Some((input, last)) = self.sort_boundary(&query, &task)? {
            let stored = validate_input_event(&input, &query)?;
            if stored.reference != reference || stored.unique != command.unique {
                return Err(AgentRunError::Conflict);
            }
            return self.sort_status(query, input, last, &slot);
        }
        if slot.stopping {
            return Err(AgentRunError::Stopping);
        }
        if slot.active.is_some() {
            return Err(AgentRunError::Busy);
        }
        // Page only the selected implementation contract; malformed installation
        // fails before accepting work or minting execution authority.
        let manifest = self
            .inner
            .capabilities
            .page_manifest(sort::ID)
            .map_err(|_| AgentRunError::Storage)?
            .ok_or(AgentRunError::Invalid("artifact.sort is not installed"))?;
        sort::validate_manifest(&manifest).map_err(|_| {
            AgentRunError::Invalid(
                "artifact.sort package does not match the supported implementation",
            )
        })?;
        let stored = self.store_artifact(
            command.text.as_bytes(),
            ArtifactWriteContext {
                session_id: Some(query.session_id.clone()),
                task_id: Some(task.clone()),
                mime: Some("text/plain; charset=utf-8".into()),
                purpose: Some("sort input".into()),
                ..Default::default()
            },
        )?;
        let input = self.append_and_publish(NewEvent {
            session_id: Some(query.session_id.clone()),
            task_id: Some(task),
            actor: EventActor::User,
            kind: event_kind::SORT_REQUESTED.into(),
            payload: json!(SortInput {
                version: 1,
                request_id: query.request_id.clone(),
                reference,
                unique: command.unique
            }),
            causation_id: Some(stored.event.event_id),
            correlation_id: Some(format!("turn_{}", Ulid::new())),
            span_id: None,
        })?;
        let cancellation = CancellationToken::new();
        let finished = CancellationToken::new();
        slot.active = Some(ActiveRun {
            input_event_id: input.event_id.clone(),
            cancellation: cancellation.clone(),
            finished: finished.clone(),
        });
        let guard = ActiveGuard {
            kernel: self.clone(),
            input_event_id: input.event_id.clone(),
            finished,
        };
        let kernel = self.clone();
        let accepted = input.clone();
        drop(runtime.spawn(async move {
            let _guard = guard;
            if let Err(code) = kernel
                .execute_sort(&accepted, &command.text, &manifest, cancellation)
                .await
            {
                // Storage failure can still leave interrupted state. Never synthesize success.
                let _ = kernel.append_sort_event(
                    &accepted,
                    EventActor::System,
                    event_kind::SORT_FAILED,
                    json!({"version":1,"code":code}),
                    accepted.event_id.clone(),
                );
            }
        }));
        self.sort_status(query, input.clone(), input, &slot)
    }

    pub fn inspect_sort(&self, query: AgentRunQuery) -> Result<SortRunResponse, AgentRunError> {
        self.inspect_or_cancel_sort(query, false)
    }

    pub fn cancel_sort(&self, query: AgentRunQuery) -> Result<SortRunResponse, AgentRunError> {
        self.inspect_or_cancel_sort(query, true)
    }

    fn inspect_or_cancel_sort(
        &self,
        query: AgentRunQuery,
        cancel: bool,
    ) -> Result<SortRunResponse, AgentRunError> {
        let task = sort_task(&query)?;
        let slot = self
            .inner
            .agent_runs
            .lock()
            .map_err(|_| AgentRunError::Storage)?;
        let (input, last) = self
            .sort_boundary(&query, &task)?
            .ok_or(AgentRunError::NotFound)?;
        validate_input_event(&input, &query)?;
        if cancel
            && let Some(active) = &slot.active
            && active.input_event_id == input.event_id
        {
            active.cancellation.cancel();
        }
        self.sort_status(query, input, last, &slot)
    }

    fn sort_boundary(
        &self,
        query: &AgentRunQuery,
        task: &str,
    ) -> Result<Option<(EventRecord, EventRecord)>, AgentRunError> {
        self.inner
            .events
            .task_turn_boundary(&query.session_id, task)
            .map_err(|_| AgentRunError::Storage)
    }

    async fn execute_sort(
        &self,
        input: &EventRecord,
        text: &str,
        manifest: &ditto_capability::CapabilityManifest,
        cancellation: CancellationToken,
    ) -> Result<(), SortError> {
        let args: SortInput =
            serde_json::from_value(input.payload.clone()).map_err(|_| SortError::Authority)?;
        if cancellation.is_cancelled() {
            return Err(SortError::Cancelled);
        }
        let deriver = sort::SortDeriver::default();
        let mut epoch = LiveExecutionEpoch::new(1);
        epoch
            .page_in_invocable(manifest, &sort::schema(), deriver.revision().clone())
            .map_err(|_| SortError::Authority)?;
        let ticket = epoch
            .seal_for_authorization()
            .map_err(|_| SortError::Authority)?;
        let binding = epoch
            .invocable_binding(sort::ID)
            .ok_or(SortError::Authority)?;
        let call = UntrustedToolCall::new(
            &input.event_id,
            sort::ID,
            json!({"reference": args.reference, "unique": args.unique}),
        )
        .map_err(|_| SortError::Authority)?;
        let invocation = InvocationCompiler::compile(binding, call, &deriver)
            .map_err(|_| SortError::Authority)?;
        let expires = input.recorded_at + Duration::seconds(30);
        let authorizer =
            InvocationAuthorizer::from_ticket(ticket, expires).map_err(|_| SortError::Authority)?;
        let lease_id = format!("sort_{}", input.event_id);
        authorizer
            .register_lease(
                CapabilityLease::new(
                    &lease_id,
                    expires,
                    sort::effect(),
                    1,
                    BTreeSet::from([sort::ID.into()]),
                    vec![ResourceScope::Exact(
                        CanonicalResource::artifact(&args.reference)
                            .map_err(|_| SortError::Authority)?,
                    )],
                    ApprovalRequirement::Never,
                )
                .map_err(|_| SortError::Authority)?,
            )
            .map_err(|_| SortError::Authority)?;
        let AuthorizationOutcome::Permitted(permit) = authorizer
            .authorize_with_lease(&invocation, &lease_id, Utc::now())
            .map_err(|_| SortError::Authority)?
        else {
            return Err(SortError::Authority);
        };
        let claim = authorizer
            .claim_execution(permit, &invocation, Utc::now())
            .map_err(|_| SortError::Authority)?;
        let digest = invocation.digest().to_string();
        let claim_id = claim.claim_id().to_owned();
        let started = self.append_sort_event(input, EventActor::System, event_kind::SORT_STARTED,
            json!({"version":1,"capability_id":sort::ID,"invocation_digest":digest,"claim_id":claim_id,
                "lease_id":lease_id,"expires_at":expires,"effect":invocation.effect(),
                "resources":invocation.resources(),"revision":invocation.capability_revision(),"epoch":epoch.evidence(),
                "input_reference":args.reference,"unique":args.unique}), input.event_id.clone()).map_err(|_| SortError::Unavailable)?;
        let output =
            sort::execute(invocation, claim, text.as_bytes(), cancellation.clone()).await?;
        // Completion wins against later cancellation only after taking the same
        // gate used by cancel/inspect/admit. No await while holding the gate.
        let _gate = self
            .inner
            .agent_runs
            .lock()
            .map_err(|_| SortError::Unavailable)?;
        if cancellation.is_cancelled() {
            return Err(SortError::Cancelled);
        }
        if Utc::now() >= expires {
            return Err(SortError::Deadline);
        }
        let stored = self
            .store_artifact(
                output.bytes(),
                ArtifactWriteContext {
                    session_id: input.session_id.clone(),
                    task_id: input.task_id.clone(),
                    producer_event_id: Some(started.event_id.clone()),
                    mime: Some("text/plain; charset=utf-8".into()),
                    purpose: Some("verified sort output".into()),
                },
            )
            .map_err(|_| SortError::Unavailable)?;
        self.append_sort_event(
            input,
            EventActor::System,
            event_kind::TASK_COMPLETED,
            json!(SortCompletion {
                version: 1,
                capability_id: sort::ID.into(),
                verifier: sort::VERIFIER.into(),
                input_reference: args.reference,
                output_reference: stored.metadata.reference.to_string(),
                unique: args.unique,
                input_lines: output.input_lines(),
                output_lines: output.output_lines(),
                exit_code: 0,
                started_event_id: started.event_id,
                invocation_digest: digest,
                claim_id
            }),
            stored.event.event_id,
        )
        .map_err(|_| SortError::Unavailable)?;
        Ok(())
    }

    fn append_sort_event(
        &self,
        input: &EventRecord,
        actor: EventActor,
        kind: &str,
        payload: Value,
        cause: String,
    ) -> Result<EventRecord, crate::KernelError> {
        self.append_and_publish(NewEvent {
            session_id: input.session_id.clone(),
            task_id: input.task_id.clone(),
            actor,
            kind: kind.into(),
            payload,
            causation_id: Some(cause),
            correlation_id: input.correlation_id.clone(),
            span_id: None,
        })
    }

    fn sort_status(
        &self,
        query: AgentRunQuery,
        input: EventRecord,
        last: EventRecord,
        slot: &RunSlot,
    ) -> Result<SortRunResponse, AgentRunError> {
        let source = validate_input_event(&input, &query)?;
        let active = slot
            .active
            .as_ref()
            .filter(|active| active.input_event_id == input.event_id);
        let mut response = SortRunResponse {
            request_id: query.request_id,
            session_id: query.session_id,
            task_id: input.task_id.clone().ok_or(AgentRunError::Storage)?,
            turn_id: input.correlation_id.clone().ok_or(AgentRunError::Storage)?,
            status: if active.is_some() {
                SortRunStatus::Running
            } else {
                SortRunStatus::Interrupted
            },
            cancellation_requested: active.is_some_and(|active| active.cancellation.is_cancelled()),
            output_reference: None,
            output: None,
            failure_code: None,
        };
        if last.kind == event_kind::TASK_COMPLETED {
            let result: SortCompletion =
                serde_json::from_value(last.payload.clone()).map_err(|_| AgentRunError::Storage)?;
            if !same_scope(&input, &last)
                || last.actor != EventActor::System
                || result.version != 1
                || result.capability_id != sort::ID
                || result.verifier != sort::VERIFIER
                || result.exit_code != 0
                || result.input_reference != source.reference
                || result.unique != source.unique
            {
                return Err(AgentRunError::Storage);
            }
            let started = self
                .inner
                .events
                .get_by_event_id(&result.started_event_id)
                .map_err(|_| AgentRunError::Storage)?
                .ok_or(AgentRunError::Storage)?;
            if !same_scope(&input, &started)
                || started.actor != EventActor::System
                || started.kind != event_kind::SORT_STARTED
                || started.seq <= input.seq
                || started.seq >= last.seq
                || started.causation_id.as_deref() != Some(&input.event_id)
                || started.payload["version"] != 1
                || started.payload["capability_id"] != sort::ID
                || started.payload["invocation_digest"] != result.invocation_digest
                || started.payload["claim_id"] != result.claim_id
                || started.payload["input_reference"] != source.reference
                || started.payload["unique"] != source.unique
            {
                return Err(AgentRunError::Storage);
            }
            self.verify_sort_artifact_root(&input, &source.reference, None, None)?;
            self.verify_sort_artifact_root(
                &last,
                &result.output_reference,
                Some(&started.event_id),
                Some(started.seq),
            )?;
            let bytes = self.sort_artifact_bytes(&source.reference, sort::MAX_INPUT_BYTES)?;
            let output =
                self.sort_artifact_bytes(&result.output_reference, sort::MAX_OUTPUT_BYTES)?;
            let verified = sort::verify_output(&bytes, output, source.unique)
                .map_err(|_| AgentRunError::Storage)?;
            if verified.input_lines() != result.input_lines
                || verified.output_lines() != result.output_lines
            {
                return Err(AgentRunError::Storage);
            }
            response.output = Some(
                String::from_utf8(verified.bytes().to_vec()).map_err(|_| AgentRunError::Storage)?,
            );
            response.output_reference = Some(result.output_reference);
            response.status = SortRunStatus::Verified;
            response.cancellation_requested = false;
        } else if last.kind == event_kind::SORT_FAILED {
            if !same_scope(&input, &last)
                || last.actor != EventActor::System
                || last.payload["version"] != 1
            {
                return Err(AgentRunError::Storage);
            }
            let code: SortError = serde_json::from_value(last.payload["code"].clone())
                .map_err(|_| AgentRunError::Storage)?;
            response.failure_code = serde_json::to_value(code)
                .ok()
                .and_then(|v| v.as_str().map(str::to_owned));
            response.status = SortRunStatus::Failed;
            response.cancellation_requested = false;
        }
        Ok(response)
    }

    pub(crate) fn sort_artifact_bytes(
        &self,
        reference: &str,
        limit: usize,
    ) -> Result<Vec<u8>, AgentRunError> {
        let reference = ArtifactRef::new(reference).map_err(|_| AgentRunError::Storage)?;
        let read = self
            .inner
            .artifacts
            .read_verified_range_with_object_limit(&reference, 0, limit, limit as u64)
            .map_err(|_| AgentRunError::Storage)?;
        if read.total_bytes() > limit as u64 {
            return Err(AgentRunError::Storage);
        }
        Ok(read.bytes().to_vec())
    }

    fn verify_sort_artifact_root(
        &self,
        consumer: &EventRecord,
        reference: &str,
        producer: Option<&str>,
        after_seq: Option<i64>,
    ) -> Result<(), AgentRunError> {
        let root = self
            .inner
            .events
            .get_by_event_id(
                consumer
                    .causation_id
                    .as_deref()
                    .ok_or(AgentRunError::Storage)?,
            )
            .map_err(|_| AgentRunError::Storage)?
            .ok_or(AgentRunError::Storage)?;
        if root.actor != EventActor::System
            || root.kind != event_kind::ARTIFACT_CREATED
            || root.session_id != consumer.session_id
            || root.task_id != consumer.task_id
            || root.seq >= consumer.seq
            || after_seq.is_some_and(|seq| root.seq <= seq)
            || root.causation_id.as_deref() != producer
            || root.payload["reference"] != reference
        {
            return Err(AgentRunError::Storage);
        }
        Ok(())
    }
}

fn sort_task(query: &AgentRunQuery) -> Result<String, AgentRunError> {
    validate_query(query)?;
    Ok(format!("sort_{}", query.request_id))
}

fn validate_input_event(
    input: &EventRecord,
    query: &AgentRunQuery,
) -> Result<SortInput, AgentRunError> {
    let source: SortInput =
        serde_json::from_value(input.payload.clone()).map_err(|_| AgentRunError::Conflict)?;
    if source.version != 1
        || source.request_id != query.request_id
        || input.actor != EventActor::User
        || input.kind != event_kind::SORT_REQUESTED
        || input.session_id.as_deref() != Some(&query.session_id)
        || input.task_id.as_deref() != Some(&sort_task(query)?)
        || !input
            .correlation_id
            .as_deref()
            .is_some_and(|id| id.starts_with("turn_"))
        || CanonicalResource::artifact(&source.reference).is_err()
    {
        return Err(AgentRunError::Storage);
    }
    Ok(source)
}

fn same_scope(input: &EventRecord, event: &EventRecord) -> bool {
    input.session_id == event.session_id
        && input.task_id == event.task_id
        && input.correlation_id == event.correlation_id
}
