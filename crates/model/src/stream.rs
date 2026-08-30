use std::{
    collections::{BTreeMap, BTreeSet},
    pin::Pin,
    task::{Context, Poll},
};

use futures_core::Stream;

use crate::{
    FailureKind, MAX_TOOL_ARGUMENT_BYTES, ModelEvent, ModelFailure, ModelStreamEvent,
    ProviderCallId, ReasoningItemId, ReasoningSegmentKey,
};

type RawModelEventStream = Pin<Box<dyn Stream<Item = ModelEvent> + Send + 'static>>;

struct ActiveToolCall {
    capability_id: String,
    arguments: String,
}

/// A validated provider event stream with kernel-assigned envelopes.
///
/// Drivers supply raw [`ModelEvent`] values through [`Self::new`]. This wrapper
/// owns sequence assignment, event validation, tool-call and reasoning-item
/// lifecycle validation, and the exactly-once terminal contract. Its private
/// fields prevent drivers from constructing an unchecked stream of
/// [`ModelStreamEvent`] envelopes.
pub struct ModelEventStream {
    raw: RawModelEventStream,
    next_sequence: u64,
    started_calls: BTreeSet<ProviderCallId>,
    active_calls: BTreeMap<ProviderCallId, ActiveToolCall>,
    ready_calls: BTreeSet<ProviderCallId>,
    started_reasoning_items: BTreeSet<ReasoningItemId>,
    active_reasoning_items: BTreeMap<ReasoningItemId, BTreeMap<ReasoningSegmentKey, String>>,
    ready_reasoning_items: BTreeSet<ReasoningItemId>,
    terminated: bool,
}

impl ModelEventStream {
    /// Wraps a raw provider stream in the validated model-stream boundary.
    pub fn new<S>(raw: S) -> Self
    where
        S: Stream<Item = ModelEvent> + Send + 'static,
    {
        Self {
            raw: Box::pin(raw),
            next_sequence: 0,
            started_calls: BTreeSet::new(),
            active_calls: BTreeMap::new(),
            ready_calls: BTreeSet::new(),
            started_reasoning_items: BTreeSet::new(),
            active_reasoning_items: BTreeMap::new(),
            ready_reasoning_items: BTreeSet::new(),
            terminated: false,
        }
    }

    fn validate_lifecycle(&mut self, event: &ModelEvent) -> Result<(), ModelFailure> {
        match event {
            ModelEvent::ToolCallStarted {
                call_id,
                capability_id,
            } => {
                if !self.started_calls.insert(call_id.clone()) {
                    return Err(protocol_failure_for_call(
                        call_id,
                        format!("tool call {call_id} was started more than once"),
                    ));
                }
                self.active_calls.insert(
                    call_id.clone(),
                    ActiveToolCall {
                        capability_id: capability_id.clone(),
                        arguments: String::new(),
                    },
                );
            }
            ModelEvent::ToolCallArgumentDelta { call_id, delta } => {
                let Some(call) = self.active_calls.get_mut(call_id) else {
                    let timing = if self.ready_calls.contains(call_id) {
                        "after it became ready"
                    } else {
                        "before it started"
                    };
                    return Err(protocol_failure_for_call(
                        call_id,
                        format!("tool call {call_id} received argument data {timing}"),
                    ));
                };
                if call.arguments.len().saturating_add(delta.len()) > MAX_TOOL_ARGUMENT_BYTES {
                    return Err(protocol_failure_for_call(
                        call_id,
                        format!(
                            "tool call {call_id} arguments exceed the \
                             {MAX_TOOL_ARGUMENT_BYTES}-byte limit"
                        ),
                    ));
                }
                call.arguments.push_str(delta);
            }
            ModelEvent::ToolCallReady {
                call_id,
                capability_id,
                arguments,
            } => {
                if self.ready_calls.contains(call_id) {
                    return Err(protocol_failure_for_call(
                        call_id,
                        format!("tool call {call_id} became ready more than once"),
                    ));
                }
                let Some(call) = self.active_calls.get(call_id) else {
                    return Err(protocol_failure_for_call(
                        call_id,
                        format!("tool call {call_id} became ready before it started"),
                    ));
                };
                if call.capability_id.as_str() != capability_id.as_str() {
                    return Err(protocol_failure_for_call(
                        call_id,
                        format!(
                            "tool call {call_id} started as capability {} \
                             but became ready as {capability_id}",
                            call.capability_id
                        ),
                    ));
                }
                let parsed_arguments: serde_json::Value = serde_json::from_str(&call.arguments)
                    .map_err(|error| {
                        failure_for_call(
                            FailureKind::MalformedToolArguments,
                            call_id,
                            format!("tool call {call_id} arguments are malformed JSON: {error}"),
                        )
                    })?;
                if &parsed_arguments != arguments {
                    return Err(protocol_failure_for_call(
                        call_id,
                        format!(
                            "tool call {call_id} ready arguments do not match its accumulated JSON"
                        ),
                    ));
                }
                self.active_calls.remove(call_id);
                self.ready_calls.insert(call_id.clone());
            }
            ModelEvent::ReasoningItemStarted { item_id } => {
                if !self.started_reasoning_items.insert(item_id.clone()) {
                    return Err(ModelFailure::new(
                        FailureKind::Protocol,
                        format!("reasoning item {item_id} was started more than once"),
                    ));
                }
                self.active_reasoning_items
                    .insert(item_id.clone(), BTreeMap::new());
            }
            ModelEvent::ReasoningDelta {
                item_id,
                segment,
                delta,
            } => {
                let Some(segments) = self.active_reasoning_items.get_mut(item_id) else {
                    let timing = if self.ready_reasoning_items.contains(item_id) {
                        "after it became ready"
                    } else {
                        "before it started"
                    };
                    return Err(ModelFailure::new(
                        FailureKind::Protocol,
                        format!("reasoning item {item_id} received a delta {timing}"),
                    ));
                };
                segments.entry(*segment).or_default().push_str(delta);
            }
            ModelEvent::ReasoningItemReady { item } => {
                if self.ready_reasoning_items.contains(&item.id) {
                    return Err(ModelFailure::new(
                        FailureKind::Protocol,
                        format!("reasoning item {} became ready more than once", item.id),
                    ));
                }
                let Some(accumulated) = self.active_reasoning_items.get(&item.id) else {
                    return Err(ModelFailure::new(
                        FailureKind::Protocol,
                        format!("reasoning item {} became ready before it started", item.id),
                    ));
                };

                let mut ready_keys = BTreeSet::new();
                let segments_match = item.segments.len() == accumulated.len()
                    && item.segments.iter().all(|segment| {
                        ready_keys.insert(segment.key)
                            && accumulated.get(&segment.key) == Some(&segment.text)
                    });
                if !segments_match {
                    return Err(ModelFailure::new(
                        FailureKind::Protocol,
                        format!(
                            "reasoning item {} ready segments do not match its accumulated deltas",
                            item.id
                        ),
                    ));
                }

                self.active_reasoning_items.remove(&item.id);
                self.ready_reasoning_items.insert(item.id.clone());
            }
            ModelEvent::Completed { .. } => {
                if let Some(failure) = self.unfinished_lifecycle_failure("provider completed") {
                    return Err(failure);
                }
            }
            ModelEvent::TextDelta { .. }
            | ModelEvent::StructuredOutput { .. }
            | ModelEvent::UsageUpdate { .. }
            | ModelEvent::ProviderWarning { .. }
            | ModelEvent::Failed { .. } => {}
        }
        Ok(())
    }

