use ditto_model::{
    FailureKind, FinishReason, MAX_TOOL_ARGUMENT_BYTES, ModelEvent, ModelEventStream,
    ModelStreamEvent, ProviderCallId, ProviderWarning, ReasoningItem, ReasoningItemId,
    ReasoningSegment, ReasoningSegmentKey, ReasoningTextKind,
};
use futures_util::{StreamExt, stream};
use serde_json::json;

fn call_id(value: &str) -> ProviderCallId {
    ProviderCallId::new(value).expect("valid provider call id")
}

fn reasoning_id(value: &str) -> ReasoningItemId {
    ReasoningItemId::new(value).expect("valid reasoning item id")
}

const fn reasoning_key(kind: ReasoningTextKind, index: u32) -> ReasoningSegmentKey {
    ReasoningSegmentKey { kind, index }
}

fn reasoning_item(
    id: ReasoningItemId,
    segments: impl IntoIterator<Item = (ReasoningSegmentKey, &'static str)>,
) -> ReasoningItem {
    ReasoningItem {
        id,
        segments: segments
            .into_iter()
            .map(|(key, text)| ReasoningSegment {
                key,
                text: text.into(),
            })
            .collect(),
        state: None,
    }
}

async fn collect(events: Vec<ModelEvent>) -> Vec<ModelStreamEvent> {
    ModelEventStream::new(stream::iter(events)).collect().await
}

fn assert_single_protocol_failure(events: &[ModelStreamEvent]) {
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event.event, ModelEvent::Failed { .. }))
            .count(),
        1
    );
    assert!(matches!(
        events.last().map(|event| &event.event),
        Some(ModelEvent::Failed { failure }) if failure.kind == FailureKind::Protocol
    ));
    assert!(
        !events
            .iter()
            .any(|event| matches!(event.event, ModelEvent::Completed { .. }))
    );
}

#[tokio::test]
async fn wrapper_assigns_sequences_to_a_valid_tool_lifecycle() {
    let call_id = call_id("sequence-call");
    let events = collect(vec![
        ModelEvent::TextDelta {
            text: "before".into(),
        },
        ModelEvent::ToolCallStarted {
            call_id: call_id.clone(),
            capability_id: "artifact.read".into(),
        },
        ModelEvent::ToolCallArgumentDelta {
            call_id: call_id.clone(),
            delta: r#"{"reference":"sha256:"#.into(),
        },
        ModelEvent::ToolCallArgumentDelta {
            call_id: call_id.clone(),
            delta: r#"abc"}"#.into(),
        },
        ModelEvent::ToolCallReady {
            call_id,
            capability_id: "artifact.read".into(),
            arguments: json!({"reference":"sha256:abc"}),
        },
        ModelEvent::Completed {
            finish_reason: FinishReason::ToolCalls,
            continuation: None,
        },
    ])
    .await;

    assert_eq!(
        events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        [0, 1, 2, 3, 4, 5]
    );
    assert!(events.iter().all(|event| event.validate().is_ok()));
    assert!(matches!(
        events.last().map(|event| &event.event),
        Some(ModelEvent::Completed { .. })
    ));
}

#[tokio::test]
async fn wrapper_accumulates_and_validates_a_reasoning_item_lifecycle() {
    let item_id = reasoning_id("reasoning-sequence");
    let summary = reasoning_key(ReasoningTextKind::Summary, 0);
    let provider_reasoning = reasoning_key(ReasoningTextKind::ProviderReasoning, 0);
    let events = collect(vec![
        ModelEvent::ReasoningItemStarted {
            item_id: item_id.clone(),
        },
        ModelEvent::ReasoningDelta {
            item_id: item_id.clone(),
            segment: summary,
            delta: "first ".into(),
        },
        ModelEvent::ReasoningDelta {
            item_id: item_id.clone(),
            segment: provider_reasoning,
            delta: "private".into(),
        },
        ModelEvent::ReasoningDelta {
            item_id: item_id.clone(),
            segment: summary,
            delta: "second".into(),
        },
        ModelEvent::ReasoningItemReady {
            item: reasoning_item(
                item_id,
                [(summary, "first second"), (provider_reasoning, "private")],
            ),
        },
        ModelEvent::Completed {
            finish_reason: FinishReason::EndTurn,
            continuation: None,
        },
    ])
    .await;

    assert_eq!(
        events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        [0, 1, 2, 3, 4, 5]
    );
    assert!(matches!(
        events.last().map(|event| &event.event),
        Some(ModelEvent::Completed { .. })
    ));
}

