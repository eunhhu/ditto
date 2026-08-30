use std::collections::{BTreeMap, BTreeSet};

use async_stream::stream;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::{
    CancellationToken, ContinuationState, DriverDescriptor, DriverId, FailureKind, FinishReason,
    ModelContractError, ModelDriver, ModelEvent, ModelEventStream, ModelFailure, ModelFeature,
    ModelRequest, ProviderCallId, ProviderWarning, ReasoningItem, ReasoningItemId,
    ReasoningSegmentKey, RequestCapabilities, ToolCallBuffer, UsageUpdate,
};

/// Provider-neutral frames used by the deterministic development driver.
///
/// `ToolCallReady` is deliberately only a lifecycle marker. The fixture driver
/// parses the accumulated argument deltas and emits either a ready event or a
/// typed malformed-argument failure.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FixtureFrame {
    TextDelta {
        text: String,
    },
    ToolCallStarted {
        call_id: ProviderCallId,
        capability_id: String,
    },
    ToolCallArgumentDelta {
        call_id: ProviderCallId,
        delta: String,
    },
    ToolCallReady {
        call_id: ProviderCallId,
    },
    StructuredOutput {
        value: Value,
    },
    UsageUpdate {
        update: UsageUpdate,
    },
    ProviderWarning {
        warning: ProviderWarning,
    },
    ReasoningItemStarted {
        item_id: ReasoningItemId,
    },
    ReasoningDelta {
        item_id: ReasoningItemId,
        segment: ReasoningSegmentKey,
        delta: String,
    },
    ReasoningItemReady {
        item: ReasoningItem,
    },
    Completed {
        finish_reason: FinishReason,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        continuation: Option<ContinuationState>,
    },
    Failed {
        failure: ModelFailure,
    },
}

impl FixtureFrame {
    const fn is_terminal(&self) -> bool {
        matches!(self, Self::Completed { .. } | Self::Failed { .. })
    }

    fn validate(&self) -> Result<(), ModelContractError> {
        let event = match self {
            Self::TextDelta { text } => ModelEvent::TextDelta { text: text.clone() },
            Self::ToolCallStarted {
                call_id,
                capability_id,
            } => ModelEvent::ToolCallStarted {
                call_id: call_id.clone(),
                capability_id: capability_id.clone(),
            },
            Self::ToolCallArgumentDelta { call_id, delta } => ModelEvent::ToolCallArgumentDelta {
                call_id: call_id.clone(),
                delta: delta.clone(),
            },
            Self::ToolCallReady { .. } => return Ok(()),
            Self::StructuredOutput { value } => ModelEvent::StructuredOutput {
                value: value.clone(),
            },
            Self::UsageUpdate { update } => ModelEvent::UsageUpdate {
                update: update.clone(),
            },
            Self::ProviderWarning { warning } => ModelEvent::ProviderWarning {
                warning: warning.clone(),
            },
            Self::ReasoningItemStarted { item_id } => ModelEvent::ReasoningItemStarted {
                item_id: item_id.clone(),
            },
            Self::ReasoningDelta {
                item_id,
                segment,
                delta,
            } => ModelEvent::ReasoningDelta {
                item_id: item_id.clone(),
                segment: *segment,
                delta: delta.clone(),
            },
            Self::ReasoningItemReady { item } => {
                ModelEvent::ReasoningItemReady { item: item.clone() }
            }
            Self::Completed {
                finish_reason,
                continuation,
            } => ModelEvent::Completed {
                finish_reason: finish_reason.clone(),
                continuation: continuation.clone(),
            },
            Self::Failed { failure } => ModelEvent::Failed {
                failure: failure.clone(),
            },
        };
        event.validate()
    }