    fn emit(&mut self, event: ModelEvent) -> ModelStreamEvent {
        let sequence = self.next_sequence;
        if event.is_terminal() {
            self.terminated = true;
        } else {
            self.next_sequence = self
                .next_sequence
                .checked_add(1)
                .expect("non-terminal model stream sequence was checked before emission");
        }
        ModelStreamEvent::new(sequence, event)
    }

    fn fail(&mut self, failure: ModelFailure) -> ModelStreamEvent {
        self.emit(ModelEvent::Failed { failure })
    }

    fn closure_failure(&self) -> ModelFailure {
        self.unfinished_lifecycle_failure("provider stream ended")
            .unwrap_or_else(|| {
                ModelFailure::new(
                    FailureKind::Protocol,
                    "provider stream ended without a terminal event",
                )
            })
    }

    fn unfinished_lifecycle_failure(&self, prefix: &str) -> Option<ModelFailure> {
        let active_calls = self.active_calls.len();
        let active_reasoning = self.active_reasoning_items.len();
        if active_calls == 0 && active_reasoning == 0 {
            return None;
        }
        Some(ModelFailure::new(
            FailureKind::Protocol,
            format!(
                "{prefix} with {active_calls} unfinished tool calls and \
                 {active_reasoning} unfinished reasoning items"
            ),
        ))
    }
}

impl Stream for ModelEventStream {
    type Item = ModelStreamEvent;

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.as_mut().get_mut();
        if this.terminated {
            return Poll::Ready(None);
        }

        let event = match this.raw.as_mut().poll_next(context) {
            Poll::Pending => return Poll::Pending,
            Poll::Ready(Some(event)) => event,
            Poll::Ready(None) => {
                let failure = this.closure_failure();
                return Poll::Ready(Some(this.fail(failure)));
            }
        };

        if let Err(error) = event.validate() {
            return Poll::Ready(Some(this.fail(ModelFailure::new(
                FailureKind::Protocol,
                format!("provider emitted an invalid model event: {error}"),
            ))));
        }

        if this.next_sequence == u64::MAX && !event.is_terminal() {
            return Poll::Ready(Some(this.fail(ModelFailure::new(
                FailureKind::Protocol,
                "model stream sequence space was exhausted",
            ))));
        }

        if let Err(failure) = this.validate_lifecycle(&event) {
            return Poll::Ready(Some(this.fail(failure)));
        }

        Poll::Ready(Some(this.emit(event)))
    }
}

fn protocol_failure_for_call(call_id: &ProviderCallId, message: impl Into<String>) -> ModelFailure {
    failure_for_call(FailureKind::Protocol, call_id, message)
}

fn failure_for_call(
    kind: FailureKind,
    call_id: &ProviderCallId,
    message: impl Into<String>,
) -> ModelFailure {
    let mut failure = ModelFailure::new(kind, message);
    failure.call_id = Some(call_id.clone());
    failure
}
