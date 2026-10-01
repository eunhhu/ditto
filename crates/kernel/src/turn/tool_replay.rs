//! Replaying agent tool calls from their shared lifecycle (ADR 0036), without
//! provider, network or process I/O: the recorded results are the results,
//! checked against each tool's rules.
use super::super::memory::{self as memory_tool, MemoryAction, MemoryResult};
use super::super::sort::{self as sort_tool, SortToolResult};
use super::super::tool::{
    self as tool, ReplayedToolCall, STOPPED, ToolOutput, ToolRequested, ToolStarted,
};
use super::super::web::{self as web_tool, WebResult};
use super::*;
use ditto_web_fetch::WebRequest;
use serde_json::json;

impl<'turn> ReplayProjector<'turn, '_> {
    /// Take `tool.requested` for `call`, normalized by replay's own rule.
    fn take_tool_request(
        &mut self,
        request_index: u8,
        call: &ReadyCall,
        normalized: Option<Value>,
    ) -> Result<(&'turn EventRecord, ToolRequested), ReplayError> {
        let event = self.take(event_kind::TOOL_REQUESTED, EventActor::Model)?;
        let request: ToolRequested = decode_payload(event)?;
        if !tool::valid_requested(&request, event, &self.events[0], normalized.as_ref())
            || request.request_index != request_index
            || request.call_id != call.call_id
            || request.capability_id != call.capability_id
            || request.arguments != call.arguments
        {
            return Err(replay_invalid("tool request contradicts the model's call"));
        }
        Ok((event, request))
    }

    /// The journaled stop of a call before authorization, if it comes next;
    /// the call is recorded without an output.
    fn take_tool_stop(
        &mut self,
        request_index: u8,
        call: &ReadyCall,
        request: &ToolRequested,
    ) -> Result<Option<TurnFailure>, ReplayError> {
        if !self.next_is_failure() {
            return Ok(None);
        }
        let time = self.events[self.index].recorded_at;
        let failure = self.take_failure()?.expect("checked failure");
        if failure.message != STOPPED
            || failure.reason.is_some()
            || failure.request_index != Some(request_index)
            || failure.call_id.as_ref() != Some(&call.call_id)
            || !match failure.code {
                TurnFailureCode::Cancelled => failure.evidence.is_none(),
                TurnFailureCode::DeadlineExceeded => self.valid_deadline_failure(&failure, time),
                _ => false,
            }
        {
            return Err(replay_invalid("invalid tool checkpoint failure"));
        }
        self.tool_calls.push(ReplayedToolCall {
            requested: request.clone(),
            started: None,
            output: None,
        });
        Ok(Some(failure))
    }

    /// Take `tool.started` when it comes next: a claim under the turn's epoch
    /// and deadline.
    fn take_tool_start(
        &mut self,
        request: &ToolRequested,
        requested: &EventRecord,
    ) -> Result<Option<(&'turn EventRecord, ToolStarted)>, ReplayError> {
        if self
            .events
            .get(self.index)
            .is_none_or(|event| event.kind != event_kind::TOOL_STARTED)
        {
            return Ok(None);
        }
        let event = self.take(event_kind::TOOL_STARTED, EventActor::Capability)?;
        let start: ToolStarted = decode_payload(event)?;
        let epoch = self
            .execution_epoch_id
            .as_ref()
            .ok_or_else(|| replay_invalid("tool call precedes the selection"))?;
        let deadline = self
            .deadline
            .ok_or_else(|| replay_invalid("tool call has no turn deadline"))?;
        if !tool::valid_started(&start, event, request, requested, &self.events[0])
            || start.epoch_id != epoch.as_str()
            || start.expires_at != deadline
        {
            return Err(replay_invalid(
                "tool start contradicts its request or lease",
            ));
        }
        Ok(Some((event, start)))
    }

    /// Take `tool.output` and decode its result as `T`.
    fn take_tool_output<T: DeserializeOwned>(
        &mut self,
        request: &ToolRequested,
        requested: &EventRecord,
        started: Option<&EventRecord>,
    ) -> Result<(&'turn EventRecord, ToolOutput, T), ReplayError> {
        let event = self.take(event_kind::TOOL_OUTPUT, EventActor::Capability)?;
        let output: ToolOutput = decode_payload(event)?;
        if !tool::valid_output(&output, event, request, requested, started, &self.events[0]) {
            return Err(replay_invalid("tool output contradicts its request"));
        }
        let result = serde_json::from_value(output.result.clone())
            .map_err(|_| replay_invalid("tool result is not the tool's"))?;
        Ok((event, output, result))
    }

    /// Whether `event` came at or after the turn's deadline.
    fn after_deadline(&self, event: &EventRecord) -> bool {
        self.deadline
            .is_some_and(|deadline| event.recorded_at >= deadline)
    }

    /// Feed a replayed result back to the model's conversation and record
    /// the call.
    fn record_tool_call(
        &mut self,
        call: &ReadyCall,
        call_record: ReplayedToolCall,
        model_value: Value,
        is_error: bool,
    ) {
        self.conversation.push(ConversationItem::ToolResult {
            call_id: call.call_id.clone(),
            content: vec![ContentPart::Structured { value: model_value }],
            is_error,
        });
        self.tool_calls.push(call_record);
        self.tool_call_count += 1;
    }

    /// Replay one `web.browse` call: a read of a granted URL or a search,
    /// each counted against its own lease.
    pub(super) fn replay_web_call(
        &mut self,
        request_index: u8,
        call: &ReadyCall,
    ) -> Result<Option<TurnFailure>, ReplayError> {
        let (read, search) = self
            .web
            .ok_or_else(|| replay_invalid("web.browse was not selected"))?;
        let normalized = web_tool::normalize_call(&call.arguments, read, search);
        let (requested, request) =
            self.take_tool_request(request_index, call, normalized.as_ref().map(|r| json!(r)))?;
        if let Some(failure) = self.take_tool_stop(request_index, call, &request)? {
            return Ok(Some(failure));
        }
        let start = self.take_tool_start(&request, requested)?;
        let used = match &normalized {
            Some(WebRequest::Read { .. }) => self.web_reads,
            Some(WebRequest::Search { .. }) => self.web_searches,
            None => 0,
        };
        if let Some((_, started)) = &start
            && !normalized.as_ref().is_some_and(|request| {
                web_tool::valid_start(started, request, &self.web_grant, used)
            })
        {
            return Err(replay_invalid(
                "web call started against its grant or lease",
            ));
        }
        let (event, output, result): (_, _, WebResult) =
            self.take_tool_output(&request, requested, start.as_ref().map(|(event, _)| *event))?;
        if !web_tool::valid_result(
            &result,
            normalized.as_ref(),
            start.is_some(),
            &self.web_grant,
            used,
            self.after_deadline(event),
        ) {
            return Err(replay_invalid("web result contradicts its call"));
        }
        match (&normalized, start.is_some()) {
            (Some(WebRequest::Read { .. }), true) => self.web_reads += 1,
            (Some(WebRequest::Search { .. }), true) => self.web_searches += 1,
            _ => {}
        }
        let record = ReplayedToolCall {
            requested: request,
            started: start.map(|(_, started)| started),
            output: Some(output),
        };
        self.record_tool_call(call, record, result.model_value(), result.is_error());
        Ok(None)
    }

    /// The memories a search could read, rebuilt from the recorded
    /// compilation in one pass: its nodes, and the excluded nodes' own
    /// recorded events at or before the provenance cutoff.
    fn rebuild_recall_space(
        &self,
        context: &ContextCompiledPayload,
    ) -> Result<Vec<ditto_context::ContextNode>, ReplayError> {
        let wanted = context
            .compiled
            .receipt
            .excluded
            .iter()
            .filter(|exclusion| memory_tool::searchable(&exclusion.reason))
            .map(|exclusion| exclusion.node_id.as_str())
            .collect::<BTreeSet<_>>();
        let mut recorded = std::collections::BTreeMap::new();
        for event in self.snapshot.iter().filter(|event| {
            event.kind == event_kind::CONTEXT_NODE_RECORDED
                && event.seq <= context.provenance_through_seq
                && event.session_id.as_deref() == Some(self.session_id.as_str())
        }) {
            let Some(node) = event.payload.get("node") else {
                continue;
            };
            let Some(id) = node.get("id").and_then(Value::as_str) else {
                continue;
            };
            if !wanted.contains(id) || recorded.contains_key(id) {
                continue;
            }
            if let Ok(node) = serde_json::from_value::<ditto_context::ContextNode>(node.clone())
                && node.id == id
            {
                recorded.insert(id, node);
            }
        }
        if let Some(missing) = wanted.iter().find(|id| !recorded.contains_key(*id)) {
            return Err(replay_invalid(format!(
                "excluded context node {missing} has no recorded source"
            )));
        }
        Ok(super::super::recall_space(
            &context.compiled,
            recorded.values(),
        ))
    }

    /// Replay one `memory.manage` call: a search replay recomputes, or a
    /// write whose decision replay recomputes and whose `memory.written`
    /// event and context node it checks.
    pub(super) fn replay_memory_call(
        &mut self,
        request_index: u8,
        call: &ReadyCall,
    ) -> Result<Option<TurnFailure>, ReplayError> {
        if !self.memory_selected {
            return Err(replay_invalid("memory.manage was not selected"));
        }
        let action = memory_tool::normalize_call(&call.arguments);
        let (requested, request) =
            self.take_tool_request(request_index, call, action.as_ref().map(|a| json!(a)))?;
        if let Some(failure) = self.take_tool_stop(request_index, call, &request)? {
            return Ok(Some(failure));
        }
        let (event, output, result): (_, _, MemoryResult) =
            self.take_tool_output(&request, requested, None)?;
        let consistent = match &action {
            Some(MemoryAction::Search { query }) => {
                if self.recall_space.is_none() {
                    let context = self
                        .context_payload
                        .as_ref()
                        .ok_or_else(|| replay_invalid("memory search precedes the context"))?;
                    self.recall_space = Some(self.rebuild_recall_space(context)?);
                }
                result
                    == MemoryResult::search(query, self.recall_space.as_deref().unwrap_or_default())
            }
            _ => {
                let snapshot = self.snapshot;
                let session = self.session_id.as_str();
                let caused = snapshot
                    .iter()
                    .filter(|record| {
                        record.kind == event_kind::MEMORY_WRITTEN
                            && record.causation_id.as_deref() == Some(&requested.event_id)
                    })
                    .count();
                let decided = |before_seq: i64, at| {
                    memory_tool::refusal(
                        action.as_ref(),
                        self.read_external_content,
                        self.memory_write_count,
                        |target| {
                            memory_tool::active_memory_in(snapshot, session, target, before_seq, at)
                        },
                    )
                };
                match &result {
                    MemoryResult::Refused { code } => {
                        caused == 0 && decided(event.seq, event.recorded_at) == Some(*code)
                    }
                    MemoryResult::Remembered { .. } | MemoryResult::Forgotten { .. } => {
                        caused == 1
                            && action
                                .as_ref()
                                .and_then(MemoryAction::write)
                                .and_then(|write| {
                                    memory_tool::written_records(
                                        snapshot, &request, &write, requested, &output, &result,
                                        event,
                                    )
                                })
                                .is_some_and(|(written, _)| {
                                    decided(written.seq, written.recorded_at).is_none()
                                })
                    }
                    MemoryResult::Found { .. } => false,
                }
            }
        };
        if !consistent {
            return Err(replay_invalid(
                "memory call contradicts its rules or its records",
            ));
        }
        self.memory_write_count += u32::from(matches!(
            result,
            MemoryResult::Remembered { .. } | MemoryResult::Forgotten { .. }
        ));
        let record = ReplayedToolCall {
            requested: request,
            started: None,
            output: Some(output),
        };
        self.record_tool_call(call, record, result.model_value(), result.is_error());
        Ok(None)
    }

    /// Replay one `artifact.sort` call on the permitted attachment, at most
    /// one claim a turn, its verified output rooted in the session.
    pub(super) fn replay_sort_call(
        &mut self,
        request_index: u8,
        call: &ReadyCall,
    ) -> Result<Option<TurnFailure>, ReplayError> {
        let grant = self
            .sort
            .clone()
            .ok_or_else(|| replay_invalid("sort has no user permission"))?;
        let arguments = sort_tool::normalize_call(&call.arguments);
        let (requested, request) =
            self.take_tool_request(request_index, call, arguments.as_ref().map(|a| json!(a)))?;
        if let Some(failure) = self.take_tool_stop(request_index, call, &request)? {
            return Ok(Some(failure));
        }
        let start = self.take_tool_start(&request, requested)?;
        if start.is_some()
            && (self.sort_claimed
                || !arguments
                    .as_ref()
                    .is_some_and(|arguments| grant.permits(arguments)))
        {
            return Err(replay_invalid(
                "sort dispatch contradicts permission or lease",
            ));
        }
        let started = start.as_ref().map(|(event, _)| *event);
        let (event, output, result): (_, _, SortToolResult) =
            self.take_tool_output(&request, requested, started)?;
        if !sort_tool::valid_result(
            &result,
            arguments.as_ref(),
            started.is_some(),
            &grant,
            self.sort_claimed,
            self.after_deadline(event),
            output.artifact_event_id.as_deref(),
        ) {
            return Err(replay_invalid("sort output contradicts dispatch"));
        }
        if let SortToolResult::Verified { reference, .. } = &result {
            let started = started.ok_or_else(|| replay_invalid("sort output has no start"))?;
            if !self.snapshot.iter().any(|root| {
                sort_tool::valid_output_root(
                    root,
                    reference,
                    output.artifact_event_id.as_deref(),
                    event,
                    started,
                )
            }) {
                return Err(replay_invalid("sort output has no matching artifact root"));
            }
        }
        self.sort_claimed |= started.is_some();
        let record = ReplayedToolCall {
            requested: request,
            started: start.map(|(_, started)| started),
            output: Some(output),
        };
        self.record_tool_call(call, record, result.model_value(), result.is_error());
        Ok(None)
    }
}