#[tokio::test]
async fn wrapper_accepts_a_non_unpin_raw_stream() {
    let raw = async_stream::stream! {
        yield ModelEvent::TextDelta { text: "raw".into() };
        yield ModelEvent::Completed {
            finish_reason: FinishReason::EndTurn,
            continuation: None,
        };
    };

    let events = ModelEventStream::new(raw).collect::<Vec<_>>().await;
    assert_eq!(events.len(), 2);
    assert_eq!((events[0].sequence, events[1].sequence), (0, 1));
}

#[tokio::test]
async fn post_terminal_raw_events_are_never_exposed() {
    let events = collect(vec![
        ModelEvent::Completed {
            finish_reason: FinishReason::EndTurn,
            continuation: None,
        },
        ModelEvent::TextDelta {
            text: "must remain hidden".into(),
        },
        ModelEvent::Failed {
            failure: ditto_model::ModelFailure::new(
                FailureKind::Provider,
                "also hidden after the first terminal",
            ),
        },
    ])
    .await;

    assert_eq!(events.len(), 1);
    assert_eq!(events[0].sequence, 0);
    assert!(matches!(events[0].event, ModelEvent::Completed { .. }));
}

#[tokio::test]
async fn raw_transport_closure_synthesizes_exactly_one_terminal_failure() {
    let mut stream = ModelEventStream::new(stream::iter(vec![ModelEvent::TextDelta {
        text: "truncated".into(),
    }]));

    let first = stream.next().await.expect("raw text event");
    let terminal = stream.next().await.expect("synthesized terminal failure");
    assert_eq!((first.sequence, terminal.sequence), (0, 1));
    assert!(matches!(first.event, ModelEvent::TextDelta { .. }));
    assert!(matches!(
        terminal.event,
        ModelEvent::Failed { failure }
            if failure.kind == FailureKind::Protocol
                && failure.message.contains("without a terminal event")
    ));
    assert!(stream.next().await.is_none());
    assert!(stream.next().await.is_none());
}

#[tokio::test]
async fn raw_closure_with_an_active_call_is_a_terminal_protocol_failure() {
    let events = collect(vec![ModelEvent::ToolCallStarted {
        call_id: call_id("truncated-call"),
        capability_id: "artifact.read".into(),
    }])
    .await;

    assert_eq!(events.len(), 2);
    assert_eq!(events[1].sequence, 1);
    assert_single_protocol_failure(&events);
    assert!(matches!(
        &events[1].event,
        ModelEvent::Failed { failure }
            if failure.message.contains("unfinished tool calls")
    ));
}

#[tokio::test]
async fn invalid_event_is_replaced_by_one_valid_terminal_failure() {
    let events = collect(vec![
        ModelEvent::ProviderWarning {
            warning: ProviderWarning {
                code: None,
                message: "   ".into(),
            },
        },
        ModelEvent::Completed {
            finish_reason: FinishReason::EndTurn,
            continuation: None,
        },
    ])
    .await;

    assert_eq!(events.len(), 1);
    assert_single_protocol_failure(&events);
    events[0]
        .validate()
        .expect("replacement terminal is itself valid");
}

#[tokio::test]
async fn deltas_and_ready_events_require_a_started_active_call() {
    for invalid in [
        ModelEvent::ToolCallArgumentDelta {
            call_id: call_id("delta-before-start"),
            delta: "{}".into(),
        },
        ModelEvent::ToolCallReady {
            call_id: call_id("ready-before-start"),
            capability_id: "artifact.read".into(),
            arguments: json!({}),
        },
    ] {
        let events = collect(vec![
            invalid,
            ModelEvent::Completed {
                finish_reason: FinishReason::EndTurn,
                continuation: None,
            },
        ])
        .await;

        assert_eq!(events.len(), 1);
        assert_single_protocol_failure(&events);
        assert!(matches!(
            &events[0].event,
            ModelEvent::Failed { failure } if failure.call_id.is_some()
        ));
    }
}

