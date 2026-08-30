use std::collections::{BTreeMap, BTreeSet};

use chrono::{Duration, Utc};
use ditto_capability::CapabilitySchema;
use ditto_context::ContextCapsule;
use ditto_model::{
    CancellationToken, ContentPart, ContinuationCapability, ContinuationState, ConversationItem,
    DriverDescriptor, DriverId, ExecutionEpochId, FailureKind, FinishReason, FixtureDriver,
    FixtureFrame, MAX_CONTINUATION_BYTES, MAX_CONTINUATION_DEPTH, MAX_PROMPT_CACHE_NAMESPACE_BYTES,
    MAX_REASONING_STATE_BYTES, MIN_REASONING_BUDGET_TOKENS, MessageRole, ModelContractError,
    ModelDriver, ModelEvent, ModelFeature, ModelRequest, ModelRequestId, ModelStreamEvent,
    ModelTurn, OpaqueReasoningState, OutputConstraint, ParallelToolCalls, PromptCacheCapabilities,
    PromptCacheMode, PromptCacheNamespace, PromptCachePolicy, PromptCacheTtlSeconds,
    ProviderCallId, ProviderStateFormat, ProviderWarning, ReasoningCapabilities,
    ReasoningDisclosure, ReasoningEffort, ReasoningItem, ReasoningItemId, ReasoningMode,
    ReasoningModeKind, ReasoningRequest, ReasoningSegment, ReasoningSegmentKey, ReasoningTextKind,
    RequestCapabilities, StableSystemPrefix, SummaryDetail, TokenRange, TokenUsage, ToolChoice,
    ToolChoiceKind, UsageSemantics, UsageUpdate,
};
use futures_util::StreamExt;
use serde_json::{Value, json};

fn call_id(value: &str) -> ProviderCallId {
    ProviderCallId::new(value).expect("valid provider call id")
}

fn conversation_tool_call(id: &str, capability_id: &str) -> ConversationItem {
    ConversationItem::ToolCall {
        call_id: call_id(id),
        capability_id: capability_id.into(),
        arguments: json!({"reference": format!("sha256:{id}")}),
    }
}

fn conversation_tool_result(id: &str) -> ConversationItem {
    ConversationItem::ToolResult {
        call_id: call_id(id),
        content: vec![ContentPart::Text {
            text: format!("result for {id}"),
        }],
        is_error: false,
    }
}

fn reasoning_id(value: &str) -> ReasoningItemId {
    ReasoningItemId::new(value).expect("valid reasoning item id")
}

const fn reasoning_key(kind: ReasoningTextKind, index: u32) -> ReasoningSegmentKey {
    ReasoningSegmentKey { kind, index }
}

fn tool_schema() -> CapabilitySchema {
    CapabilitySchema {
        id: "artifact.read".into(),
        version: "0.1.0".into(),
        summary: "Read a bounded artifact range.".into(),
        input_schema: json!({
            "type":"object",
            "properties":{"reference":{"type":"string"}},
            "required":["reference"],
            "additionalProperties":false
        }),
        output_schema: json!({
            "type":"object",
            "properties":{"content":{"type":"string"}},
            "required":["content"]
        }),
    }
}

fn frontier_request_capabilities() -> RequestCapabilities {
    RequestCapabilities {
        incoming_continuations: BTreeSet::new(),
        reasoning: Some(ReasoningCapabilities {
            modes: BTreeSet::from([
                ReasoningModeKind::Disabled,
                ReasoningModeKind::Adaptive,
                ReasoningModeKind::Manual,
            ]),
            efforts: BTreeSet::from([ReasoningEffort::High]),
            disclosures: BTreeSet::from([
                ReasoningDisclosure::Omitted,
                ReasoningDisclosure::Summary {
                    detail: SummaryDetail::Automatic,
                },
            ]),
            preserves_state: true,
            replays_items: true,
            replay_state_formats: BTreeSet::from([
                ProviderStateFormat::new("openai", "responses-reasoning-encrypted-v1")
                    .expect("valid state format"),
                ProviderStateFormat::new("anthropic", "messages-signature-v1")
                    .expect("valid state format"),
            ]),
            manual_budget_tokens: Some(
                TokenRange::new(MIN_REASONING_BUDGET_TOKENS, 32_768)
                    .expect("valid manual budget range"),
            ),
        }),
        prompt_cache: Some(PromptCacheCapabilities {
            modes: BTreeSet::from([
                PromptCacheMode::Disabled,
                PromptCacheMode::Automatic,
                PromptCacheMode::StablePrefix,
            ]),
            ttl_seconds: BTreeSet::from([
                PromptCacheTtlSeconds::new(300).expect("valid TTL"),
                PromptCacheTtlSeconds::new(3_600).expect("valid TTL"),
            ]),
            supports_namespace: true,
        }),
        tool_choices: BTreeSet::from([
            ToolChoiceKind::Auto,
            ToolChoiceKind::None,
            ToolChoiceKind::Required,
            ToolChoiceKind::Specific,
        ]),
        parallel_tool_calls: BTreeSet::from([ParallelToolCalls::Allow, ParallelToolCalls::Forbid]),
    }
}

fn request(required: impl IntoIterator<Item = ModelFeature>) -> ModelRequest {
    let mut request = ModelRequest::new(
        ModelRequestId::new("request-1").expect("valid request id"),
        ExecutionEpochId::new("epoch-1").expect("valid epoch id"),
        StableSystemPrefix {
            segments: vec![
                "You are Ditto.".into(),
                "Use structured tools directly.".into(),
            ],
        },
        ModelTurn {
            conversation: vec![ditto_model::ConversationItem::Message {
                role: MessageRole::User,
                content: vec![ContentPart::Text {
                    text: "Read the artifact.".into(),
                }],
            }],
            context: ContextCapsule::default(),
            output: OutputConstraint::Text,
        },
    );
    request.features.required.extend(required);
    request
}

fn request_with_tool_schema() -> ModelRequest {
    let mut request = request([ModelFeature::ToolCalls]);
    request.tools.push(tool_schema());
    request
}

fn driver(id: &str, frames: Vec<FixtureFrame>) -> FixtureDriver {
    FixtureDriver::new(DriverId::new(id).expect("valid driver id"), frames).expect("valid fixture")
}

