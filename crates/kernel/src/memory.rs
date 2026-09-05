use chrono::Utc;
use ditto_context::{
    ContextLens, ContextNode, ContextNodeKind, ContextOrigin, ContextScope, EpistemicStatus,
};
use ditto_context_projection::{
    ContextNodeRecordedPayloadV1, ContextProjectionError, VerifiedContextSnapshot,
};
use ditto_protocol::{
    EventActor, EventRecord, MAX_USER_MEMORY_BYTES, MAX_USER_MEMORY_PAGE_SIZE, MemoryPage,
    MemoryQuery, MemoryWriteOutcome, RememberInputCommand, RememberInputResponse, UserMemory,
    event_kind,
};
use ditto_retrieval::{RetrievalWorkBudget, SessionId};
use ulid::Ulid;

use crate::{DittoKernel, KernelError, TrustedContextNodeDraft};

impl DittoKernel {
    /// Promote exact same-session user input without exposing trusted drafts.
    pub fn remember_input(
        &self,
        command: RememberInputCommand,
    ) -> Result<RememberInputResponse, KernelError> {
        validate_session(&command.session_id)?;
        validate_input_id(&command.input_event_id)?;
        if let Some(id) = &command.replaces {
            validate_memory_id(id)?;
        }
        let _gate = self
            .inner
            .context_admission_gate
            .lock()
            .map_err(|_| KernelError::ContextAdmissionGatePoisoned)?;
        let source = self
            .inner
            .events
            .get_by_event_id(&command.input_event_id)?
            .ok_or_else(|| invalid("memory source input is unavailable in this session"))?;
        let text = source_text(&source, &command.session_id)?;
        let node = memory_node(&command.input_event_id, text, command.replaces.clone());
        let high_water = self.inner.events.latest_seq()?;
        self.inner
            .context_projection
            .synchronize_through(&self.inner.events, high_water)?;
        let draft = TrustedContextNodeDraft::session(&command.session_id, node.clone());
        let validated = match self.inner.context_projection.validate_draft(
            &self.inner.events,
            high_water,
            &draft,
        ) {
            Ok(validated) => validated,
            Err(ContextProjectionError::DuplicateNodeIdentity { event_id, .. }) => {
                let existing = self
                    .inner
                    .events
                    .get_by_event_id(&event_id)?
                    .ok_or_else(|| invalid("committed memory event is unavailable"))?;
                let payload: ContextNodeRecordedPayloadV1 =
                    serde_json::from_value(existing.payload.clone())?;
                if payload.event_version != 1 || payload.node != node {
                    return Err(KernelError::MemoryConflict(
                        "this input was already saved with different replacement intent",
                    ));
                }
                return Ok(write_response(
                    &node.id,
                    existing,
                    MemoryWriteOutcome::AlreadyRecorded,
                ));
            }
            Err(
                ContextProjectionError::MissingSupersededNode { .. }
                | ContextProjectionError::SupersessionScopeMismatch { .. }
                | ContextProjectionError::SelfSupersession { .. },
            ) => {
                return Err(KernelError::MemoryConflict(
                    "replacement target is not an active memory in this session",
                ));
            }
            Err(error) => return Err(error.into()),
        };

        if let Some(replaces) = &command.replaces {
            let snapshot = self.memory_snapshot_locked(&command.session_id, high_water)?;
            let target = snapshot
                .candidates()
                .iter()
                .find(|candidate| candidate.id == *replaces)
                .ok_or(KernelError::MemoryConflict(
                    "replacement target is no longer active in this session",
                ))?;
            self.verify_memory_source(target, &command.session_id)?;
        }
        match self.commit_context_node(&validated) {
            Ok(event) => Ok(write_response(
                &node.id,
                event,
                MemoryWriteOutcome::Recorded,
            )),
            Err(KernelError::CommittedButProjectionUnavailable { event, .. }) => {
                Ok(write_response(
                    &node.id,
                    *event,
                    MemoryWriteOutcome::CommittedButProjectionUnavailable,
                ))
            }
            Err(error) => Err(error),
        }
    }

    /// Inspect current explicit memory through the existing verified snapshot.
    pub fn list_memories(&self, query: MemoryQuery) -> Result<MemoryPage, KernelError> {
        validate_session(&query.session_id)?;
        if let Some(id) = &query.after_id {
            validate_memory_id(id)?;
        }
        let limit = query.limit.unwrap_or(20);
        if !(1..=MAX_USER_MEMORY_PAGE_SIZE).contains(&limit) {
            return Err(invalid("memory page limit must be between 1 and 100"));
        }
        let _gate = self
            .inner
            .context_admission_gate
            .lock()
            .map_err(|_| KernelError::ContextAdmissionGatePoisoned)?;
        let through_seq = self.inner.events.latest_seq()?;
        let snapshot = self.memory_snapshot_locked(&query.session_id, through_seq)?;
        let mut candidates: Vec<_> = snapshot
            .candidates()
            .iter()
            .filter(|node| {
                node.id.len() == 33
                    && node.id.starts_with("memory-")
                    && query.after_id.as_ref().is_none_or(|after| node.id > *after)
            })
            .collect();
        candidates.sort_unstable_by(|left, right| left.id.cmp(&right.id));
        let more = candidates.len() > limit;
        let memories = candidates
            .into_iter()
            .take(limit)
            .map(|node| self.verify_memory_source(node, &query.session_id))
            .collect::<Result<Vec<_>, _>>()?;
        let next_after_id = more.then(|| memories.last().expect("positive page limit").id.clone());
        Ok(MemoryPage {
            memories,
            next_after_id,
            through_seq,
        })
    }