#[tokio::test]
async fn duplicate_start_and_duplicate_ready_are_terminal_protocol_failures() {
    let duplicate_start = call_id("duplicate-start");
    let start_events = collect(vec![
        ModelEvent::ToolCallStarted {
            call_id: duplicate_start.clone(),
            capability_id: "artifact.read".into(),
        },
        ModelEvent::ToolCallStarted {
            call_id: duplicate_start,
            capability_id: "artifact.read".into(),
        },
        ModelEvent::Completed {
            finish_reason: FinishReason::ToolCalls,
            continuation: None,
        },
    ])
    .await;
    assert_eq!(start_events.len(), 2);
    assert_single_protocol_failure(&start_events);

    let duplicate_ready = call_id("duplicate-ready");
    let ready_events = collect(vec![
        ModelEvent::ToolCallStarted {
            call_id: duplicate_ready.clone(),
            capability_id: "artifact.read".into(),
        },
        ModelEvent::ToolCallArgumentDelta {
            call_id: duplicate_ready.clone(),
            delta: "{}".into(),
        },
        ModelEvent::ToolCallReady {
            call_id: duplicate_ready.clone(),
            capability_id: "artifact.read".into(),
            arguments: json!({}),
        },
        ModelEvent::ToolCallReady {
            call_id: duplicate_ready,
            capability_id: "artifact.read".into(),
            arguments: json!({}),
        },
        ModelEvent::Completed {
            finish_reason: FinishReason::ToolCalls,
            continuation: None,
        },
    ])
    .await;
    assert_eq!(ready_events.len(), 4);
    assert_single_protocol_failure(&ready_events);
}

#[tokio::test]
async fn argument_delta_after_ready_is_a_terminal_protocol_failure() {
    let call_id = call_id("delta-after-ready");
    let events = collect(vec![
        ModelEvent::ToolCallStarted {
            call_id: call_id.clone(),
            capability_id: "artifact.read".into(),
        },
        ModelEvent::ToolCallArgumentDelta {
            call_id: call_id.clone(),
            delta: "{}".into(),
        },
        ModelEvent::ToolCallReady {
            call_id: call_id.clone(),
            capability_id: "artifact.read".into(),
            arguments: json!({}),
        },
        ModelEvent::ToolCallArgumentDelta {
            call_id,
            delta: "{}".into(),
        },
    ])
    .await;

    assert_eq!(events.len(), 4);
    assert_single_protocol_failure(&events);
}

#[tokio::test]
async fn completion_is_rejected_while_a_tool_call_is_active() {
    let events = collect(vec![
        ModelEvent::ToolCallStarted {
            call_id: call_id("unfinished-call"),
            capability_id: "artifact.read".into(),
        },
        ModelEvent::Completed {
            finish_reason: FinishReason::ToolCalls,
            continuation: None,
        },
        ModelEvent::TextDelta {
            text: "must remain hidden".into(),
        },
    ])
    .await;

    assert_eq!(events.len(), 2);
    assert_single_protocol_failure(&events);
    assert!(matches!(
        &events[1].event,
        ModelEvent::Failed { failure }
            if failure.message.contains("unfinished tool calls")
    ));
}

#[tokio::test]
async fn ready_capability_must_match_the_started_capability() {
    let call_id = call_id("capability-mismatch");
    let events = collect(vec![
        ModelEvent::ToolCallStarted {
            call_id: call_id.clone(),
            capability_id: "artifact.read".into(),
        },
        ModelEvent::ToolCallArgumentDelta {
            call_id: call_id.clone(),
            delta: "{}".into(),
        },
        ModelEvent::ToolCallReady {
            call_id,
            capability_id: "artifact.write".into(),
            arguments: json!({}),
        },
    ])
    .await;

    assert_eq!(events.len(), 3);
    assert_single_protocol_failure(&events);
}

#[tokio::test]
async fn zero_delta_tool_ready_is_a_malformed_arguments_failure() {
    let call_id = call_id("zero-delta-ready");
    let events = collect(vec![
        ModelEvent::ToolCallStarted {
            call_id: call_id.clone(),
            capability_id: "artifact.read".into(),
        },
        ModelEvent::ToolCallReady {
            call_id,
            capability_id: "artifact.read".into(),
            arguments: json!({}),
        },
        ModelEvent::Completed {
            finish_reason: FinishReason::ToolCalls,
            continuation: None,
        },
    ])
    .await;

    assert_eq!(events.len(), 2);
    assert!(matches!(
        &events[1].event,
        ModelEvent::Failed { failure }
            if failure.kind == FailureKind::MalformedToolArguments
                && failure.call_id.as_ref().is_some_and(|id| id.as_str() == "zero-delta-ready")
    ));
}

