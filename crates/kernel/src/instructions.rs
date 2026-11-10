//! The profile's standing instructions: one durable document, superseded by
//! writing a new one (ADR 0045, `docs/design/platform-surface.md` P2).
//!
//! They are journaled rather than read from a file at request time, because a
//! turn must replay against the text that was in force when it ran - a file that
//! someone edits would silently change what an old turn says it was told.

use chrono::{DateTime, Utc};
use ditto_protocol::{EventActor, EventRecord, NewEvent, event_kind};
use serde::{Deserialize, Serialize};

use crate::{DittoKernel, KernelError};

/// Characters a profile may pin. Longer than any instruction a person reads back
/// to themselves; shorter than a document that would crowd out the turn.
pub const MAX_INSTRUCTIONS_CHARS: usize = 2_000;

/// The instruction text in force from `event_id` onward.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstructionDocument {
    /// The text exactly as it was journaled; empty means "no standing
    /// instructions", which is different from "never written".
    pub text: String,
    pub event_id: String,
    pub seq: i64,
    pub recorded_at: DateTime<Utc>,
}

impl InstructionDocument {
    /// The segment the model reads, or `None` when nothing is in force.
    pub fn segment(&self) -> Option<String> {
        segment_for(&self.text)
    }
}

/// The formatted segment for `text`, or `None` when it is blank.
pub(crate) fn segment_for(text: &str) -> Option<String> {
    {
        if text.trim().is_empty() {
            return None;
        }
        Some(format!(
            "Standing instructions from the person you work for. Follow them, and \
             where they conflict with the rules above, say so instead of silently \
             choosing:\n{}",
            text.trim()
        ))
    }
}

impl DittoKernel {
    /// Record new standing instructions, superseding whatever was in force.
    pub fn set_instructions(&self, text: &str) -> Result<InstructionDocument, KernelError> {
        let text = text.trim();
        if text.chars().count() > MAX_INSTRUCTIONS_CHARS {
            return Err(KernelError::InvalidCommand(format!(
                "instructions must be at most {MAX_INSTRUCTIONS_CHARS} characters"
            )));
        }
        let supersedes = self
            .instructions_in_force(self.inner.events.latest_seq()?)?
            .map(|document| document.event_id);
        let event = self.append_and_publish(NewEvent {
            session_id: None,
            task_id: None,
            actor: EventActor::User,
            kind: event_kind::INSTRUCTIONS_RECORDED.to_owned(),
            payload: serde_json::json!({
                "event_version": 1,
                "text": text,
                "supersedes": supersedes,
            }),
            causation_id: None,
            correlation_id: None,
            span_id: None,
        })?;
        Ok(InstructionDocument {
            text: text.to_owned(),
            event_id: event.event_id,
            seq: event.seq,
            recorded_at: event.recorded_at,
        })
    }

    /// The instructions in force through `through_seq` - the text a turn accepted
    /// at or before that sequence was told to follow.
    pub fn instructions_in_force(
        &self,
        through_seq: i64,
    ) -> Result<Option<InstructionDocument>, KernelError> {
        let events = self.inner.events.list_kind_through(
            event_kind::INSTRUCTIONS_RECORDED,
            0,
            through_seq,
            10_000,
        )?;
        Ok(events.last().map(document_from))
    }
}

fn document_from(event: &EventRecord) -> InstructionDocument {
    let text = event
        .payload
        .get("text")
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    InstructionDocument {
        text: text.to_owned(),
        event_id: event.event_id.clone(),
        seq: event.seq,
        recorded_at: event.recorded_at,
    }
}
