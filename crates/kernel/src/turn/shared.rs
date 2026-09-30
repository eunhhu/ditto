use ditto_model::{ContentPart, ConversationItem, MessageRole, ProviderCallId, StableSystemPrefix};
use ditto_protocol::{EventActor, EventRecord, event_kind};
use serde_json::Value;

use super::types::{MAX_TURN_FAILURE_MESSAGE_BYTES, TurnFailureCode};

pub(super) const STABLE_PREFIX_SEGMENTS: [&str; 2] = [
    "You are Ditto's model strategy component. The harness owns context, capability authority, effects, persistence, and verification.",
    "Use only the complete capability schemas supplied for this execution epoch. A model terminal is not verified task completion.",
];
/// Kernel-owned cancellation and deadline checkpoints of the turn loop.
///
/// Runtime and replay share these exact messages; each stage-specific pair is
/// part of the durable failure contract, so wording changes need a new turn
/// payload version.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Checkpoint {
    BeforeContextCompilation,
    BeforeModelRequest,
    AfterModelRequestPersisted,
    AwaitingModelOutput,
    BeforeCapabilityRequest,
    AfterCapabilityRequest,
    AfterExecutionStarted,
    AfterArtifactRead,
    AfterFinalOutput,
}

impl Checkpoint {
    pub(super) const fn cancelled_message(self) -> &'static str {
        match self {
            Self::BeforeContextCompilation => "turn was cancelled before context compilation",
            Self::BeforeModelRequest => "turn was cancelled before a model request",
            Self::AfterModelRequestPersisted => {
                "turn was cancelled after persisting a model request and before driver invocation"
            }
            Self::AwaitingModelOutput => "turn was cancelled while awaiting model output",
            Self::BeforeCapabilityRequest => "turn was cancelled before capability request",
            Self::AfterCapabilityRequest => {
                "turn was cancelled after capability request and before execution started"
            }
            Self::AfterExecutionStarted => {
                "turn was cancelled after execution started and before its result"
            }
            Self::AfterArtifactRead => {
                "turn was cancelled after the artifact read and before its result"
            }
            Self::AfterFinalOutput => {
                "turn was cancelled after final model output and before turn completion"
            }
        }
    }

    pub(super) const fn deadline_message(self) -> &'static str {
        match self {
            Self::BeforeContextCompilation => "turn deadline elapsed before context compilation",
            Self::BeforeModelRequest => "turn deadline elapsed before a model request",
            Self::AfterModelRequestPersisted => {
                "turn deadline elapsed after persisting a model request and before driver invocation"
            }
            Self::AwaitingModelOutput => "turn deadline elapsed while awaiting model output",
            Self::BeforeCapabilityRequest => "turn deadline elapsed before capability execution",
            Self::AfterCapabilityRequest => {
                "turn deadline elapsed after capability request and before execution started"
            }
            Self::AfterExecutionStarted => {
                "turn deadline elapsed after execution started and before its result"
            }
            Self::AfterArtifactRead => {
                "turn deadline elapsed after the artifact read and before its result"
            }
            Self::AfterFinalOutput => {
                "turn deadline elapsed after final model output and before turn completion"
            }
        }
    }
}

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

/// Conversation history bounds (turn payload version 3, ADR 0022). Changing
/// any of them changes recorded conversations and needs a new version.
pub(super) const MAX_HISTORY_EXCHANGES: usize = 8;
/// Finished turns examined per thread, including skipped non-agent turns.
pub(super) const MAX_HISTORY_CANDIDATES: usize = 32;
pub(super) const MAX_HISTORY_MESSAGE_BYTES: usize = 4 * 1_024;
pub(super) const MAX_HISTORY_BYTES: usize = 24 * 1_024;

/// One finished agent-run exchange of the current conversation thread.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HistoryExchange {
    pub(crate) turn_id: String,
    pub(crate) user: String,
    pub(crate) assistant: String,
}

/// A stored exchange with the task and `turn.finished` sequence it came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ThreadExchange {
    pub(crate) task_id: String,
    pub(crate) finished_seq: i64,
    pub(crate) exchange: HistoryExchange,
}