#[tokio::test]
async fn malformed_accumulated_tool_json_is_a_typed_terminal_failure() {
    let call_id = call_id("malformed-stream-json");
    let events = collect(vec![
        ModelEvent::ToolCallStarted {
            call_id: call_id.clone(),
            capability_id: "artifact.read".into(),
        },
        ModelEvent::ToolCallArgumentDelta {
            call_id: call_id.clone(),
            delta: r#"{"reference":"#.into(),
        },
        ModelEvent::ToolCallReady {
            call_id,
            capability_id: "artifact.read".into(),
            arguments: json!({}),
        },
    ])
    .await;

    assert_eq!(events.len(), 3);
    assert!(matches!(
        &events[2].event,
        ModelEvent::Failed { failure }
            if failure.kind == FailureKind::MalformedToolArguments
                && failure.call_id.as_ref().is_some_and(|id| id.as_str() == "malformed-stream-json")
    ));
}

#[tokio::test]
async fn oversized_tool_argument_delta_is_a_terminal_protocol_failure() {
    let call_id = call_id("oversized-arguments");
    let events = collect(vec![
        ModelEvent::ToolCallStarted {
            call_id: call_id.clone(),
            capability_id: "artifact.read".into(),
        },
        ModelEvent::ToolCallArgumentDelta {
            call_id,
            delta: "x".repeat(MAX_TOOL_ARGUMENT_BYTES + 1),
        },
    ])
    .await;

    assert_eq!(events.len(), 2);
    assert_single_protocol_failure(&events);
    assert!(matches!(
        &events[1].event,
        ModelEvent::Failed { failure }
            if failure.call_id.as_ref().is_some_and(|id| id.as_str() == "oversized-arguments")
                && failure.message.contains("byte limit")
    ));
}

#[tokio::test]
async fn raw_ready_arguments_must_equal_the_accumulated_json() {
    let call_id = call_id("ready-arguments-mismatch");
    let events = collect(vec![
        ModelEvent::ToolCallStarted {
            call_id: call_id.clone(),
            capability_id: "artifact.read".into(),
        },
        ModelEvent::ToolCallArgumentDelta {
            call_id: call_id.clone(),
            delta: r#"{"reference":"sha256:expected"}"#.into(),
        },
        ModelEvent::ToolCallReady {
            call_id,
            capability_id: "artifact.read".into(),
            arguments: json!({"reference":"sha256:different"}),
        },
    ])
    .await;

    assert_eq!(events.len(), 3);
    assert_single_protocol_failure(&events);
    assert!(matches!(
        &events[2].event,
        ModelEvent::Failed { failure }
            if failure.call_id.as_ref().is_some_and(|id| id.as_str() == "ready-arguments-mismatch")
                && failure.message.contains("do not match")
    ));
}

#[tokio::test]
async fn reasoning_delta_and_ready_require_a_started_item() {
    let delta_events = collect(vec![ModelEvent::ReasoningDelta {
        item_id: reasoning_id("reasoning-delta-before-start"),
        segment: reasoning_key(ReasoningTextKind::Summary, 0),
        delta: "orphan".into(),
    }])
    .await;
    assert_eq!(delta_events.len(), 1);
    assert_single_protocol_failure(&delta_events);

    let ready_events = collect(vec![ModelEvent::ReasoningItemReady {
        item: reasoning_item(
            reasoning_id("reasoning-ready-before-start"),
            [(reasoning_key(ReasoningTextKind::Summary, 0), "orphan")],
        ),
    }])
    .await;
    assert_eq!(ready_events.len(), 1);
    assert_single_protocol_failure(&ready_events);
}

#[tokio::test]
async fn duplicate_reasoning_start_and_ready_are_terminal_protocol_failures() {
    let duplicate_start = reasoning_id("duplicate-reasoning-start");
    let start_events = collect(vec![
        ModelEvent::ReasoningItemStarted {
            item_id: duplicate_start.clone(),
        },
        ModelEvent::ReasoningItemStarted {
            item_id: duplicate_start,
        },
    ])
    .await;
    assert_eq!(start_events.len(), 2);
    assert_single_protocol_failure(&start_events);

    let duplicate_ready = reasoning_id("duplicate-reasoning-ready");
    let key = reasoning_key(ReasoningTextKind::Summary, 0);
    let ready_item = reasoning_item(duplicate_ready.clone(), [(key, "complete")]);
    let ready_events = collect(vec![
        ModelEvent::ReasoningItemStarted {
            item_id: duplicate_ready.clone(),
        },
        ModelEvent::ReasoningDelta {
            item_id: duplicate_ready,
            segment: key,
            delta: "complete".into(),
        },
        ModelEvent::ReasoningItemReady {
            item: ready_item.clone(),
        },
        ModelEvent::ReasoningItemReady { item: ready_item },
    ])
    .await;
    assert_eq!(ready_events.len(), 4);
    assert_single_protocol_failure(&ready_events);
}