    fn memory_snapshot_locked(
        &self,
        session: &str,
        high_water: i64,
    ) -> Result<VerifiedContextSnapshot, KernelError> {
        Ok(self
            .inner
            .context_projection
            .synchronize_and_verified_snapshot_through_at(
                &self.inner.events,
                high_water,
                session,
                None,
                Utc::now(),
                &mut RetrievalWorkBudget::new(),
            )?)
    }

    fn verify_memory_source(
        &self,
        node: &ContextNode,
        session: &str,
    ) -> Result<UserMemory, KernelError> {
        let input_id = validate_memory_id(&node.id)?;
        let source = self
            .inner
            .events
            .get_by_event_id(&input_id)?
            .ok_or_else(|| invalid("memory source input is unavailable in this session"))?;
        let text = source_text(&source, session)?;
        let replaces = match node.supersedes.as_slice() {
            [] => None,
            [id] => {
                validate_memory_id(id)?;
                Some(id.clone())
            }
            _ => return Err(invalid("stored memory has invalid replacement metadata")),
        };
        if *node != memory_node(&input_id, text, replaces.clone()) {
            return Err(invalid("stored memory does not match its exact user input"));
        }
        Ok(UserMemory {
            id: node.id.clone(),
            text: text.to_owned(),
            input_event_id: input_id,
            replaces,
        })
    }
}

fn memory_node(input_id: &str, text: &str, replaces: Option<String>) -> ContextNode {
    ContextNode {
        id: format!("memory-{}", input_id.to_ascii_lowercase()),
        kind: ContextNodeKind::Claim,
        summary: text.to_owned(),
        origin: ContextOrigin::User,
        epistemic: EpistemicStatus::Asserted,
        scope: ContextScope::Session,
        lens: ContextLens::Personal,
        confidence: 1.0,
        source_event_ids: vec![input_id.to_owned()],
        supersedes: replaces.into_iter().collect(),
        valid_from: None,
        valid_until: None,
    }
}

fn source_text<'a>(source: &'a EventRecord, session: &str) -> Result<&'a str, KernelError> {
    if source.actor != EventActor::User
        || source.kind != event_kind::INPUT_RECEIVED
        || source.session_id.as_deref() != Some(session)
        || source.task_id.is_some()
    {
        return Err(invalid(
            "memory source must be task-free user input from this session",
        ));
    }
    let text = source
        .payload
        .get("text")
        .and_then(|value| value.as_str())
        .ok_or_else(|| invalid("memory source has no text"))?;
    if text.trim().is_empty() || text.len() > MAX_USER_MEMORY_BYTES {
        return Err(invalid(
            "memory text must contain 1 through 4096 UTF-8 bytes",
        ));
    }
    Ok(text)
}

fn validate_session(session: &str) -> Result<(), KernelError> {
    if session.len() > ditto_retrieval::MAX_RETRIEVAL_IDENTIFIER_BYTES {
        return Err(invalid("memory session is not canonical"));
    }
    SessionId::new(session).map_err(|_| invalid("memory session is not canonical"))?;
    Ok(())
}

fn validate_input_id(id: &str) -> Result<(), KernelError> {
    let parsed = (id.len() == 26)
        .then(|| Ulid::from_string(id).ok())
        .flatten();
    if parsed.is_none_or(|value| value.to_string() != id) {
        return Err(invalid("memory input ID must be a canonical ULID"));
    }
    Ok(())
}

fn validate_memory_id(id: &str) -> Result<String, KernelError> {
    if id.len() != 33 {
        return Err(invalid("invalid memory ID"));
    }
    let input_id = id
        .strip_prefix("memory-")
        .ok_or_else(|| invalid("invalid memory ID"))?;
    let canonical = input_id.to_ascii_uppercase();
    validate_input_id(&canonical)?;
    if canonical.to_ascii_lowercase() != input_id {
        return Err(invalid("memory ID must use lowercase canonical text"));
    }
    Ok(canonical)
}

fn write_response(
    id: &str,
    event: EventRecord,
    outcome: MemoryWriteOutcome,
) -> RememberInputResponse {
    RememberInputResponse {
        memory_id: id.to_owned(),
        event_id: event.event_id,
        event_seq: event.seq,
        outcome,
    }
}

fn invalid(message: &str) -> KernelError {
    KernelError::InvalidCommand(message.to_owned())
}