/// The single history rule shared by runtime and replay: take exchanges newest
/// first, bound each message, stop at the exchange or byte limit, and return
/// them oldest first.
pub(super) fn select_history(
    newest_first: impl IntoIterator<Item = HistoryExchange>,
) -> Vec<HistoryExchange> {
    let mut selected = Vec::new();
    let mut used = 0_usize;
    for mut exchange in newest_first.into_iter().take(MAX_HISTORY_EXCHANGES) {
        exchange.user = bounded_history_text(&exchange.user);
        exchange.assistant = bounded_history_text(&exchange.assistant);
        let cost = exchange.user.len() + exchange.assistant.len();
        if used + cost > MAX_HISTORY_BYTES {
            break;
        }
        used += cost;
        selected.push(exchange);
    }
    selected.reverse();
    selected
}

/// Replay the exchanges as native conversation messages before the request.
pub(super) fn history_messages(history: &[HistoryExchange]) -> Vec<ConversationItem> {
    history
        .iter()
        .flat_map(|exchange| {
            [
                (MessageRole::User, &exchange.user),
                (MessageRole::Assistant, &exchange.assistant),
            ]
        })
        .map(|(role, text)| ConversationItem::Message {
            role,
            content: vec![ContentPart::Text { text: text.clone() }],
        })
        .collect()
}

/// The user text of an explicit agent-run input; `None` for other turns.
pub(super) fn agent_run_text(input: &EventRecord) -> Option<&str> {
    if input.kind != event_kind::INPUT_RECEIVED
        || input.actor != EventActor::User
        || !input.payload.get("agent_run").is_some_and(Value::is_object)
    {
        return None;
    }
    input.payload.get("text")?.as_str()
}

fn bounded_history_text(text: &str) -> String {
    const SUFFIX: &str = "...[truncated]";
    if text.len() <= MAX_HISTORY_MESSAGE_BYTES {
        return text.to_owned();
    }
    let mut end = MAX_HISTORY_MESSAGE_BYTES - SUFFIX.len();
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{SUFFIX}", &text[..end])
}

#[cfg(test)]
mod history_tests {
    use super::*;

    fn exchange(index: usize, bytes: usize) -> HistoryExchange {
        HistoryExchange {
            turn_id: format!("turn_{index}"),
            user: "u".repeat(bytes),
            assistant: "a".repeat(bytes),
        }
    }

    #[test]
    fn history_keeps_the_newest_bounded_exchanges_oldest_first() {
        let newest_first = (0..12).rev().map(|index| exchange(index, 10));
        let selected = select_history(newest_first);
        assert_eq!(
            selected
                .iter()
                .map(|e| e.turn_id.as_str())
                .collect::<Vec<_>>(),
            [
                "turn_4", "turn_5", "turn_6", "turn_7", "turn_8", "turn_9", "turn_10", "turn_11"
            ]
        );
        let messages = history_messages(&selected[..1]);
        assert!(matches!(
            &messages[..],
            [
                ConversationItem::Message {
                    role: MessageRole::User,
                    ..
                },
                ConversationItem::Message {
                    role: MessageRole::Assistant,
                    ..
                }
            ]
        ));
    }

    #[test]
    fn long_messages_truncate_on_char_boundaries_and_the_byte_budget_stops_older_turns() {
        let long = HistoryExchange {
            turn_id: "turn_long".into(),
            user: "한".repeat(3_000),
            assistant: "x".repeat(10_000),
        };
        let bounded = select_history([long]);
        assert!(bounded[0].user.len() <= MAX_HISTORY_MESSAGE_BYTES);
        assert!(bounded[0].user.ends_with("...[truncated]"));
        assert_eq!(bounded[0].assistant.len(), MAX_HISTORY_MESSAGE_BYTES);
        // Three 8 KiB exchanges fill 24 KiB exactly; the fourth is not taken.
        let full = (0..4).rev().map(|index| exchange(index, 4 * 1_024));
        let selected = select_history(full);
        assert_eq!(
            selected
                .iter()
                .map(|e| e.turn_id.as_str())
                .collect::<Vec<_>>(),
            ["turn_1", "turn_2", "turn_3"]
        );
    }
}