#[tokio::test]
async fn reasoning_ready_id_and_segments_must_match_accumulated_deltas() {
    let started_id = reasoning_id("reasoning-ready-match");
    let key = reasoning_key(ReasoningTextKind::Summary, 0);
    let mismatched_text = collect(vec![
        ModelEvent::ReasoningItemStarted {
            item_id: started_id.clone(),
        },
        ModelEvent::ReasoningDelta {
            item_id: started_id.clone(),
            segment: key,
            delta: "expected".into(),
        },
        ModelEvent::ReasoningItemReady {
            item: reasoning_item(started_id, [(key, "different")]),
        },
    ])
    .await;
    assert_eq!(mismatched_text.len(), 3);
    assert_single_protocol_failure(&mismatched_text);

    let started_id = reasoning_id("reasoning-started-id");
    let mismatched_id = collect(vec![
        ModelEvent::ReasoningItemStarted {
            item_id: started_id.clone(),
        },
        ModelEvent::ReasoningDelta {
            item_id: started_id,
            segment: key,
            delta: "expected".into(),
        },
        ModelEvent::ReasoningItemReady {
            item: reasoning_item(reasoning_id("different-ready-id"), [(key, "expected")]),
        },
    ])
    .await;
    assert_eq!(mismatched_id.len(), 3);
    assert_single_protocol_failure(&mismatched_id);
}

#[tokio::test]
async fn duplicate_ready_reasoning_segment_keys_are_rejected() {
    let item_id = reasoning_id("duplicate-reasoning-segment");
    let key = reasoning_key(ReasoningTextKind::Summary, 0);
    let events = collect(vec![
        ModelEvent::ReasoningItemStarted {
            item_id: item_id.clone(),
        },
        ModelEvent::ReasoningDelta {
            item_id: item_id.clone(),
            segment: key,
            delta: "complete".into(),
        },
        ModelEvent::ReasoningItemReady {
            item: reasoning_item(item_id, [(key, "complete"), (key, "complete")]),
        },
    ])
    .await;

    assert_eq!(events.len(), 3);
    assert_single_protocol_failure(&events);
}

#[tokio::test]
async fn completion_and_raw_closure_reject_active_reasoning_items() {
    let completed_events = collect(vec![
        ModelEvent::ReasoningItemStarted {
            item_id: reasoning_id("reasoning-active-at-completion"),
        },
        ModelEvent::Completed {
            finish_reason: FinishReason::EndTurn,
            continuation: None,
        },
    ])
    .await;
    assert_eq!(completed_events.len(), 2);
    assert_single_protocol_failure(&completed_events);
    assert!(matches!(
        &completed_events[1].event,
        ModelEvent::Failed { failure }
            if failure.message.contains("unfinished reasoning items")
    ));

    let closure_events = collect(vec![ModelEvent::ReasoningItemStarted {
        item_id: reasoning_id("reasoning-active-at-closure"),
    }])
    .await;
    assert_eq!(closure_events.len(), 2);
    assert_single_protocol_failure(&closure_events);
    assert!(matches!(
        &closure_events[1].event,
        ModelEvent::Failed { failure }
            if failure.message.contains("unfinished reasoning items")
    ));
}

#[tokio::test]
async fn explicit_failure_may_abort_active_tool_and_reasoning_lifecycles() {
    let events = collect(vec![
        ModelEvent::ToolCallStarted {
            call_id: call_id("aborted-tool"),
            capability_id: "artifact.read".into(),
        },
        ModelEvent::ReasoningItemStarted {
            item_id: reasoning_id("aborted-reasoning"),
        },
        ModelEvent::Failed {
            failure: ditto_model::ModelFailure::new(
                FailureKind::Provider,
                "provider aborted the response",
            ),
        },
        ModelEvent::Completed {
            finish_reason: FinishReason::EndTurn,
            continuation: None,
        },
    ])
    .await;

    assert_eq!(events.len(), 3);
    assert!(matches!(
        &events[2].event,
        ModelEvent::Failed { failure }
            if failure.kind == FailureKind::Provider
                && failure.message == "provider aborted the response"
    ));
}
