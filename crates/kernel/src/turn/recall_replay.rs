use super::super::recall::*;
use super::*;

impl ReplayProjector<'_, '_> {
    /// The memories a version-8 agent run could search, rebuilt from the
    /// recorded compilation in one pass: its nodes, and the excluded nodes'
    /// own recorded events at or before the provenance cutoff.
    fn rebuild_recall_space(
        &self,
        context: &ContextCompiledPayload,
    ) -> Result<Vec<ditto_context::ContextNode>, ReplayError> {
        let wanted = context
            .compiled
            .receipt
            .excluded
            .iter()
            .filter(|exclusion| searchable(&exclusion.reason))
            .map(|exclusion| exclusion.node_id.as_str())
            .collect::<std::collections::BTreeSet<_>>();
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

    /// Replay one `memory.search` call: its request, then the result replay
    /// recomputes over the turn's searchable memories.
    pub(super) fn replay_recall_call(
        &mut self,
        request_index: u8,
        call: &ReadyCall,
    ) -> Result<(), ReplayError> {
        let requested = self.take(event_kind::AGENT_MEMORY_REQUESTED, EventActor::Model)?;
        let request: RecallToolRequested = decode_payload(requested)?;
        if !valid_requested(&request, requested, &self.events[0])
            || request.request_index != request_index
            || request.call_id != call.call_id
            || request.arguments != call.arguments
        {
            return Err(replay_invalid(
                "memory search request contradicts model call",
            ));
        }
        if self.recall_space.is_none() {
            let context = self
                .context_payload
                .as_ref()
                .ok_or_else(|| replay_invalid("memory search precedes the compiled context"))?;
            self.recall_space = Some(self.rebuild_recall_space(context)?);
        }
        let event = self.take(event_kind::AGENT_MEMORY_OUTPUT, EventActor::Capability)?;
        let output: RecallToolOutput = decode_payload(event)?;
        if !valid_output(
            &output,
            event,
            &request,
            requested,
            &self.events[0],
            self.recall_space.as_deref().unwrap_or_default(),
            self.version.unwrap_or_default(),
        ) {
            return Err(replay_invalid(
                "memory search result differs from the searchable memories",
            ));
        }
        self.conversation.push(ConversationItem::ToolResult {
            call_id: call.call_id.clone(),
            content: vec![ContentPart::Structured {
                value: output.result.model_value(self.version.unwrap_or_default()),
            }],
            is_error: output.result.is_error(),
        });
        self.recall_calls.push(ReplayedRecallCall {
            requested: request,
            output,
        });
        self.tool_call_count += 1;
        Ok(())
    }
}