    fn into_event(self, tool_calls: &mut ToolCallBuffer) -> Result<ModelEvent, ModelFailure> {
        match self {
            Self::TextDelta { text } => Ok(ModelEvent::TextDelta { text }),
            Self::ToolCallStarted {
                call_id,
                capability_id,
            } => tool_calls
                .start(call_id, capability_id)
                .map_err(ModelFailure::from_tool_call_error),
            Self::ToolCallArgumentDelta { call_id, delta } => tool_calls
                .push_arguments(&call_id, delta)
                .map_err(ModelFailure::from_tool_call_error),
            Self::ToolCallReady { call_id } => tool_calls
                .finish(&call_id)
                .map_err(ModelFailure::from_tool_call_error),
            Self::StructuredOutput { value } => Ok(ModelEvent::StructuredOutput { value }),
            Self::UsageUpdate { update } => Ok(ModelEvent::UsageUpdate { update }),
            Self::ProviderWarning { warning } => Ok(ModelEvent::ProviderWarning { warning }),
            Self::ReasoningItemStarted { item_id } => {
                Ok(ModelEvent::ReasoningItemStarted { item_id })
            }
            Self::ReasoningDelta {
                item_id,
                segment,
                delta,
            } => Ok(ModelEvent::ReasoningDelta {
                item_id,
                segment,
                delta,
            }),
            Self::ReasoningItemReady { item } => Ok(ModelEvent::ReasoningItemReady { item }),
            Self::Completed {
                finish_reason,
                continuation,
            } => Ok(ModelEvent::Completed {
                finish_reason,
                continuation,
            }),
            Self::Failed { failure } => Ok(ModelEvent::Failed { failure }),
        }
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum FixtureError {
    #[error("fixture frame {index} follows a terminal frame")]
    FrameAfterTerminal { index: usize },
    #[error("fixture frame {index} is invalid: {reason}")]
    InvalidFrame { index: usize, reason: String },
}

#[derive(Debug, Clone)]
pub struct FixtureDriver {
    descriptor: DriverDescriptor,
    frames: Vec<FixtureFrame>,
}

impl FixtureDriver {
    pub fn new(id: DriverId, frames: Vec<FixtureFrame>) -> Result<Self, FixtureError> {
        let mut terminal_seen = false;
        for (index, frame) in frames.iter().enumerate() {
            if terminal_seen {
                return Err(FixtureError::FrameAfterTerminal { index });
            }
            frame
                .validate()
                .map_err(|error| FixtureError::InvalidFrame {
                    index,
                    reason: error.to_string(),
                })?;
            terminal_seen = frame.is_terminal();
        }
        let emitted_features = derive_emitted_features(&frames);
        Ok(Self {
            descriptor: DriverDescriptor {
                id,
                request_capabilities: RequestCapabilities::default(),
                emitted_features,
            },
            frames,
        })
    }
}

impl ModelDriver for FixtureDriver {
    fn descriptor(&self) -> &DriverDescriptor {
        &self.descriptor
    }

    fn stream(&self, request: ModelRequest, cancellation: CancellationToken) -> ModelEventStream {
        let descriptor = self.descriptor.clone();
        let frames = self.frames.clone();
        let raw = stream! {
            if let Err(error) = request.validate() {
                yield ModelEvent::Failed {
                    failure: ModelFailure::new(FailureKind::Protocol, error.to_string()),
                };
                return;
            }

            if let Err(error) = request.validate_against(&descriptor) {
                yield ModelEvent::Failed {
                    failure: ModelFailure::new(FailureKind::UnsupportedFeature, error.to_string()),
                };
                return;
            }

            let mut tool_calls = ToolCallBuffer::default();
            for frame in frames {
                // Yielding between fixture frames lets tests and callers observe
                // backpressure and cancel between any two semantic events.
                tokio::task::yield_now().await;
                if let Some(failure) = control_failure(&request, &cancellation) {
                    yield ModelEvent::Failed { failure };
                    return;
                }

                let event = match frame.into_event(&mut tool_calls) {
                    Ok(event) => event,
                    Err(failure) => {
                        yield ModelEvent::Failed { failure };
                        return;
                    }
                };

                if let Err(error) = event.validate() {
                    yield ModelEvent::Failed {
                        failure: ModelFailure::new(FailureKind::Protocol, error.to_string()),
                    };
                    return;
                }
                let terminal = event.is_terminal();
                yield event;
                if terminal {
                    return;
                }
            }
        };
        ModelEventStream::new(raw)
    }
}

fn derive_emitted_features(frames: &[FixtureFrame]) -> BTreeSet<ModelFeature> {
    let mut features = BTreeSet::new();
    let mut tool_calls = ToolCallBuffer::default();
    let mut active_reasoning =
        BTreeMap::<ReasoningItemId, BTreeMap<ReasoningSegmentKey, String>>::new();
    let mut ready_reasoning = BTreeSet::new();
    for frame in frames.iter().cloned() {
        let Ok(event) = frame.into_event(&mut tool_calls) else {
            break;
        };
        if event.validate().is_err() {
            break;
        }
        match &event {
            ModelEvent::TextDelta { .. } => {
                features.insert(ModelFeature::Text);
            }
            ModelEvent::ToolCallReady { .. } => {
                features.insert(ModelFeature::ToolCalls);
            }
            ModelEvent::ToolCallStarted { .. } | ModelEvent::ToolCallArgumentDelta { .. } => {}
            ModelEvent::StructuredOutput { .. } => {
                features.insert(ModelFeature::StructuredOutput);
            }
            ModelEvent::UsageUpdate { .. } => {
                features.insert(ModelFeature::Usage);
            }
            ModelEvent::ProviderWarning { .. } => {
                features.insert(ModelFeature::ProviderWarnings);
            }
            ModelEvent::ReasoningItemStarted { item_id } => {
                if ready_reasoning.contains(item_id)
                    || active_reasoning
                        .insert(item_id.clone(), BTreeMap::new())
                        .is_some()
                {
                    break;
                }
            }
            ModelEvent::ReasoningDelta {
                item_id,
                segment,
                delta,
            } => {
                let Some(segments) = active_reasoning.get_mut(item_id) else {
                    break;
                };
                segments.entry(*segment).or_default().push_str(delta);
            }
            ModelEvent::ReasoningItemReady { item } => {
                let Some(accumulated) = active_reasoning.remove(&item.id) else {
                    break;
                };
                if !ready_reasoning.insert(item.id.clone()) {
                    break;
                }
                let ready_segments = item
                    .segments
                    .iter()
                    .map(|segment| (segment.key, segment.text.clone()))
                    .collect::<BTreeMap<_, _>>();
                if accumulated != ready_segments {
                    break;
                }
                for segment in &item.segments {
                    features.insert(match segment.key.kind {
                        crate::ReasoningTextKind::Summary => ModelFeature::ReasoningSummary,
                        crate::ReasoningTextKind::ProviderReasoning => {
                            ModelFeature::ReasoningContent
                        }
                    });
                }
                if item.state.is_some() {
                    features.insert(ModelFeature::ReasoningState);
                }
            }
            ModelEvent::Completed { continuation, .. } => {
                if tool_calls.has_active_calls() || !active_reasoning.is_empty() {
                    break;
                }
                if continuation.is_some() {
                    features.insert(ModelFeature::Continuation);
                }
            }
            ModelEvent::Failed { .. } => {}
        }
        if event.is_terminal() {
            break;
        }
    }
    features
}

fn control_failure(
    request: &ModelRequest,
    cancellation: &CancellationToken,
) -> Option<ModelFailure> {
    if cancellation.is_cancelled() {
        return Some(ModelFailure::new(
            FailureKind::Cancelled,
            "model request was cancelled",
        ));
    }
    if request
        .control
        .deadline
        .as_ref()
        .is_some_and(|deadline| deadline <= &Utc::now())
    {
        return Some(ModelFailure::new(
            FailureKind::DeadlineExceeded,
            "model request deadline elapsed",
        ));
    }
    None
}