async fn collect(driver: &FixtureDriver, request: ModelRequest) -> Vec<ModelStreamEvent> {
    driver
        .stream(request, CancellationToken::new())
        .collect()
        .await
}

#[tokio::test]
async fn text_only_fixture_emits_ordered_deltas_and_completion() {
    let driver = driver(
        "text-fixture",
        vec![
            FixtureFrame::TextDelta {
                text: "hello ".into(),
            },
            FixtureFrame::TextDelta {
                text: "world".into(),
            },
            FixtureFrame::Completed {
                finish_reason: FinishReason::EndTurn,
                continuation: None,
            },
        ],
    );

    let events = collect(&driver, request([ModelFeature::Text])).await;
    assert_eq!(
        events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        [0, 1, 2]
    );
    assert!(matches!(
        &events[0].event,
        ModelEvent::TextDelta { text } if text == "hello "
    ));
    assert!(matches!(
        &events[1].event,
        ModelEvent::TextDelta { text } if text == "world"
    ));
    assert!(matches!(
        events[2].event,
        ModelEvent::Completed {
            finish_reason: FinishReason::EndTurn,
            continuation: None
        }
    ));
}

#[tokio::test]
async fn tool_fixture_emits_stable_started_deltas_and_ready() {
    let first = call_id("call-first");
    let second = call_id("call-second");
    let driver = driver(
        "tool-fixture",
        vec![
            FixtureFrame::ToolCallStarted {
                call_id: first.clone(),
                capability_id: "artifact.read".into(),
            },
            FixtureFrame::ToolCallStarted {
                call_id: second.clone(),
                capability_id: "artifact.read".into(),
            },
            FixtureFrame::ToolCallArgumentDelta {
                call_id: first.clone(),
                delta: r#"{"reference":"sha256:first""#.into(),
            },
            FixtureFrame::ToolCallArgumentDelta {
                call_id: second.clone(),
                delta: r#"{"reference":"sha256:second"}"#.into(),
            },
            FixtureFrame::ToolCallArgumentDelta {
                call_id: first.clone(),
                delta: "}".into(),
            },
            FixtureFrame::ToolCallReady {
                call_id: second.clone(),
            },
            FixtureFrame::ToolCallReady {
                call_id: first.clone(),
            },
            FixtureFrame::Completed {
                finish_reason: FinishReason::ToolCalls,
                continuation: None,
            },
        ],
    );
    assert_eq!(
        driver.descriptor().emitted_features,
        BTreeSet::from([ModelFeature::ToolCalls])
    );
    assert_eq!(
        driver.descriptor().request_capabilities,
        RequestCapabilities::default()
    );

    let events = collect(&driver, request([ModelFeature::ToolCalls])).await;
    assert_eq!(events.len(), 8);
    assert!(matches!(
        &events[0].event,
        ModelEvent::ToolCallStarted { call_id, .. } if call_id == &first
    ));
    assert!(matches!(
        &events[5].event,
        ModelEvent::ToolCallReady { call_id, arguments, .. }
            if call_id == &second && arguments["reference"] == "sha256:second"
    ));
    assert!(matches!(
        &events[6].event,
        ModelEvent::ToolCallReady { call_id, arguments, .. }
            if call_id == &first && arguments["reference"] == "sha256:first"
    ));
}

#[tokio::test]
async fn malformed_tool_arguments_emit_typed_failure() {
    let call_id = call_id("malformed-call");
    let driver = driver(
        "malformed-fixture",
        vec![
            FixtureFrame::ToolCallStarted {
                call_id: call_id.clone(),
                capability_id: "artifact.read".into(),
            },
            FixtureFrame::ToolCallArgumentDelta {
                call_id: call_id.clone(),
                delta: r#"{"offset":"#.into(),
            },
            FixtureFrame::ToolCallReady { call_id },
            FixtureFrame::Completed {
                finish_reason: FinishReason::ToolCalls,
                continuation: None,
            },
        ],
    );

    let events = collect(&driver, request([])).await;
    assert_eq!(events.len(), 3);
    assert!(matches!(
        &events[2].event,
        ModelEvent::Failed { failure }
            if failure.kind == FailureKind::MalformedToolArguments
                && failure.call_id.as_ref().is_some_and(|id| id.as_str() == "malformed-call")
    ));
    assert!(
        !events
            .iter()
            .any(|event| matches!(event.event, ModelEvent::Completed { .. }))
    );
}

#[tokio::test]
async fn cancellation_terminates_without_provider_completion() {
    let driver = driver(
        "cancel-fixture",
        vec![
            FixtureFrame::TextDelta {
                text: "first".into(),
            },
            FixtureFrame::TextDelta {
                text: "second".into(),
            },
            FixtureFrame::Completed {
                finish_reason: FinishReason::EndTurn,
                continuation: None,
            },
        ],
    );
    let cancellation = CancellationToken::new();
    let mut stream = driver.stream(request([ModelFeature::Text]), cancellation.clone());
    assert!(matches!(
        stream.next().await.expect("first event").event,
        ModelEvent::TextDelta { .. }
    ));
    cancellation.cancel();

    let terminal = stream.next().await.expect("cancellation event");
    assert!(matches!(
        terminal.event,
        ModelEvent::Failed { failure } if failure.kind == FailureKind::Cancelled
    ));
    assert!(stream.next().await.is_none());
}

