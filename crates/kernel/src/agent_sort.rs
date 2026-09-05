//! Bounded, independently verified process status inside an agent run.
use ditto_artifact_sort as worker;
use ditto_protocol::{AgentSortProgress, AgentSortState, EventRecord, event_kind};

use crate::{AgentRunError, DittoKernel, agent_run::AgentRunMetadata, turn::sort::*};

impl DittoKernel {
    pub(crate) fn agent_sort_progress(
        &self,
        input: &EventRecord,
        active: bool,
    ) -> Result<Option<AgentSortProgress>, AgentRunError> {
        let metadata: AgentRunMetadata = serde_json::from_value(input.payload["agent_run"].clone())
            .map_err(|_| AgentRunError::Storage)?;
        let Some(grant) = metadata.sort else {
            return Ok(None);
        };
        let get = |id: &str| {
            self.inner
                .events
                .get_by_event_id(id)
                .map_err(|_| AgentRunError::Storage)?
                .ok_or(AgentRunError::Storage)
        };
        let root = get(&grant.source_event_id)?;
        if !grant.matches_root(&root, input) {
            return Err(AgentRunError::Storage);
        }
        let bytes = self.sort_artifact_bytes(&grant.reference, worker::MAX_INPUT_BYTES)?;
        worker::validate_input(&bytes).map_err(|_| AgentRunError::Storage)?;
        if root.payload["bytes"].as_u64() != Some(bytes.len() as u64) {
            return Err(AgentRunError::Storage);
        }
        let events = self
            .inner
            .events
            .agent_sort_events(
                input.session_id.as_deref().ok_or(AgentRunError::Storage)?,
                input.task_id.as_deref().ok_or(AgentRunError::Storage)?,
                input
                    .correlation_id
                    .as_deref()
                    .ok_or(AgentRunError::Storage)?,
            )
            .map_err(|_| AgentRunError::Storage)?;
        if events.len() > 8 {
            return Err(AgentRunError::Storage);
        }
        let mut progress = AgentSortProgress {
            input_reference: grant.reference.clone(),
            allow_deduplicate: grant.allow_deduplicate,
            state: AgentSortState::NotRun,
            output_reference: None,
            output: None,
            failure_code: None,
        };
        let mut start: Option<(SortToolStarted, &EventRecord)> = None;
        let mut claimed_output = false;
        let mut last_output_index = None;
        for event in &events {
            if event.kind == event_kind::AGENT_SORT_STARTED {
                let payload: SortToolStarted = serde_json::from_value(event.payload.clone())
                    .map_err(|_| AgentRunError::Storage)?;
                let requested = get(event
                    .causation_id
                    .as_deref()
                    .ok_or(AgentRunError::Storage)?)?;
                let request: SortToolRequested = serde_json::from_value(requested.payload.clone())
                    .map_err(|_| AgentRunError::Storage)?;
                if start.is_some()
                    || last_output_index.is_some_and(|i| payload.request_index <= i)
                    || !valid_requested(&request, &requested, input)
                    || !valid_started(&payload, event, input, &grant)
                    || !start_matches_request(&payload, event, &request, &requested)
                {
                    return Err(AgentRunError::Storage);
                }
                progress.state = if active {
                    AgentSortState::Running
                } else {
                    AgentSortState::Interrupted
                };
                progress.failure_code = None;
                start = Some((payload, event));
            } else {
                let output: SortToolOutput = serde_json::from_value(event.payload.clone())
                    .map_err(|_| AgentRunError::Storage)?;
                let current_start = if output.claimed {
                    if claimed_output {
                        return Err(AgentRunError::Storage);
                    }
                    Some(start.as_ref().ok_or(AgentRunError::Storage)?)
                } else {
                    None
                };
                let requested = get(current_start
                    .map_or(event.causation_id.as_deref(), |(_, s)| {
                        s.causation_id.as_deref()
                    })
                    .ok_or(AgentRunError::Storage)?)?;
                let request: SortToolRequested = serde_json::from_value(requested.payload.clone())
                    .map_err(|_| AgentRunError::Storage)?;
                if last_output_index.is_some_and(|i| output.request_index <= i)
                    || !valid_requested(&request, &requested, input)
                    || !valid_output(
                        &output,
                        event,
                        input,
                        &grant,
                        &request,
                        &requested,
                        current_start.map(|(_, s)| *s),
                        start.is_some(),
                        input.recorded_at + chrono::Duration::minutes(5),
                    )
                    || current_start
                        .is_some_and(|(s, e)| !start_matches_request(s, e, &request, &requested))
                    || (!output.claimed && start.is_some() && !claimed_output)
                {
                    return Err(AgentRunError::Storage);
                }
                last_output_index = Some(output.request_index);
                if start.is_none() {
                    if let SortToolResult::Error { code } = &output.result {
                        progress.failure_code = serde_json::to_value(code)
                            .ok()
                            .and_then(|v| v.as_str().map(str::to_owned));
                    }
                }
                if let Some((start_payload, started)) = current_start {
                    claimed_output = true;
                    match &output.result {
                        SortToolResult::Error { code } => {
                            progress.state = AgentSortState::Failed;
                            progress.failure_code = serde_json::to_value(code)
                                .ok()
                                .and_then(|v| v.as_str().map(str::to_owned));
                        }
                        SortToolResult::Verified {
                            reference,
                            input_lines,
                            output_lines,
                            ..
                        } => {
                            let root = get(output
                                .artifact_event_id
                                .as_deref()
                                .ok_or(AgentRunError::Storage)?)?;
                            if !valid_output_root(&root, &output, event, started) {
                                return Err(AgentRunError::Storage);
                            }
                            let output_bytes =
                                self.sort_artifact_bytes(reference, worker::MAX_OUTPUT_BYTES)?;
                            if root.payload["bytes"].as_u64() != Some(output_bytes.len() as u64) {
                                return Err(AgentRunError::Storage);
                            }
                            let verified = worker::verify_output(
                                &bytes,
                                output_bytes,
                                start_payload.normalized.unique,
                            )
                            .map_err(|_| AgentRunError::Storage)?;
                            if verified.input_lines() != *input_lines
                                || verified.output_lines() != *output_lines
                            {
                                return Err(AgentRunError::Storage);
                            }
                            progress.state = AgentSortState::Verified;
                            progress.output_reference = Some(reference.clone());
                            progress.output = Some(
                                String::from_utf8(verified.bytes().to_vec())
                                    .map_err(|_| AgentRunError::Storage)?,
                            );
                        }
                    }
                }
            }
        }
        Ok(Some(progress))
    }
}
