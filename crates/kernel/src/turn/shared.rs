use ditto_model::{ContentPart, ConversationItem, MessageRole, ProviderCallId, StableSystemPrefix};
use serde_json::Value;

use super::types::{MAX_TURN_FAILURE_MESSAGE_BYTES, TurnFailureCode};

pub(super) const STABLE_PREFIX_SEGMENTS: [&str; 2] = [
    "You are Ditto's model strategy component. The harness owns context, capability authority, effects, persistence, and verification.",
    "Use only the complete capability schemas supplied for this execution epoch. A model terminal is not verified task completion.",
];
#[derive(Clone)]
pub(super) struct ReadyCall {
    pub(super) call_id: ProviderCallId,
    pub(super) capability_id: String,
    pub(super) arguments: Value,
}
pub(super) fn stable_system_prefix() -> StableSystemPrefix {
    StableSystemPrefix {
        segments: STABLE_PREFIX_SEGMENTS.map(str::to_owned).to_vec(),
    }
}

pub(super) fn append_assistant_text(conversation: &mut Vec<ConversationItem>, text: &str) {
    if let Some(ConversationItem::Message {
        role: MessageRole::Assistant,
        content,
    }) = conversation.last_mut()
        && let [ContentPart::Text { text: previous }] = content.as_mut_slice()
    {
        previous.push_str(text);
        return;
    }
    conversation.push(ConversationItem::Message {
        role: MessageRole::Assistant,
        content: vec![ContentPart::Text {
            text: text.to_owned(),
        }],
    });
}
pub(super) fn turn_failure_code_for_model(kind: ditto_model::FailureKind) -> TurnFailureCode {
    match kind {
        ditto_model::FailureKind::Cancelled => TurnFailureCode::Cancelled,
        // Provider-reported deadlines are provider failures. Only the
        // harness's own effective-deadline checkpoints may claim the typed
        // `DeadlineExceeded` terminal/evidence contract.
        ditto_model::FailureKind::DeadlineExceeded => TurnFailureCode::ModelFailure,
        ditto_model::FailureKind::Protocol => TurnFailureCode::Protocol,
        ditto_model::FailureKind::Provider
        | ditto_model::FailureKind::Transport
        | ditto_model::FailureKind::MalformedToolArguments
        | ditto_model::FailureKind::UnsupportedFeature => TurnFailureCode::ModelFailure,
    }
}

pub(super) fn bounded_turn_failure_message(message: &str) -> String {
    const SUFFIX: &str = "...[truncated]";
    if message.len() <= MAX_TURN_FAILURE_MESSAGE_BYTES {
        return message.to_owned();
    }
    let mut end = MAX_TURN_FAILURE_MESSAGE_BYTES.saturating_sub(SUFFIX.len());
    while !message.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    format!("{}{SUFFIX}", &message[..end])
}