#[tokio::test]
async fn cancellation_before_first_poll_emits_only_cancelled_failure() {
    let driver = driver(
        "pre-cancel-fixture",
        vec![FixtureFrame::Completed {
            finish_reason: FinishReason::EndTurn,
            continuation: None,
        }],
    );
    let cancellation = CancellationToken::new();
    cancellation.cancel();

    let events = driver
        .stream(request([]), cancellation)
        .collect::<Vec<_>>()
        .await;
    assert_eq!(events.len(), 1);
    assert!(matches!(
        events[0].event,
        ModelEvent::Failed { ref failure } if failure.kind == FailureKind::Cancelled
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancellation_wait_is_race_free_before_poll_and_during_subscription() {
    let cancelled_before_poll = CancellationToken::new();
    let waiting = cancelled_before_poll.cancelled();
    cancelled_before_poll.cancel();
    tokio::time::timeout(std::time::Duration::from_secs(1), waiting)
        .await
        .expect("a cancellation published before the wait is polled must complete");

    for attempt in 0..256 {
        let cancellation = CancellationToken::new();
        let waiter_token = cancellation.clone();
        let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));
        let waiter_barrier = barrier.clone();
        let waiter = tokio::spawn(async move {
            waiter_barrier.wait().await;
            waiter_token.cancelled().await;
        });

        barrier.wait().await;
        if attempt % 2 == 0 {
            tokio::task::yield_now().await;
        }
        cancellation.cancel();
        tokio::time::timeout(std::time::Duration::from_secs(1), waiter)
            .await
            .expect("concurrent cancellation must not miss the subscription window")
            .expect("cancellation waiter task must not panic");
    }
}

#[tokio::test]
async fn elapsed_deadline_is_a_typed_terminal_failure() {
    let driver = driver(
        "deadline-fixture",
        vec![FixtureFrame::Completed {
            finish_reason: FinishReason::EndTurn,
            continuation: None,
        }],
    );
    let mut request = request([]);
    request.control.deadline = Some(Utc::now() - Duration::seconds(1));

    let events = collect(&driver, request).await;
    assert_eq!(events.len(), 1);
    assert!(matches!(
        events[0].event,
        ModelEvent::Failed { ref failure } if failure.kind == FailureKind::DeadlineExceeded
    ));
}

#[test]
fn usage_and_continuation_survive_serialization_round_trips() {
    let continuation = ContinuationState::new(
        "openai",
        "responses-v1",
        json!({"response_id":"resp_123","reasoning":[{"id":"item_1"}]}),
    )
    .expect("bounded continuation");
    let usage = ModelStreamEvent::new(
        4,
        ModelEvent::UsageUpdate {
            update: UsageUpdate {
                semantics: UsageSemantics::Cumulative,
                usage: TokenUsage {
                    input_tokens: Some(21),
                    output_tokens: Some(8),
                    cached_input_tokens: Some(13),
                    reasoning_tokens: Some(3),
                    total_tokens: Some(29),
                    details: BTreeMap::from([("accepted_prediction_tokens".into(), 2)]),
                },
            },
        },
    );
    let completed = ModelStreamEvent::new(
        5,
        ModelEvent::Completed {
            finish_reason: FinishReason::MaxOutputTokens,
            continuation: Some(continuation),
        },
    );

    for event in [usage, completed] {
        let encoded = serde_json::to_vec(&event).expect("serialize event");
        let decoded: ModelStreamEvent =
            serde_json::from_slice(&encoded).expect("deserialize event");
        assert_eq!(decoded, event);
        decoded.validate().expect("valid round trip");
    }
}

#[test]
fn incoming_continuation_requires_an_exact_provider_format_capability() {
    assert!(matches!(
        ContinuationCapability::new("", "responses-v1"),
        Err(ModelContractError::EmptyIdentifier { .. })
    ));
    assert!(
        serde_json::from_value::<ContinuationCapability>(json!({
            "provider": "openai",
            "format": ""
        }))
        .is_err()
    );

    let mut continued = request([]);
    continued.continuation = Some(
        ContinuationState::new(
            "openai",
            "responses-v1",
            json!({"response_id":"resp_previous"}),
        )
        .expect("valid incoming continuation"),
    );
    continued
        .validate()
        .expect("incoming continuation is independent from emitted features");

    let exact = DriverDescriptor {
        id: DriverId::new("continuation-driver").expect("driver id"),
        request_capabilities: RequestCapabilities {
            incoming_continuations: BTreeSet::from([ContinuationCapability::new(
                "openai",
                "responses-v1",
            )
            .expect("valid continuation capability")]),
            ..RequestCapabilities::default()
        },
        emitted_features: BTreeSet::new(),
    };
    continued
        .validate_against(&exact)
        .expect("exact incoming format is supported without emitted continuation output");
    let exact_wire = serde_json::to_value(&exact.request_capabilities.incoming_continuations)
        .expect("serialize continuation capability");
    let exact_round_trip: BTreeSet<ContinuationCapability> =
        serde_json::from_value(exact_wire).expect("deserialize continuation capability");
    assert_eq!(
        exact_round_trip,
        exact.request_capabilities.incoming_continuations
    );

    for capability in [
        ContinuationCapability::new("anthropic", "responses-v1").expect("valid provider mismatch"),
        ContinuationCapability::new("openai", "responses-v2").expect("valid format mismatch"),
    ] {
        let mismatched = DriverDescriptor {
            id: DriverId::new("mismatched-continuation").expect("driver id"),
            request_capabilities: RequestCapabilities {
                incoming_continuations: BTreeSet::from([capability]),
                ..RequestCapabilities::default()
            },
            emitted_features: BTreeSet::from([ModelFeature::Continuation]),
        };
        assert!(matches!(
            continued.validate_against(&mismatched),
            Err(ModelContractError::UnsupportedContinuation {
                ref provider,
                ref format
            }) if provider == "openai" && format == "responses-v1"
        ));
    }
}

#[tokio::test]
async fn fixture_rejects_incoming_continuation_before_replaying_frames() {
    let emitted_continuation =
        ContinuationState::new("openai", "responses-v1", json!({"response_id":"resp_next"}))
            .expect("valid emitted continuation");
    let driver = driver(
        "continuation-output-only",
        vec![
            FixtureFrame::TextDelta {
                text: "must not leak".into(),
            },
            FixtureFrame::Completed {
                finish_reason: FinishReason::EndTurn,
                continuation: Some(emitted_continuation),
            },
        ],
    );
    assert_eq!(
        driver.descriptor().emitted_features,
        BTreeSet::from([ModelFeature::Text, ModelFeature::Continuation])
    );
    assert!(
        driver
            .descriptor()
            .request_capabilities
            .incoming_continuations
            .is_empty()
    );

    let mut incoming = request([ModelFeature::Text, ModelFeature::Continuation]);
    incoming.continuation = Some(
        ContinuationState::new(
            "openai",
            "responses-v1",
            json!({"response_id":"resp_previous"}),
        )
        .expect("valid incoming continuation"),
    );
    let events = collect(&driver, incoming).await;
    assert_eq!(events.len(), 1);
    assert!(matches!(
        &events[0].event,
        ModelEvent::Failed { failure }
            if failure.kind == FailureKind::UnsupportedFeature
                && failure.message.contains("incoming continuation")
    ));
}

#[tokio::test]
async fn fixture_features_are_derived_from_emitted_frames() {
    let text_driver = driver(
        "feature-fixture",
        vec![
            FixtureFrame::TextDelta {
                text: "only text".into(),
            },
            FixtureFrame::Completed {
                finish_reason: FinishReason::EndTurn,
                continuation: None,
            },
        ],
    );
    assert_eq!(
        text_driver.descriptor().emitted_features,
        BTreeSet::from([ModelFeature::Text])
    );

    let events = collect(&text_driver, request([ModelFeature::Usage])).await;
    assert_eq!(events.len(), 1);
    assert!(matches!(
        events[0].event,
        ModelEvent::Failed { ref failure } if failure.kind == FailureKind::UnsupportedFeature
    ));

    let call_id = call_id("feature-proof-call");
    let unreachable_usage = driver(
        "reachable-feature-fixture",
        vec![
            FixtureFrame::ToolCallStarted {
                call_id: call_id.clone(),
                capability_id: "artifact.read".into(),
            },
            FixtureFrame::ToolCallArgumentDelta {
                call_id: call_id.clone(),
                delta: "{".into(),
            },
            FixtureFrame::ToolCallReady { call_id },
            FixtureFrame::UsageUpdate {
                update: UsageUpdate {
                    semantics: UsageSemantics::Cumulative,
                    usage: TokenUsage {
                        input_tokens: Some(0),
                        ..TokenUsage::default()
                    },
                },
            },
            FixtureFrame::Completed {
                finish_reason: FinishReason::EndTurn,
                continuation: None,
            },
        ],
    );
    assert_eq!(
        unreachable_usage.descriptor().emitted_features,
        BTreeSet::new()
    );

    let cached_usage = driver(
        "cached-usage-fixture",
        vec![
            FixtureFrame::UsageUpdate {
                update: UsageUpdate {
                    semantics: UsageSemantics::Cumulative,
                    usage: TokenUsage {
                        input_tokens: Some(12),
                        cached_input_tokens: Some(8),
                        ..TokenUsage::default()
                    },
                },
            },
            FixtureFrame::Completed {
                finish_reason: FinishReason::EndTurn,
                continuation: None,
            },
        ],
    );
    assert_eq!(
        cached_usage.descriptor().emitted_features,
        BTreeSet::from([ModelFeature::Usage])
    );
    assert_eq!(
        cached_usage.descriptor().request_capabilities,
        RequestCapabilities::default()
    );

    let truncated_reasoning = driver(
        "truncated-reasoning-feature",
        vec![
            FixtureFrame::ReasoningItemStarted {
                item_id: reasoning_id("truncated-reasoning"),
            },
            FixtureFrame::ReasoningDelta {
                item_id: reasoning_id("truncated-reasoning"),
                segment: reasoning_key(ReasoningTextKind::Summary, 0),
                delta: "not ready".into(),
            },
        ],
    );
    assert_eq!(
        truncated_reasoning.descriptor().emitted_features,
        BTreeSet::new()
    );
}

#[tokio::test]
async fn every_stream_event_variant_is_fixture_replayable() {
    let call_id = call_id("all-events-call");
    let reasoning_id = reasoning_id("all-events-reasoning");
    let summary_key = reasoning_key(ReasoningTextKind::Summary, 0);
    let content_key = reasoning_key(ReasoningTextKind::ProviderReasoning, 0);
    let reasoning_state = OpaqueReasoningState::new(
        "anthropic",
        "messages-thinking-signature-v1",
        "signed-state",
    )
    .expect("reasoning state");
    let continuation =
        ContinuationState::new("anthropic", "messages-v1", json!({"message_id":"msg_123"}))
            .expect("continuation");
    let all_events_driver = driver(
        "all-events-fixture",
        vec![
            FixtureFrame::TextDelta {
                text: "partial".into(),
            },
            FixtureFrame::ToolCallStarted {
                call_id: call_id.clone(),
                capability_id: "artifact.read".into(),
            },
            FixtureFrame::ToolCallArgumentDelta {
                call_id: call_id.clone(),
                delta: "{}".into(),
            },
            FixtureFrame::ToolCallReady { call_id },
            FixtureFrame::StructuredOutput {
                value: json!({"ok":true}),
            },
            FixtureFrame::UsageUpdate {
                update: UsageUpdate {
                    semantics: UsageSemantics::Cumulative,
                    usage: TokenUsage {
                        input_tokens: Some(4),
                        output_tokens: Some(2),
                        ..TokenUsage::default()
                    },
                },
            },
            FixtureFrame::ProviderWarning {
                warning: ProviderWarning {
                    code: Some("unknown_event".into()),
                    message: "provider emitted a forward-compatible event".into(),
                },
            },
            FixtureFrame::ReasoningItemStarted {
                item_id: reasoning_id.clone(),
            },
            FixtureFrame::ReasoningDelta {
                item_id: reasoning_id.clone(),
                segment: summary_key,
                delta: "summary".into(),
            },
            FixtureFrame::ReasoningDelta {
                item_id: reasoning_id.clone(),
                segment: content_key,
                delta: "provider detail".into(),
            },
            FixtureFrame::ReasoningItemReady {
                item: ReasoningItem {
                    id: reasoning_id,
                    segments: vec![
                        ReasoningSegment {
                            key: summary_key,
                            text: "summary".into(),
                        },
                        ReasoningSegment {
                            key: content_key,
                            text: "provider detail".into(),
                        },
                    ],
                    state: Some(reasoning_state),
                },
            },
            FixtureFrame::Completed {
                finish_reason: FinishReason::Other("pause_turn".into()),
                continuation: Some(continuation),
            },
        ],
    );
    let mut all_events_request = request([
        ModelFeature::Text,
        ModelFeature::ToolCalls,
        ModelFeature::StructuredOutput,
        ModelFeature::Usage,
        ModelFeature::ProviderWarnings,
        ModelFeature::ReasoningSummary,
        ModelFeature::ReasoningContent,
        ModelFeature::ReasoningState,
        ModelFeature::Continuation,
    ]);
    all_events_request.turn.output = OutputConstraint::Structured {
        name: "artifact_summary".into(),
        schema: json!({"type":"object"}),
        strict: true,
    };
    assert_eq!(
        all_events_driver.descriptor().emitted_features,
        all_events_request.features.required
    );
    assert_eq!(
        all_events_driver.descriptor().request_capabilities,
        RequestCapabilities::default()
    );

    let events = collect(&all_events_driver, all_events_request).await;
    assert_eq!(events.len(), 12);
    assert!(matches!(events[0].event, ModelEvent::TextDelta { .. }));
    assert!(matches!(
        events[1].event,
        ModelEvent::ToolCallStarted { .. }
    ));
    assert!(matches!(
        events[2].event,
        ModelEvent::ToolCallArgumentDelta { .. }
    ));
    assert!(matches!(events[3].event, ModelEvent::ToolCallReady { .. }));
    assert!(matches!(
        events[4].event,
        ModelEvent::StructuredOutput { .. }
    ));
    assert!(matches!(events[5].event, ModelEvent::UsageUpdate { .. }));
    assert!(matches!(
        events[6].event,
        ModelEvent::ProviderWarning { .. }
    ));
    assert!(matches!(
        events[7].event,
        ModelEvent::ReasoningItemStarted { .. }
    ));
    assert!(matches!(events[8].event, ModelEvent::ReasoningDelta { .. }));
    assert!(matches!(events[9].event, ModelEvent::ReasoningDelta { .. }));
    assert!(matches!(
        events[10].event,
        ModelEvent::ReasoningItemReady { .. }
    ));
    assert!(matches!(events[11].event, ModelEvent::Completed { .. }));

    let failed_driver = driver(
        "failure-fixture",
        vec![FixtureFrame::Failed {
            failure: ditto_model::ModelFailure::new(FailureKind::Provider, "provider error"),
        }],
    );
    assert!(matches!(
        collect(&failed_driver, request([])).await[0].event,
        ModelEvent::Failed { .. }
    ));
}

#[test]
fn valued_generation_controls_round_trip_and_require_exact_driver_support() {
    let mut frontier = request([
        ModelFeature::ToolCalls,
        ModelFeature::ReasoningSummary,
        ModelFeature::ReasoningState,
    ]);
    frontier.tools.push(tool_schema());
    frontier.generation.reasoning = Some(ReasoningRequest {
        mode: ReasoningMode::Adaptive,
        effort: Some(ReasoningEffort::High),
        disclosure: ReasoningDisclosure::Summary {
            detail: SummaryDetail::Automatic,
        },
        preserve_state: true,
    });
    frontier.generation.prompt_cache = PromptCachePolicy::StablePrefix {
        namespace: Some(PromptCacheNamespace::new("ditto-task").expect("valid namespace")),
        ttl_seconds: Some(PromptCacheTtlSeconds::new(3_600).expect("valid TTL")),
    };
    frontier.generation.tool_use.choice = ToolChoice::Specific {
        capability_id: "artifact.read".into(),
    };
    frontier.generation.tool_use.parallel_calls = ParallelToolCalls::Forbid;
    frontier.validate().expect("valid valued controls");

    let descriptor = DriverDescriptor {
        id: DriverId::new("frontier-driver").expect("driver id"),
        request_capabilities: frontier_request_capabilities(),
        emitted_features: frontier.features.required.clone(),
    };
    frontier
        .validate_against(&descriptor)
        .expect("driver preserves every explicit value");

    let mut manual = request([ModelFeature::ReasoningSummary]);
    manual.generation.reasoning = Some(ReasoningRequest {
        mode: ReasoningMode::Manual {
            budget_tokens: 4_096,
        },
        effort: Some(ReasoningEffort::High),
        disclosure: ReasoningDisclosure::Summary {
            detail: SummaryDetail::Automatic,
        },
        preserve_state: false,
    });
    manual.validate().expect("valid manual reasoning request");
    let manual_descriptor = DriverDescriptor {
        id: DriverId::new("manual-driver").expect("driver id"),
        request_capabilities: frontier_request_capabilities(),
        emitted_features: BTreeSet::from([ModelFeature::ReasoningSummary]),
    };
    manual
        .validate_against(&manual_descriptor)
        .expect("manual budget is inside the advertised range");

    let encoded = serde_json::to_value(&frontier).expect("serialize request");
    let decoded: ModelRequest =
        serde_json::from_value(encoded.clone()).expect("deserialize request");
    assert_eq!(
        serde_json::to_value(decoded).expect("serialize decoded request"),
        encoded
    );

    let mut wrong_ttl = descriptor.clone();
    wrong_ttl
        .request_capabilities
        .prompt_cache
        .as_mut()
        .expect("cache capabilities")
        .ttl_seconds = BTreeSet::from([PromptCacheTtlSeconds::new(300).expect("valid TTL")]);
    assert!(matches!(
        frontier.validate_against(&wrong_ttl),
        Err(ModelContractError::UnsupportedGenerationControl {
            control: "prompt_cache.ttl_seconds",
            ..
        })
    ));

    let mut wrong_disclosure = descriptor;
    wrong_disclosure
        .request_capabilities
        .reasoning
        .as_mut()
        .expect("reasoning capabilities")
        .disclosures = BTreeSet::from([ReasoningDisclosure::Summary {
        detail: SummaryDetail::Detailed,
    }]);
    assert!(matches!(
        frontier.validate_against(&wrong_disclosure),
        Err(ModelContractError::UnsupportedGenerationControl {
            control: "reasoning.disclosure",
            ..
        })
    ));
}

#[test]
fn generation_control_validation_rejects_ambiguous_or_unsafe_values() {
    assert!(matches!(
        PromptCacheNamespace::new(""),
        Err(ModelContractError::EmptyPromptCacheNamespace)
    ));
    assert!(matches!(
        PromptCacheNamespace::new("x".repeat(MAX_PROMPT_CACHE_NAMESPACE_BYTES + 1)),
        Err(ModelContractError::PromptCacheNamespaceTooLong { .. })
    ));
    assert!(matches!(
        PromptCacheNamespace::new("bad\nnamespace"),
        Err(ModelContractError::InvalidPromptCacheNamespace)
    ));
    assert!(matches!(
        PromptCacheTtlSeconds::new(0),
        Err(ModelContractError::InvalidPromptCacheTtl)
    ));
    PromptCacheTtlSeconds::new(u32::MAX).expect("TTL is provider-neutral and u32-bounded");
    assert!(matches!(
        TokenRange::new(MIN_REASONING_BUDGET_TOKENS - 1, 4_096),
        Err(ModelContractError::InvalidReasoningTokenRange { .. })
    ));
    assert!(matches!(
        TokenRange::new(8_192, 4_096),
        Err(ModelContractError::InvalidReasoningTokenRange { .. })
    ));

    let mut manual = request([]);
    manual.generation.reasoning = Some(ReasoningRequest {
        mode: ReasoningMode::Manual {
            budget_tokens: MIN_REASONING_BUDGET_TOKENS - 1,
        },
        ..ReasoningRequest::default()
    });
    assert!(matches!(
        manual.validate(),
        Err(ModelContractError::InvalidGenerationControl {
            control: "reasoning.mode",
            ..
        })
    ));

    let mut disabled = request([ModelFeature::ReasoningSummary]);
    disabled.generation.reasoning = Some(ReasoningRequest {
        mode: ReasoningMode::Disabled,
        effort: Some(ReasoningEffort::High),
        disclosure: ReasoningDisclosure::Summary {
            detail: SummaryDetail::Automatic,
        },
        preserve_state: false,
    });
    assert!(matches!(
        disabled.validate(),
        Err(ModelContractError::InvalidGenerationControl {
            control: "reasoning",
            ..
        })
    ));

    let mut no_stable_boundary = request([]);
    no_stable_boundary.stable_system_prefix.segments.clear();
    no_stable_boundary.generation.prompt_cache = PromptCachePolicy::StablePrefix {
        namespace: None,
        ttl_seconds: None,
    };
    assert!(matches!(
        no_stable_boundary.validate(),
        Err(ModelContractError::InvalidGenerationControl {
            control: "prompt_cache",
            ..
        })
    ));

    let mut unknown_tool = request([ModelFeature::ToolCalls]);
    unknown_tool.tools.push(tool_schema());
    unknown_tool.generation.tool_use.choice = ToolChoice::Specific {
        capability_id: "artifact.missing".into(),
    };
    assert!(matches!(
        unknown_tool.validate(),
        Err(ModelContractError::InvalidGenerationControl {
            control: "tool_use.choice",
            ..
        })
    ));

    let mut disabled_tools = request([ModelFeature::ToolCalls]);
    disabled_tools.tools.push(tool_schema());
    disabled_tools.generation.tool_use.choice = ToolChoice::None;
    disabled_tools.generation.tool_use.parallel_calls = ParallelToolCalls::Allow;
    assert!(matches!(
        disabled_tools.validate(),
        Err(ModelContractError::InvalidGenerationControl {
            control: "tool_use.parallel_calls",
            ..
        })
    ));

    let mut tool_schemas_without_tool_output = request([]);
    tool_schemas_without_tool_output.tools.push(tool_schema());
    tool_schemas_without_tool_output.generation.tool_use.choice = ToolChoice::None;
    tool_schemas_without_tool_output
        .validate()
        .expect("explicitly disabled tools do not require an emitted tool-call feature");
    let no_tool_output_driver = DriverDescriptor {
        id: DriverId::new("no-tool-output").expect("driver id"),
        request_capabilities: RequestCapabilities {
            tool_choices: BTreeSet::from([ToolChoiceKind::None]),
            ..RequestCapabilities::default()
        },
        emitted_features: BTreeSet::new(),
    };
    tool_schemas_without_tool_output
        .validate_against(&no_tool_output_driver)
        .expect("tool-choice support is independent from emitted tool-call features");
}

#[tokio::test]
async fn fixture_rejects_nondefault_request_controls_before_replaying_frames() {
    let driver = driver(
        "no-request-capabilities",
        vec![
            FixtureFrame::TextDelta {
                text: "must not leak".into(),
            },
            FixtureFrame::Completed {
                finish_reason: FinishReason::EndTurn,
                continuation: None,
            },
        ],
    );
    assert_eq!(
        driver.descriptor().request_capabilities,
        RequestCapabilities::default()
    );

    let mut controlled = request([ModelFeature::Text]);
    controlled.generation.prompt_cache = PromptCachePolicy::Disabled;
    let events = collect(&driver, controlled).await;
    assert_eq!(events.len(), 1);
    assert!(matches!(
        &events[0].event,
        ModelEvent::Failed { failure }
            if failure.kind == FailureKind::UnsupportedFeature
                && failure.message.contains("prompt_cache")
    ));
}

#[test]
fn opaque_reasoning_state_is_bounded_redacted_and_replayable() {
    let secret = "signed-provider-state";
    let state = OpaqueReasoningState::new("openai", "responses-reasoning-encrypted-v1", secret)
        .expect("valid opaque state");
    assert!(!format!("{state:?}").contains(secret));

    let item = ReasoningItem {
        id: reasoning_id("replay-item"),
        segments: vec![ReasoningSegment {
            key: reasoning_key(ReasoningTextKind::Summary, 0),
            text: "summary".into(),
        }],
        state: Some(state),
    };
    item.validate().expect("valid reasoning item");
    let encoded = serde_json::to_vec(&item).expect("serialize reasoning item");
    let decoded: ReasoningItem = serde_json::from_slice(&encoded).expect("deserialize item");
    assert_eq!(decoded, item);

    assert!(matches!(
        OpaqueReasoningState::new("openai", "responses-v1", ""),
        Err(ModelContractError::EmptyReasoningState)
    ));
    assert!(matches!(
        OpaqueReasoningState::new(
            "openai",
            "responses-v1",
            "x".repeat(MAX_REASONING_STATE_BYTES + 1)
        ),
        Err(ModelContractError::ReasoningStateTooLarge { .. })
    ));
}

#[test]
fn opaque_reasoning_replay_requires_explicit_driver_support() {
    let mut request = request([ModelFeature::Text]);
    request
        .turn
        .conversation
        .push(ditto_model::ConversationItem::Reasoning {
            item: ReasoningItem {
                id: reasoning_id("opaque-replay"),
                segments: Vec::new(),
                state: Some(
                    OpaqueReasoningState::new("anthropic", "messages-signature-v1", "signature")
                        .expect("valid state"),
                ),
            },
        });
    request.validate().expect("valid replay request");

    let mut descriptor = DriverDescriptor {
        id: DriverId::new("replay-driver").expect("driver id"),
        request_capabilities: RequestCapabilities::default(),
        emitted_features: BTreeSet::from([ModelFeature::Text]),
    };
    assert!(matches!(
        request.validate_against(&descriptor),
        Err(ModelContractError::UnsupportedGenerationControl {
            control: "reasoning.replay_item",
            ..
        })
    ));

    descriptor.request_capabilities.reasoning = Some(ReasoningCapabilities {
        replays_items: true,
        ..ReasoningCapabilities::default()
    });
    assert!(matches!(
        request.validate_against(&descriptor),
        Err(ModelContractError::UnsupportedReasoningState { ref provider, ref format })
            if provider == "anthropic" && format == "messages-signature-v1"
    ));
    descriptor
        .request_capabilities
        .reasoning
        .as_mut()
        .expect("reasoning capabilities")
        .replay_state_formats
        .insert(
            ProviderStateFormat::new("openai", "responses-reasoning-encrypted-v1")
                .expect("valid mismatched format"),
        );
    assert!(matches!(
        request.validate_against(&descriptor),
        Err(ModelContractError::UnsupportedReasoningState { .. })
    ));
    descriptor
        .request_capabilities
        .reasoning
        .as_mut()
        .expect("reasoning capabilities")
        .replay_state_formats
        .insert(
            ProviderStateFormat::new("anthropic", "messages-signature-v1")
                .expect("valid exact format"),
        );
    request
        .validate_against(&descriptor)
        .expect("driver explicitly supports opaque replay");
}

#[test]
fn no_op_fixture_frames_are_rejected_before_feature_advertisement() {
    let cases = [
        vec![FixtureFrame::TextDelta {
            text: String::new(),
        }],
        vec![FixtureFrame::ToolCallArgumentDelta {
            call_id: call_id("empty-delta"),
            delta: String::new(),
        }],
        vec![FixtureFrame::UsageUpdate {
            update: UsageUpdate {
                semantics: UsageSemantics::Cumulative,
                usage: TokenUsage::default(),
            },
        }],
    ];
    for frames in cases {
        assert!(matches!(
            FixtureDriver::new(DriverId::new("no-op-fixture").expect("driver id"), frames),
            Err(ditto_model::FixtureError::InvalidFrame { .. })
        ));
    }
}

#[test]
fn request_validate_at_rejects_untrusted_deserialized_context() {
    let now = Utc::now();
    let mut wire = serde_json::to_value(request([])).expect("serialize request");
    wire["turn"]["context"]["nodes"] = json!([{
        "id": "model-assertion",
        "kind": "claim",
        "summary": "the model claimed this as user-provided",
        "origin": "model",
        "epistemic": "asserted",
        "scope": "turn",
        "confidence": 1.0
    }]);
    let decoded: ModelRequest = serde_json::from_value(wire).expect("deserialize request");
    assert!(matches!(
        decoded.validate_at(now),
        Err(ModelContractError::InvalidContext { .. })
    ));
}

#[test]
fn continuation_is_bounded_and_debug_redacted() {
    let secret_marker = "signed-reasoning-state-that-must-not-be-logged";
    let continuation =
        ContinuationState::new("openai", "responses-v1", json!({"opaque":secret_marker}))
            .expect("bounded continuation");
    assert!(!format!("{continuation:?}").contains(secret_marker));

    let too_large = ContinuationState::new(
        "openai",
        "responses-v1",
        Value::String("x".repeat(MAX_CONTINUATION_BYTES)),
    );
    assert!(matches!(
        too_large,
        Err(ModelContractError::ContinuationTooLarge { .. })
    ));

    let mut too_deep = Value::Null;
    for _ in 0..MAX_CONTINUATION_DEPTH {
        too_deep = Value::Array(vec![too_deep]);
    }
    assert!(matches!(
        ContinuationState::new("openai", "responses-v1", too_deep),
        Err(ModelContractError::ContinuationTooDeep { .. })
    ));
}

#[test]
fn conversation_tool_history_accepts_interleaved_resolved_calls() {
    let mut request = request_with_tool_schema();
    request.turn.conversation.extend([
        conversation_tool_call("history-first", "artifact.read"),
        conversation_tool_call("history-second", "artifact.read"),
        conversation_tool_result("history-second"),
        ConversationItem::Message {
            role: MessageRole::Assistant,
            content: vec![ContentPart::Text {
                text: "Both results are available.".into(),
            }],
        },
        conversation_tool_result("history-first"),
    ]);

    request
        .validate()
        .expect("interleaved calls resolve by stable provider call id");
}

#[test]
fn conversation_tool_history_rejects_duplicate_or_orphaned_correlations() {
    let mut duplicate_call = request_with_tool_schema();
    duplicate_call.turn.conversation.extend([
        conversation_tool_call("duplicate-history", "artifact.read"),
        conversation_tool_call("duplicate-history", "artifact.read"),
    ]);
    assert!(matches!(
        duplicate_call.validate(),
        Err(ModelContractError::DuplicateConversationToolCall { ref call_id })
            if call_id.as_str() == "duplicate-history"
    ));

    let mut orphan_result = request_with_tool_schema();
    orphan_result
        .turn
        .conversation
        .push(conversation_tool_result("orphan-history"));
    assert!(matches!(
        orphan_result.validate(),
        Err(ModelContractError::OrphanConversationToolResult { ref call_id })
            if call_id.as_str() == "orphan-history"
    ));

    let mut duplicate_result = request_with_tool_schema();
    duplicate_result.turn.conversation.extend([
        conversation_tool_call("duplicate-result", "artifact.read"),
        conversation_tool_result("duplicate-result"),
        conversation_tool_result("duplicate-result"),
    ]);
    assert!(matches!(
        duplicate_result.validate(),
        Err(ModelContractError::DuplicateConversationToolResult { ref call_id })
            if call_id.as_str() == "duplicate-result"
    ));
}

#[test]
fn conversation_tool_history_rejects_unknown_capabilities_and_unresolved_calls() {
    let mut unknown_capability = request_with_tool_schema();
    unknown_capability
        .turn
        .conversation
        .push(conversation_tool_call(
            "unknown-capability",
            "artifact.write",
        ));
    assert!(matches!(
        unknown_capability.validate(),
        Err(ModelContractError::UnknownConversationCapability {
            ref call_id,
            ref capability_id
        }) if call_id.as_str() == "unknown-capability" && capability_id == "artifact.write"
    ));

    let mut unresolved = request_with_tool_schema();
    unresolved.turn.conversation.push(conversation_tool_call(
        "unresolved-history",
        "artifact.read",
    ));
    assert!(matches!(
        unresolved.validate(),
        Err(ModelContractError::UnresolvedConversationToolCall { ref call_id })
            if call_id.as_str() == "unresolved-history"
    ));
}

#[test]
fn request_keeps_stable_prefix_volatile_turn_and_full_schemas_distinct() {
    let mut request = request([ModelFeature::ToolCalls]);
    request.tools.push(tool_schema());
    request.validate().expect("valid model request");

    let wire = serde_json::to_value(&request).expect("serialize request");
    assert_eq!(
        wire["stable_system_prefix"]["segments"][0],
        "You are Ditto."
    );
    assert_eq!(wire["turn"]["conversation"][0]["type"], "message");
    assert_eq!(
        wire["tools"][0]["input_schema"]["properties"]["reference"]["type"],
        "string"
    );
    assert_eq!(wire["ir_version"], 1);
}

#[test]
fn structured_output_uses_the_canonical_draft_2020_12_schema_boundary() {
    let mut malformed = request([ModelFeature::StructuredOutput]);
    malformed.turn.output = OutputConstraint::Structured {
        name: "malformed".into(),
        schema: json!({"type": 42}),
        strict: true,
    };
    assert!(matches!(
        malformed.validate(),
        Err(ModelContractError::InvalidOutputSchema { ref reason })
            if reason.contains("Draft 2020-12")
    ));

    let mut unsupported_dialect = request([ModelFeature::StructuredOutput]);
    unsupported_dialect.turn.output = OutputConstraint::Structured {
        name: "legacy".into(),
        schema: json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object"
        }),
        strict: true,
    };
    assert!(matches!(
        unsupported_dialect.validate(),
        Err(ModelContractError::InvalidOutputSchema { ref reason })
            if reason.contains("unsupported dialect")
    ));

    let mut canonical = request([ModelFeature::StructuredOutput]);
    canonical.turn.output = OutputConstraint::Structured {
        name: "canonical".into(),
        schema: json!({
            "$schema": ditto_capability::JSON_SCHEMA_DRAFT_2020_12_URI,
            "type": "object",
            "properties": {"value": {"type": "string"}}
        }),
        strict: true,
    };
    canonical.validate().expect("canonical schema is accepted");
}

