//! Conversation threads: a reset marker bounds the history that later agent
//! runs replay (ADR 0022). Memories and prior events are never removed.
use ditto_protocol::{
    ConversationExchange, ConversationQuery, ConversationResetResponse, ConversationView,
    EventActor, MAX_CONVERSATION_VIEW_EXCHANGES, NewEvent, ResetConversationCommand, event_kind,
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

    /// The current thread's newest finished exchanges, oldest first and
    /// unabridged, for display. The model's bounded history rule is separate.
    pub fn conversation_view(
        &self,
        query: ConversationQuery,
    ) -> Result<ConversationView, KernelError> {
        SessionId::new(&query.session_id)
            .map_err(|_| KernelError::InvalidCommand("session ID is not canonical".into()))?;
        let limit = query
            .limit
            .unwrap_or(MAX_CONVERSATION_VIEW_EXCHANGES)
            .clamp(1, MAX_CONVERSATION_VIEW_EXCHANGES);
        let through_seq = self.latest_event_seq()?;
        let mut exchanges = self
            .thread_exchanges(&query.session_id, through_seq.saturating_add(1), limit)?
            .into_iter()
            .map(|thread| ConversationExchange {
                task_id: thread.task_id,
                turn_id: thread.exchange.turn_id,
                user: thread.exchange.user,
                assistant: thread.exchange.assistant,
                finished_seq: thread.finished_seq,
            })
            .collect::<Vec<_>>();
        exchanges.reverse();
        Ok(ConversationView {
            session_id: query.session_id,
            exchanges,
            through_seq,
        })
    }
}
