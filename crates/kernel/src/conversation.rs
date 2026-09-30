//! Conversation threads: a reset marker bounds the history that later agent
//! runs replay (ADR 0022). Memories and prior events are never removed.
use ditto_protocol::{
    ConversationResetResponse, EventActor, NewEvent, ResetConversationCommand, event_kind,
};
use ditto_retrieval::SessionId;
use serde_json::json;

use crate::{DittoKernel, KernelError};

impl DittoKernel {
    /// Start a new conversation thread in a session. Later runs replay only
    /// exchanges recorded after this marker; explicit memories are unchanged.
    pub fn reset_conversation(
        &self,
        command: ResetConversationCommand,
    ) -> Result<ConversationResetResponse, KernelError> {
        SessionId::new(&command.session_id)
            .map_err(|_| KernelError::InvalidCommand("session ID is not canonical".into()))?;
        let event = self.append_and_publish(NewEvent {
            session_id: Some(command.session_id.clone()),
            task_id: None,
            actor: EventActor::User,
            kind: event_kind::CONVERSATION_RESET.into(),
            payload: json!({ "version": 1 }),
            causation_id: None,
            correlation_id: None,
            span_id: None,
        })?;
        Ok(ConversationResetResponse {
            session_id: command.session_id,
            event_id: event.event_id,
            event_seq: event.seq,
        })
    }
}