#[test]
fn unsupported_ir_versions_are_rejected_after_deserialization() {
    let mut wire = serde_json::to_value(request([])).expect("serialize request");
    wire["ir_version"] = json!(2);
    let decoded: ModelRequest = serde_json::from_value(wire).expect("decode versioned request");
    assert!(matches!(
        decoded.validate(),
        Err(ModelContractError::UnsupportedVersion {
            found: 2,
            expected: 1
        })
    ));

    let mut event = ModelStreamEvent::new(
        0,
        ModelEvent::Completed {
            finish_reason: FinishReason::EndTurn,
            continuation: None,
        },
    );
    event.ir_version = 2;
    assert!(matches!(
        event.validate(),
        Err(ModelContractError::UnsupportedVersion { found: 2, .. })
    ));
}

#[tokio::test]
async fn out_of_order_tool_delta_is_a_protocol_failure() {
    let active = call_id("active-call");
    let driver = driver(
        "invalid-order-fixture",
        vec![
            FixtureFrame::ToolCallStarted {
                call_id: active,
                capability_id: "artifact.read".into(),
            },
            FixtureFrame::ToolCallArgumentDelta {
                call_id: call_id("missing-call"),
                delta: "{}".into(),
            },
        ],
    );
    let events = collect(&driver, request([])).await;
    assert_eq!(events.len(), 2);
    assert!(matches!(
        events[1].event,
        ModelEvent::Failed { ref failure } if failure.kind == FailureKind::Protocol
    ));
}

#[tokio::test]
async fn duplicate_and_truncated_tool_lifecycles_fail_terminally() {
    let duplicate_call_id = call_id("duplicate-call");
    let duplicate = driver(
        "duplicate-call-fixture",
        vec![
            FixtureFrame::ToolCallStarted {
                call_id: duplicate_call_id.clone(),
                capability_id: "artifact.read".into(),
            },
            FixtureFrame::ToolCallStarted {
                call_id: duplicate_call_id,
                capability_id: "artifact.read".into(),
            },
        ],
    );
    let duplicate_events = collect(&duplicate, request([])).await;
    assert_eq!(duplicate_events.len(), 2);
    assert!(matches!(
        duplicate_events[1].event,
        ModelEvent::Failed { ref failure } if failure.kind == FailureKind::Protocol
    ));

    let truncated = driver(
        "truncated-call-fixture",
        vec![FixtureFrame::ToolCallStarted {
            call_id: call_id("truncated-call"),
            capability_id: "artifact.read".into(),
        }],
    );
    let truncated_events = collect(&truncated, request([])).await;
    assert_eq!(truncated_events.len(), 2);
    assert!(matches!(
        truncated_events[1].event,
        ModelEvent::Failed { ref failure }
            if failure.kind == FailureKind::Protocol
                && failure.message.contains("unfinished tool calls")
    ));
}

#[test]
fn fixture_rejects_frames_after_a_terminal_event() {
    let result = FixtureDriver::new(
        DriverId::new("extra-terminal-fixture").expect("driver id"),
        vec![
            FixtureFrame::Completed {
                finish_reason: FinishReason::EndTurn,
                continuation: None,
            },
            FixtureFrame::TextDelta {
                text: "must not emit".into(),
            },
        ],
    );
    assert!(matches!(
        result,
        Err(ditto_model::FixtureError::FrameAfterTerminal { index: 1 })
    ));
}
