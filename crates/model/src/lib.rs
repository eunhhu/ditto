//! Provider-neutral model request and streaming-event contract.
//!
//! This crate owns model-provider semantics only. A provider stream completing
//! is not Ditto task verification and no task-completion event exists here.

mod fixture;
mod stream;

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
};

use chrono::{DateTime, Utc};
use ditto_capability::{CapabilitySchema, validate_json_schema};
use ditto_context::ContextCapsule;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use thiserror::Error;
use tokio::sync::watch;

pub use fixture::{FixtureDriver, FixtureError, FixtureFrame};
pub use stream::ModelEventStream;

pub const MODEL_IR_VERSION: u16 = 1;
pub const MAX_IDENTIFIER_BYTES: usize = 512;
pub const MAX_CONTINUATION_BYTES: usize = 64 * 1024;
pub const MAX_CONTINUATION_DEPTH: usize = 32;
pub const MAX_TOOL_ARGUMENT_BYTES: usize = 1024 * 1024;
pub const MAX_USAGE_DETAILS: usize = 32;
pub const MAX_PROMPT_CACHE_NAMESPACE_BYTES: usize = 64;
pub const MIN_REASONING_BUDGET_TOKENS: u32 = 1_024;
pub const MAX_REASONING_STATE_BYTES: usize = 64 * 1_024;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ModelContractError {
    #[error("unsupported model IR version {found}; expected {expected}")]
    UnsupportedVersion { found: u16, expected: u16 },
    #[error("{field} is empty")]
    EmptyIdentifier { field: &'static str },
    #[error("{field} exceeds the {maximum}-byte limit")]
    IdentifierTooLong { field: &'static str, maximum: usize },
    #[error("{field} has surrounding whitespace or contains control characters")]
    InvalidIdentifier { field: &'static str },
    #[error("stable system prefix contains an empty segment")]
    EmptySystemSegment,
    #[error("conversation message contains no content")]
    EmptyMessage,
    #[error("duplicate tool schema {capability_id}")]
    DuplicateTool { capability_id: String },
    #[error("invalid schema for capability {capability_id}: {reason}")]
    InvalidToolSchema {
        capability_id: String,
        reason: String,
    },
    #[error("invalid structured output JSON Schema: {reason}")]
    InvalidOutputSchema { reason: String },
    #[error("feature {feature:?} is both required and preferred")]
    DuplicateFeatureRequest { feature: ModelFeature },
    #[error("request content requires feature {feature:?}")]
    MissingRequiredFeature { feature: ModelFeature },
    #[error("invalid generation control {control}: {reason}")]
    InvalidGenerationControl {
        control: &'static str,
        reason: String,
    },
    #[error("driver does not support generation control {control}: {value}")]
    UnsupportedGenerationControl {
        control: &'static str,
        value: String,
    },
    #[error("driver {driver_id} does not emit required features {features:?}")]
    UnsupportedRequiredFeatures {
        driver_id: String,
        features: Vec<ModelFeature>,
    },
    #[error("driver does not support incoming continuation {provider}/{format}")]
    UnsupportedContinuation { provider: String, format: String },
    #[error("driver does not support reasoning replay state {provider}/{format}")]
    UnsupportedReasoningState { provider: String, format: String },
    #[error("model context capsule is invalid: {reason}")]
    InvalidContext { reason: String },
    #[error("usage detail count exceeds the limit of {maximum}")]
    TooManyUsageDetails { maximum: usize },
    #[error("usage detail key {key:?} is invalid")]
    InvalidUsageDetailKey { key: String },
    #[error("continuation state is {actual} bytes, exceeding the {maximum}-byte limit")]
    ContinuationTooLarge { actual: usize, maximum: usize },
    #[error("continuation state nesting depth {actual} exceeds the limit of {maximum}")]
    ContinuationTooDeep { actual: usize, maximum: usize },
    #[error("{field} text is empty")]
    EmptyText { field: &'static str },
    #[error("reasoning item contains no segments or opaque state")]
    EmptyReasoningItem,
    #[error("reasoning item contains duplicate segment {kind:?} at index {index}")]
    DuplicateReasoningSegment { kind: ReasoningTextKind, index: u32 },
    #[error("opaque reasoning state is empty")]
    EmptyReasoningState,
    #[error("opaque reasoning state is {actual} bytes, exceeding the {maximum}-byte limit")]
    ReasoningStateTooLarge { actual: usize, maximum: usize },
    #[error("prompt-cache namespace is empty")]
    EmptyPromptCacheNamespace,
    #[error("prompt-cache namespace exceeds the {maximum}-byte limit")]
    PromptCacheNamespaceTooLong { maximum: usize },
    #[error("prompt-cache namespace contains control characters")]
    InvalidPromptCacheNamespace,
    #[error("prompt-cache TTL must be greater than zero seconds")]
    InvalidPromptCacheTtl,
    #[error(
        "reasoning token range {minimum}..={maximum} is invalid; minimum must be at least {floor}"
    )]
    InvalidReasoningTokenRange {
        minimum: u32,
        maximum: u32,
        floor: u32,
    },
    #[error("usage update contains no counters or details")]
    EmptyUsageUpdate,
    #[error("conversation tool call {call_id} appears more than once")]
    DuplicateConversationToolCall { call_id: ProviderCallId },
    #[error("conversation tool result {call_id} has no preceding tool call")]
    OrphanConversationToolResult { call_id: ProviderCallId },
    #[error("conversation tool result {call_id} appears more than once")]
    DuplicateConversationToolResult { call_id: ProviderCallId },
    #[error("conversation tool call {call_id} has no following result")]
    UnresolvedConversationToolCall { call_id: ProviderCallId },
    #[error("conversation tool call {call_id} references unknown capability {capability_id}")]
    UnknownConversationCapability {
        call_id: ProviderCallId,
        capability_id: String,
    },
}

fn validate_identifier(field: &'static str, value: &str) -> Result<(), ModelContractError> {
    if value.is_empty() {
        return Err(ModelContractError::EmptyIdentifier { field });
    }
    if value.len() > MAX_IDENTIFIER_BYTES {
        return Err(ModelContractError::IdentifierTooLong {
            field,
            maximum: MAX_IDENTIFIER_BYTES,
        });
    }
    if value.trim() != value || value.chars().any(char::is_control) {
        return Err(ModelContractError::InvalidIdentifier { field });
    }
    Ok(())
}

macro_rules! bounded_identifier {
    ($name:ident, $field:literal) => {
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, ModelContractError> {
                let value = value.into();
                validate_identifier($field, &value)?;
                Ok(Self(value))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                self.as_str()
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                Self::new(value).map_err(serde::de::Error::custom)
            }
        }
    };
}

bounded_identifier!(ModelRequestId, "model request id");
bounded_identifier!(ExecutionEpochId, "execution epoch id");
bounded_identifier!(ProviderCallId, "provider call id");
bounded_identifier!(ReasoningItemId, "reasoning item id");
bounded_identifier!(CancellationId, "cancellation id");
bounded_identifier!(DriverId, "driver id");

#[derive(Clone, PartialEq, Serialize)]
#[serde(transparent)]
pub struct OpaqueProviderState(Value);

impl OpaqueProviderState {
    pub fn new(value: Value) -> Result<Self, ModelContractError> {
        let depth = json_depth(&value);
        if depth > MAX_CONTINUATION_DEPTH {
            return Err(ModelContractError::ContinuationTooDeep {
                actual: depth,
                maximum: MAX_CONTINUATION_DEPTH,
            });
        }
        let actual = serde_json::to_vec(&value)
            .expect("serializing serde_json::Value cannot fail")
            .len();
        if actual > MAX_CONTINUATION_BYTES {
            return Err(ModelContractError::ContinuationTooLarge {
                actual,
                maximum: MAX_CONTINUATION_BYTES,
            });
        }
        Ok(Self(value))
    }

    pub fn value(&self) -> &Value {
        &self.0
    }

    pub fn serialized_len(&self) -> usize {
        serde_json::to_vec(&self.0)
            .expect("serializing serde_json::Value cannot fail")
            .len()
    }
}

impl fmt::Debug for OpaqueProviderState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OpaqueProviderState")
            .field("serialized_bytes", &self.serialized_len())
            .field("value", &"<redacted>")
            .finish()
    }
}

impl<'de> Deserialize<'de> for OpaqueProviderState {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

fn json_depth(value: &Value) -> usize {
    let mut deepest = 1;
    let mut pending = vec![(value, 1)];
    while let Some((current, depth)) = pending.pop() {
        deepest = deepest.max(depth);
        match current {
            Value::Array(values) => {
                pending.extend(values.iter().map(|value| (value, depth + 1)));
            }
            Value::Object(values) => {
                pending.extend(values.values().map(|value| (value, depth + 1)));
            }
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
        }
    }
    deepest
}

#[derive(Clone, PartialEq, Serialize)]
pub struct ContinuationState {
    provider: String,
    format: String,
    state: OpaqueProviderState,
}

impl ContinuationState {
    pub fn new(
        provider: impl Into<String>,
        format: impl Into<String>,
        state: Value,
    ) -> Result<Self, ModelContractError> {
        let provider = provider.into();
        let format = format.into();
        validate_identifier("continuation provider", &provider)?;
        validate_identifier("continuation format", &format)?;
        Ok(Self {
            provider,
            format,
            state: OpaqueProviderState::new(state)?,
        })
    }

    pub fn provider(&self) -> &str {
        &self.provider
    }

    pub fn format(&self) -> &str {
        &self.format
    }

    pub fn state(&self) -> &OpaqueProviderState {
        &self.state
    }
}

impl fmt::Debug for ContinuationState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ContinuationState")
            .field("provider", &self.provider)
            .field("format", &self.format)
            .field("state", &self.state)
            .finish()
    }
}

impl<'de> Deserialize<'de> for ContinuationState {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Wire {
            provider: String,
            format: String,
            state: Value,
        }

        let wire = Wire::deserialize(deserializer)?;
        Self::new(wire.provider, wire.format, wire.state).map_err(serde::de::Error::custom)
    }
}

/// An exact provider-owned state format accepted at a replay boundary.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct ProviderStateFormat {
    provider: String,
    format: String,
}

impl ProviderStateFormat {
    pub fn new(
        provider: impl Into<String>,
        format: impl Into<String>,
    ) -> Result<Self, ModelContractError> {
        let provider = provider.into();
        let format = format.into();
        validate_identifier("provider state capability provider", &provider)?;
        validate_identifier("provider state capability format", &format)?;
        Ok(Self { provider, format })
    }

    pub fn provider(&self) -> &str {
        &self.provider
    }

    pub fn format(&self) -> &str {
        &self.format
    }

    fn matches(&self, provider: &str, format: &str) -> bool {
        self.provider == provider && self.format == format
    }
}

impl<'de> Deserialize<'de> for ProviderStateFormat {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Wire {
            provider: String,
            format: String,
        }

        let wire = Wire::deserialize(deserializer)?;
        Self::new(wire.provider, wire.format).map_err(serde::de::Error::custom)
    }
}

/// Exact provider wire format accepted for incoming continuation state.
pub type ContinuationCapability = ProviderStateFormat;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StableSystemPrefix {
    #[serde(default)]
    pub segments: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelFeature {
    Text,
    ToolCalls,
    StructuredOutput,
    Usage,
    Continuation,
    ProviderWarnings,
    ReasoningSummary,
    ReasoningContent,
    ReasoningState,
}

/// Provider request modes for model reasoning.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningMode {
    #[default]
    ProviderDefault,
    Disabled,
    Adaptive,
    Manual {
        budget_tokens: u32,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningModeKind {
    ProviderDefault,
    Disabled,
    Adaptive,
    Manual,
}

impl ReasoningMode {
    const fn kind(self) -> ReasoningModeKind {
        match self {
            Self::ProviderDefault => ReasoningModeKind::ProviderDefault,
            Self::Disabled => ReasoningModeKind::Disabled,
            Self::Adaptive => ReasoningModeKind::Adaptive,
            Self::Manual { .. } => ReasoningModeKind::Manual,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningEffort {
    None,
    Minimal,
    Low,
    Medium,
    High,
    XHigh,
    Max,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SummaryDetail {
    Automatic,
    Concise,
    Detailed,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ReasoningDisclosure {
    #[default]
    Omitted,
    Summary {
        detail: SummaryDetail,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReasoningRequest {
    #[serde(default)]
    pub mode: ReasoningMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<ReasoningEffort>,
    #[serde(default)]
    pub disclosure: ReasoningDisclosure,
    #[serde(default)]
    pub preserve_state: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct PromptCacheNamespace(String);

impl PromptCacheNamespace {
    pub fn new(value: impl Into<String>) -> Result<Self, ModelContractError> {
        let value = value.into();
        if value.is_empty() {
            return Err(ModelContractError::EmptyPromptCacheNamespace);
        }
        if value.len() > MAX_PROMPT_CACHE_NAMESPACE_BYTES {
            return Err(ModelContractError::PromptCacheNamespaceTooLong {
                maximum: MAX_PROMPT_CACHE_NAMESPACE_BYTES,
            });
        }
        if value.chars().any(char::is_control) {
            return Err(ModelContractError::InvalidPromptCacheNamespace);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for PromptCacheNamespace {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct PromptCacheTtlSeconds(u32);

impl PromptCacheTtlSeconds {
    pub fn new(seconds: u32) -> Result<Self, ModelContractError> {
        if seconds == 0 {
            return Err(ModelContractError::InvalidPromptCacheTtl);
        }
        Ok(Self(seconds))
    }

    pub const fn get(self) -> u32 {
        self.0
    }
}

impl<'de> Deserialize<'de> for PromptCacheTtlSeconds {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(u32::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptCacheMode {
    Disabled,
    Automatic,
    StablePrefix,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PromptCachePolicy {
    #[default]
    ProviderDefault,
    Disabled,
    Automatic {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        namespace: Option<PromptCacheNamespace>,
    },
    StablePrefix {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        namespace: Option<PromptCacheNamespace>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ttl_seconds: Option<PromptCacheTtlSeconds>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolChoiceKind {
    Auto,
    None,
    Required,
    Specific,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolChoice {
    #[default]
    ProviderDefault,
    None,
    Auto,
    Required,
    Specific {
        capability_id: String,
    },
}

impl ToolChoice {
    const fn kind(&self) -> Option<ToolChoiceKind> {
        match self {
            Self::ProviderDefault => None,
            Self::None => Some(ToolChoiceKind::None),
            Self::Auto => Some(ToolChoiceKind::Auto),
            Self::Required => Some(ToolChoiceKind::Required),
            Self::Specific { .. } => Some(ToolChoiceKind::Specific),
        }
    }
}

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum ParallelToolCalls {
    #[default]
    ProviderDefault,
    Allow,
    Forbid,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolUsePolicy {
    #[serde(default)]
    pub choice: ToolChoice,
    #[serde(default)]
    pub parallel_calls: ParallelToolCalls,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GenerationControls {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<ReasoningRequest>,
    #[serde(default)]
    pub prompt_cache: PromptCachePolicy,
    #[serde(default)]
    pub tool_use: ToolUsePolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct TokenRange {
    minimum: u32,
    maximum: u32,
}

impl TokenRange {
    pub fn new(minimum: u32, maximum: u32) -> Result<Self, ModelContractError> {
        if minimum < MIN_REASONING_BUDGET_TOKENS || minimum > maximum {
            return Err(ModelContractError::InvalidReasoningTokenRange {
                minimum,
                maximum,
                floor: MIN_REASONING_BUDGET_TOKENS,
            });
        }
        Ok(Self { minimum, maximum })
    }

    pub const fn contains(self, value: u32) -> bool {
        self.minimum <= value && value <= self.maximum
    }

    pub const fn minimum(self) -> u32 {
        self.minimum
    }

    pub const fn maximum(self) -> u32 {
        self.maximum
    }
}

impl<'de> Deserialize<'de> for TokenRange {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Wire {
            minimum: u32,
            maximum: u32,
        }

        let wire = Wire::deserialize(deserializer)?;
        Self::new(wire.minimum, wire.maximum).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReasoningCapabilities {
    #[serde(default)]
    pub modes: BTreeSet<ReasoningModeKind>,
    #[serde(default)]
    pub efforts: BTreeSet<ReasoningEffort>,
    #[serde(default)]
    pub disclosures: BTreeSet<ReasoningDisclosure>,
    #[serde(default)]
    pub preserves_state: bool,
    #[serde(default)]
    pub replays_items: bool,
    #[serde(default)]
    pub replay_state_formats: BTreeSet<ProviderStateFormat>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manual_budget_tokens: Option<TokenRange>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptCacheCapabilities {
    #[serde(default)]
    pub modes: BTreeSet<PromptCacheMode>,
    #[serde(default)]
    pub ttl_seconds: BTreeSet<PromptCacheTtlSeconds>,
    #[serde(default)]
    pub supports_namespace: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestCapabilities {
    #[serde(default)]
    pub incoming_continuations: BTreeSet<ContinuationCapability>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<ReasoningCapabilities>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_cache: Option<PromptCacheCapabilities>,
    #[serde(default)]
    pub tool_choices: BTreeSet<ToolChoiceKind>,
    #[serde(default)]
    pub parallel_tool_calls: BTreeSet<ParallelToolCalls>,
}

impl GenerationControls {
    fn validate(
        &self,
        stable_prefix_segments: usize,
        tool_ids: &BTreeSet<&str>,
        required_features: &BTreeSet<ModelFeature>,
    ) -> Result<(), ModelContractError> {
        if let Some(reasoning) = &self.reasoning {
            if let ReasoningMode::Manual { budget_tokens } = reasoning.mode
                && budget_tokens < MIN_REASONING_BUDGET_TOKENS
            {
                return Err(invalid_control(
                    "reasoning.mode",
                    format!("manual budget must be at least {MIN_REASONING_BUDGET_TOKENS} tokens"),
                ));
            }
            if reasoning.mode == ReasoningMode::Disabled
                && (reasoning.effort.is_some()
                    || !matches!(reasoning.disclosure, ReasoningDisclosure::Omitted)
                    || reasoning.preserve_state)
            {
                return Err(invalid_control(
                    "reasoning",
                    "disabled reasoning cannot set effort, disclosure, or preserved state",
                ));
            }
            if matches!(reasoning.disclosure, ReasoningDisclosure::Summary { .. })
                && !required_features.contains(&ModelFeature::ReasoningSummary)
            {
                return Err(ModelContractError::MissingRequiredFeature {
                    feature: ModelFeature::ReasoningSummary,
                });
            }
            if reasoning.preserve_state
                && !required_features.contains(&ModelFeature::ReasoningState)
            {
                return Err(ModelContractError::MissingRequiredFeature {
                    feature: ModelFeature::ReasoningState,
                });
            }
        }

        if matches!(self.prompt_cache, PromptCachePolicy::StablePrefix { .. })
            && stable_prefix_segments == 0
            && tool_ids.is_empty()
        {
            return Err(invalid_control(
                "prompt_cache",
                "stable-prefix caching requires stable system content or tool schemas",
            ));
        }

        match &self.tool_use.choice {
            ToolChoice::ProviderDefault | ToolChoice::None | ToolChoice::Auto => {}
            ToolChoice::Required if tool_ids.is_empty() => {
                return Err(invalid_control(
                    "tool_use.choice",
                    "required tool choice needs at least one tool schema",
                ));
            }
            ToolChoice::Specific { capability_id } => {
                validate_identifier("tool choice capability id", capability_id)?;
                if !tool_ids.contains(capability_id.as_str()) {
                    return Err(invalid_control(
                        "tool_use.choice",
                        format!("unknown capability {capability_id}"),
                    ));
                }
            }
            ToolChoice::Required => {}
        }
        if !matches!(
            self.tool_use.parallel_calls,
            ParallelToolCalls::ProviderDefault
        ) && (tool_ids.is_empty() || matches!(self.tool_use.choice, ToolChoice::None))
        {
            return Err(invalid_control(
                "tool_use.parallel_calls",
                "an explicit parallel policy requires enabled tool schemas",
            ));
        }
        Ok(())
    }

    pub fn validate_against(
        &self,
        capabilities: &RequestCapabilities,
    ) -> Result<(), ModelContractError> {
        if let Some(reasoning) = &self.reasoning {
            let Some(supported) = &capabilities.reasoning else {
                return Err(unsupported_control("reasoning", "reasoning request"));
            };
            if !supported.modes.contains(&reasoning.mode.kind()) {
                return Err(unsupported_control(
                    "reasoning.mode",
                    format!("{:?}", reasoning.mode.kind()),
                ));
            }
            if let ReasoningMode::Manual { budget_tokens } = reasoning.mode
                && !supported
                    .manual_budget_tokens
                    .is_some_and(|range| range.contains(budget_tokens))
            {
                return Err(unsupported_control(
                    "reasoning.mode",
                    format!("manual budget of {budget_tokens} tokens"),
                ));
            }
            if let Some(effort) = reasoning.effort
                && !supported.efforts.contains(&effort)
            {
                return Err(unsupported_control(
                    "reasoning.effort",
                    format!("{effort:?}"),
                ));
            }
            if !supported.disclosures.contains(&reasoning.disclosure) {
                return Err(unsupported_control(
                    "reasoning.disclosure",
                    format!("{:?}", reasoning.disclosure),
                ));
            }
            if reasoning.preserve_state && !supported.preserves_state {
                return Err(unsupported_control("reasoning.preserve_state", "true"));
            }
        }

        let cache_request = match &self.prompt_cache {
            PromptCachePolicy::ProviderDefault => None,
            PromptCachePolicy::Disabled => Some((PromptCacheMode::Disabled, None, None)),
            PromptCachePolicy::Automatic { namespace } => {
                Some((PromptCacheMode::Automatic, namespace.as_ref(), None))
            }
            PromptCachePolicy::StablePrefix {
                namespace,
                ttl_seconds,
            } => Some((
                PromptCacheMode::StablePrefix,
                namespace.as_ref(),
                ttl_seconds.as_ref(),
            )),
        };
        if let Some((mode, namespace, ttl_seconds)) = cache_request {
            let Some(supported) = &capabilities.prompt_cache else {
                return Err(unsupported_control("prompt_cache", format!("{mode:?}")));
            };
            if !supported.modes.contains(&mode) {
                return Err(unsupported_control("prompt_cache", format!("{mode:?}")));
            }
            if namespace.is_some() && !supported.supports_namespace {
                return Err(unsupported_control("prompt_cache.namespace", "namespace"));
            }
            if let Some(ttl_seconds) = ttl_seconds
                && !supported.ttl_seconds.contains(ttl_seconds)
            {
                return Err(unsupported_control(
                    "prompt_cache.ttl_seconds",
                    ttl_seconds.get().to_string(),
                ));
            }
        }

        if let Some(kind) = self.tool_use.choice.kind()
            && !capabilities.tool_choices.contains(&kind)
        {
            return Err(unsupported_control("tool_use.choice", format!("{kind:?}")));
        }
        if !matches!(
            self.tool_use.parallel_calls,
            ParallelToolCalls::ProviderDefault
        ) && !capabilities
            .parallel_tool_calls
            .contains(&self.tool_use.parallel_calls)
        {
            return Err(unsupported_control(
                "tool_use.parallel_calls",
                format!("{:?}", self.tool_use.parallel_calls),
            ));
        }
        Ok(())
    }
}

fn invalid_control(control: &'static str, reason: impl Into<String>) -> ModelContractError {
    ModelContractError::InvalidGenerationControl {
        control,
        reason: reason.into(),
    }
}

fn unsupported_control(control: &'static str, value: impl Into<String>) -> ModelContractError {
    ModelContractError::UnsupportedGenerationControl {
        control,
        value: value.into(),
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeatureRequest {
    #[serde(default)]
    pub required: BTreeSet<ModelFeature>,
    #[serde(default)]
    pub preferred: BTreeSet<ModelFeature>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageRole {
    User,
    Assistant,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentPart {
    Text { text: String },
    Structured { value: Value },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningTextKind {
    Summary,
    ProviderReasoning,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ReasoningSegmentKey {
    pub kind: ReasoningTextKind,
    pub index: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReasoningSegment {
    pub key: ReasoningSegmentKey,
    pub text: String,
}

/// Signed or encrypted provider state needed to replay a reasoning item.
///
/// The payload is bounded at construction/deserialization and deliberately
/// omitted from `Debug` output so signatures cannot leak through diagnostics.
#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct OpaqueReasoningState {
    provider: String,
    format: String,
    data: String,
}

impl OpaqueReasoningState {
    pub fn new(
        provider: impl Into<String>,
        format: impl Into<String>,
        data: impl Into<String>,
    ) -> Result<Self, ModelContractError> {
        let provider = provider.into();
        let format = format.into();
        let data = data.into();
        validate_identifier("reasoning state provider", &provider)?;
        validate_identifier("reasoning state format", &format)?;
        if data.is_empty() {
            return Err(ModelContractError::EmptyReasoningState);
        }
        if data.len() > MAX_REASONING_STATE_BYTES {
            return Err(ModelContractError::ReasoningStateTooLarge {
                actual: data.len(),
                maximum: MAX_REASONING_STATE_BYTES,
            });
        }
        Ok(Self {
            provider,
            format,
            data,
        })
    }

    pub fn provider(&self) -> &str {
        &self.provider
    }

    pub fn format(&self) -> &str {
        &self.format
    }

    pub fn data(&self) -> &str {
        &self.data
    }

    pub fn serialized_len(&self) -> usize {
        self.data.len()
    }
}

impl fmt::Debug for OpaqueReasoningState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OpaqueReasoningState")
            .field("provider", &self.provider)
            .field("format", &self.format)
            .field("serialized_bytes", &self.data.len())
            .field("data", &"<redacted>")
            .finish()
    }
}

impl<'de> Deserialize<'de> for OpaqueReasoningState {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Wire {
            provider: String,
            format: String,
            data: String,
        }

        let wire = Wire::deserialize(deserializer)?;
        Self::new(wire.provider, wire.format, wire.data).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReasoningItem {
    pub id: ReasoningItemId,
    #[serde(default)]
    pub segments: Vec<ReasoningSegment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<OpaqueReasoningState>,
}

impl ReasoningItem {
    pub fn validate(&self) -> Result<(), ModelContractError> {
        if self.segments.is_empty() && self.state.is_none() {
            return Err(ModelContractError::EmptyReasoningItem);
        }
        let mut keys = BTreeSet::new();
        for segment in &self.segments {
            if segment.text.is_empty() {
                return Err(ModelContractError::EmptyText {
                    field: "reasoning segment",
                });
            }
            if !keys.insert(segment.key) {
                return Err(ModelContractError::DuplicateReasoningSegment {
                    kind: segment.key.kind,
                    index: segment.key.index,
                });
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ConversationItem {
    Message {
        role: MessageRole,
        content: Vec<ContentPart>,
    },
    ToolCall {
        call_id: ProviderCallId,
        capability_id: String,
        arguments: Value,
    },
    ToolResult {
        call_id: ProviderCallId,
        content: Vec<ContentPart>,
        is_error: bool,
    },
    Reasoning {
        item: ReasoningItem,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ModelTurn {
    #[serde(default)]
    pub conversation: Vec<ConversationItem>,
    #[serde(default)]
    pub context: ContextCapsule,
    #[serde(default)]
    pub output: OutputConstraint,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OutputConstraint {
    #[default]
    Text,
    Structured {
        name: String,
        schema: Value,
        #[serde(default)]
        strict: bool,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestControl {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancellation_id: Option<CancellationId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelRequest {
    pub ir_version: u16,
    pub request_id: ModelRequestId,
    pub execution_epoch_id: ExecutionEpochId,
    pub stable_system_prefix: StableSystemPrefix,
    pub turn: ModelTurn,
    #[serde(default)]
    pub tools: Vec<CapabilitySchema>,
    #[serde(default)]
    pub features: FeatureRequest,
    #[serde(default)]
    pub generation: GenerationControls,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continuation: Option<ContinuationState>,
    #[serde(default)]
    pub control: RequestControl,
}

impl ModelRequest {
    pub fn new(
        request_id: ModelRequestId,
        execution_epoch_id: ExecutionEpochId,
        stable_system_prefix: StableSystemPrefix,
        turn: ModelTurn,
    ) -> Self {
        Self {
            ir_version: MODEL_IR_VERSION,
            request_id,
            execution_epoch_id,
            stable_system_prefix,
            turn,
            tools: Vec::new(),
            features: FeatureRequest::default(),
            generation: GenerationControls::default(),
            continuation: None,
            control: RequestControl::default(),
        }
    }

    pub fn validate(&self) -> Result<(), ModelContractError> {
        self.validate_at(Utc::now())
    }

    /// Validate the request and its model-facing context at a deterministic
    /// wall-clock instant.
    pub fn validate_at(&self, now: DateTime<Utc>) -> Result<(), ModelContractError> {
        if self.ir_version != MODEL_IR_VERSION {
            return Err(ModelContractError::UnsupportedVersion {
                found: self.ir_version,
                expected: MODEL_IR_VERSION,
            });
        }
        if self
            .stable_system_prefix
            .segments
            .iter()
            .any(|segment| segment.trim().is_empty())
        {
            return Err(ModelContractError::EmptySystemSegment);
        }
        self.turn
            .context
            .validate_at(now)
            .map_err(|error| ModelContractError::InvalidContext {
                reason: error.to_string(),
            })?;

        for item in &self.turn.conversation {
            match item {
                ConversationItem::Message { content, .. } if content.is_empty() => {
                    return Err(ModelContractError::EmptyMessage);
                }
                ConversationItem::ToolCall { capability_id, .. } => {
                    validate_identifier("conversation capability id", capability_id)?;
                }
                ConversationItem::Reasoning { item } => item.validate()?,
                ConversationItem::Message { .. } | ConversationItem::ToolResult { .. } => {}
            }
        }

        let mut tool_ids = BTreeSet::new();
        for tool in &self.tools {
            tool.validate()
                .map_err(|error| ModelContractError::InvalidToolSchema {
                    capability_id: tool.id.clone(),
                    reason: error.to_string(),
                })?;
            if !tool_ids.insert(tool.id.as_str()) {
                return Err(ModelContractError::DuplicateTool {
                    capability_id: tool.id.clone(),
                });
            }
        }

        let mut conversation_calls = BTreeMap::<ProviderCallId, bool>::new();
        for item in &self.turn.conversation {
            match item {
                ConversationItem::ToolCall {
                    call_id,
                    capability_id,
                    ..
                } => {
                    if !tool_ids.contains(capability_id.as_str()) {
                        return Err(ModelContractError::UnknownConversationCapability {
                            call_id: call_id.clone(),
                            capability_id: capability_id.clone(),
                        });
                    }
                    if conversation_calls.insert(call_id.clone(), false).is_some() {
                        return Err(ModelContractError::DuplicateConversationToolCall {
                            call_id: call_id.clone(),
                        });
                    }
                }
                ConversationItem::ToolResult { call_id, .. } => {
                    let Some(resolved) = conversation_calls.get_mut(call_id) else {
                        return Err(ModelContractError::OrphanConversationToolResult {
                            call_id: call_id.clone(),
                        });
                    };
                    if *resolved {
                        return Err(ModelContractError::DuplicateConversationToolResult {
                            call_id: call_id.clone(),
                        });
                    }
                    *resolved = true;
                }
                ConversationItem::Message { .. } | ConversationItem::Reasoning { .. } => {}
            }
        }
        if let Some((call_id, _)) = conversation_calls.iter().find(|(_, resolved)| !**resolved) {
            return Err(ModelContractError::UnresolvedConversationToolCall {
                call_id: call_id.clone(),
            });
        }

        if let OutputConstraint::Structured { name, schema, .. } = &self.turn.output {
            validate_identifier("structured output name", name)?;
            validate_json_schema(schema).map_err(|error| {
                ModelContractError::InvalidOutputSchema {
                    reason: error.to_string(),
                }
            })?;
        }

        if let Some(feature) = self
            .features
            .required
            .intersection(&self.features.preferred)
            .next()
        {
            return Err(ModelContractError::DuplicateFeatureRequest { feature: *feature });
        }
        if !self.tools.is_empty()
            && !matches!(self.generation.tool_use.choice, ToolChoice::None)
            && !self.features.required.contains(&ModelFeature::ToolCalls)
        {
            return Err(ModelContractError::MissingRequiredFeature {
                feature: ModelFeature::ToolCalls,
            });
        }
        if matches!(self.turn.output, OutputConstraint::Structured { .. })
            && !self
                .features
                .required
                .contains(&ModelFeature::StructuredOutput)
        {
            return Err(ModelContractError::MissingRequiredFeature {
                feature: ModelFeature::StructuredOutput,
            });
        }
        self.generation.validate(
            self.stable_system_prefix.segments.len(),
            &tool_ids,
            &self.features.required,
        )?;
        Ok(())
    }

    /// Validate that a driver can honor every mandatory request value without
    /// silently dropping a control or replay state.
    pub fn validate_against(
        &self,
        descriptor: &DriverDescriptor,
    ) -> Result<(), ModelContractError> {
        let unsupported = self
            .features
            .required
            .difference(&descriptor.emitted_features)
            .copied()
            .collect::<Vec<_>>();
        if !unsupported.is_empty() {
            return Err(ModelContractError::UnsupportedRequiredFeatures {
                driver_id: descriptor.id.to_string(),
                features: unsupported,
            });
        }
        self.generation
            .validate_against(&descriptor.request_capabilities)?;
        if let Some(continuation) = &self.continuation
            && !descriptor
                .request_capabilities
                .incoming_continuations
                .iter()
                .any(|capability| {
                    capability.matches(continuation.provider(), continuation.format())
                })
        {
            return Err(ModelContractError::UnsupportedContinuation {
                provider: continuation.provider().to_owned(),
                format: continuation.format().to_owned(),
            });
        }
        let reasoning_capabilities = descriptor.request_capabilities.reasoning.as_ref();
        if self
            .turn
            .conversation
            .iter()
            .any(|item| matches!(item, ConversationItem::Reasoning { .. }))
            && !reasoning_capabilities.is_some_and(|capabilities| capabilities.replays_items)
        {
            return Err(unsupported_control(
                "reasoning.replay_item",
                "reasoning conversation item",
            ));
        }
        for state in self.turn.conversation.iter().filter_map(|item| match item {
            ConversationItem::Reasoning {
                item: ReasoningItem {
                    state: Some(state), ..
                },
            } => Some(state),
            ConversationItem::Message { .. }
            | ConversationItem::ToolCall { .. }
            | ConversationItem::ToolResult { .. }
            | ConversationItem::Reasoning {
                item: ReasoningItem { state: None, .. },
            } => None,
        }) {
            let supports_state = reasoning_capabilities.is_some_and(|capabilities| {
                capabilities
                    .replay_state_formats
                    .iter()
                    .any(|format| format.matches(state.provider(), state.format()))
            });
            if !supports_state {
                return Err(ModelContractError::UnsupportedReasoningState {
                    provider: state.provider().to_owned(),
                    format: state.format().to_owned(),
                });
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageSemantics {
    Delta,
    Cumulative,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<u64>,
    #[serde(default)]
    pub details: BTreeMap<String, u64>,
}

impl TokenUsage {
    pub fn validate(&self) -> Result<(), ModelContractError> {
        if self.input_tokens.is_none()
            && self.output_tokens.is_none()
            && self.cached_input_tokens.is_none()
            && self.reasoning_tokens.is_none()
            && self.total_tokens.is_none()
            && self.details.is_empty()
        {
            return Err(ModelContractError::EmptyUsageUpdate);
        }
        if self.details.len() > MAX_USAGE_DETAILS {
            return Err(ModelContractError::TooManyUsageDetails {
                maximum: MAX_USAGE_DETAILS,
            });
        }
        for key in self.details.keys() {
            if key.is_empty()
                || key.len() > 64
                || key.trim() != key
                || key.chars().any(char::is_control)
            {
                return Err(ModelContractError::InvalidUsageDetailKey { key: key.clone() });
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageUpdate {
    pub semantics: UsageSemantics,
    pub usage: TokenUsage,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderWarning {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "provider_reason", rename_all = "snake_case")]
pub enum FinishReason {
    EndTurn,
    StopSequence,
    MaxOutputTokens,
    ToolCalls,
    ContentFilter,
    Refusal,
    Other(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureKind {
    Provider,
    Transport,
    Protocol,
    MalformedToolArguments,
    UnsupportedFeature,
    Cancelled,
    DeadlineExceeded,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelFailure {
    pub kind: FailureKind,
    pub message: String,
    #[serde(default)]
    pub retryable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_id: Option<ProviderCallId>,
}

impl ModelFailure {
    pub fn new(kind: FailureKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            retryable: false,
            provider_code: None,
            call_id: None,
        }
    }

    fn from_tool_call_error(error: ToolCallError) -> Self {
        let call_id = error.call_id().clone();
        let kind = if matches!(error, ToolCallError::MalformedArguments { .. }) {
            FailureKind::MalformedToolArguments
        } else {
            FailureKind::Protocol
        };
        let mut failure = Self::new(kind, error.to_string());
        failure.call_id = Some(call_id);
        failure
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ModelEvent {
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
        capability_id: String,
        arguments: Value,
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

impl ModelEvent {
    pub const fn is_terminal(&self) -> bool {
        matches!(self, Self::Completed { .. } | Self::Failed { .. })
    }

    pub fn validate(&self) -> Result<(), ModelContractError> {
        match self {
            Self::TextDelta { text } => {
                if text.is_empty() {
                    return Err(ModelContractError::EmptyText {
                        field: "text delta",
                    });
                }
            }
            Self::ToolCallArgumentDelta { delta, .. } => {
                if delta.is_empty() {
                    return Err(ModelContractError::EmptyText {
                        field: "tool argument delta",
                    });
                }
            }
            Self::StructuredOutput { .. } | Self::ReasoningItemStarted { .. } => {}
            Self::ReasoningDelta { delta, .. } => {
                if delta.is_empty() {
                    return Err(ModelContractError::EmptyText {
                        field: "reasoning delta",
                    });
                }
            }
            Self::ReasoningItemReady { item } => item.validate()?,
            Self::ToolCallStarted { capability_id, .. }
            | Self::ToolCallReady { capability_id, .. } => {
                validate_identifier("tool call capability id", capability_id)?;
            }
            Self::UsageUpdate { update } => update.usage.validate()?,
            Self::ProviderWarning { warning } => {
                if warning.message.trim().is_empty() {
                    return Err(ModelContractError::EmptyText {
                        field: "provider warning",
                    });
                }
            }
            Self::Completed { finish_reason, .. } => {
                if let FinishReason::Other(reason) = finish_reason {
                    validate_identifier("provider finish reason", reason)?;
                }
            }
            Self::Failed { failure } => {
                if failure.message.trim().is_empty() {
                    return Err(ModelContractError::EmptyText {
                        field: "model failure",
                    });
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelStreamEvent {
    pub ir_version: u16,
    pub sequence: u64,
    pub event: ModelEvent,
}

impl ModelStreamEvent {
    pub const fn new(sequence: u64, event: ModelEvent) -> Self {
        Self {
            ir_version: MODEL_IR_VERSION,
            sequence,
            event,
        }
    }

    pub fn validate(&self) -> Result<(), ModelContractError> {
        if self.ir_version != MODEL_IR_VERSION {
            return Err(ModelContractError::UnsupportedVersion {
                found: self.ir_version,
                expected: MODEL_IR_VERSION,
            });
        }
        self.event.validate()
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ToolCallError {
    #[error("tool call {call_id} was started more than once")]
    DuplicateStart { call_id: ProviderCallId },
    #[error("tool call {call_id} received arguments before it started")]
    UnknownCall { call_id: ProviderCallId },
    #[error("tool call {call_id} arguments exceed the {maximum}-byte limit")]
    ArgumentsTooLarge {
        call_id: ProviderCallId,
        maximum: usize,
    },
    #[error("tool call {call_id} arguments are malformed JSON: {reason}")]
    MalformedArguments {
        call_id: ProviderCallId,
        reason: String,
    },
}

impl ToolCallError {
    pub const fn call_id(&self) -> &ProviderCallId {
        match self {
            Self::DuplicateStart { call_id }
            | Self::UnknownCall { call_id }
            | Self::ArgumentsTooLarge { call_id, .. }
            | Self::MalformedArguments { call_id, .. } => call_id,
        }
    }
}

#[derive(Debug)]
struct PartialToolCall {
    capability_id: String,
    arguments: String,
}

#[derive(Debug, Default)]
pub struct ToolCallBuffer {
    active: BTreeMap<ProviderCallId, PartialToolCall>,
    completed: BTreeSet<ProviderCallId>,
}

impl ToolCallBuffer {
    pub fn start(
        &mut self,
        call_id: ProviderCallId,
        capability_id: impl Into<String>,
    ) -> Result<ModelEvent, ToolCallError> {
        if self.active.contains_key(&call_id) || self.completed.contains(&call_id) {
            return Err(ToolCallError::DuplicateStart { call_id });
        }
        let capability_id = capability_id.into();
        self.active.insert(
            call_id.clone(),
            PartialToolCall {
                capability_id: capability_id.clone(),
                arguments: String::new(),
            },
        );
        Ok(ModelEvent::ToolCallStarted {
            call_id,
            capability_id,
        })
    }

    pub fn push_arguments(
        &mut self,
        call_id: &ProviderCallId,
        delta: impl AsRef<str>,
    ) -> Result<ModelEvent, ToolCallError> {
        let delta = delta.as_ref();
        let Some(call) = self.active.get_mut(call_id) else {
            return Err(ToolCallError::UnknownCall {
                call_id: call_id.clone(),
            });
        };
        if call.arguments.len().saturating_add(delta.len()) > MAX_TOOL_ARGUMENT_BYTES {
            return Err(ToolCallError::ArgumentsTooLarge {
                call_id: call_id.clone(),
                maximum: MAX_TOOL_ARGUMENT_BYTES,
            });
        }
        call.arguments.push_str(delta);
        Ok(ModelEvent::ToolCallArgumentDelta {
            call_id: call_id.clone(),
            delta: delta.to_owned(),
        })
    }

    pub fn finish(&mut self, call_id: &ProviderCallId) -> Result<ModelEvent, ToolCallError> {
        let Some(call) = self.active.remove(call_id) else {
            return Err(ToolCallError::UnknownCall {
                call_id: call_id.clone(),
            });
        };
        let arguments = serde_json::from_str(&call.arguments).map_err(|error| {
            ToolCallError::MalformedArguments {
                call_id: call_id.clone(),
                reason: error.to_string(),
            }
        })?;
        self.completed.insert(call_id.clone());
        Ok(ModelEvent::ToolCallReady {
            call_id: call_id.clone(),
            capability_id: call.capability_id,
            arguments,
        })
    }

    pub fn has_active_calls(&self) -> bool {
        !self.active.is_empty()
    }

    pub fn active_call_count(&self) -> usize {
        self.active.len()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DriverDescriptor {
    pub id: DriverId,
    #[serde(default)]
    pub request_capabilities: RequestCapabilities,
    #[serde(default)]
    pub emitted_features: BTreeSet<ModelFeature>,
}

pub trait ModelDriver: Send + Sync {
    fn descriptor(&self) -> &DriverDescriptor;

    fn stream(&self, request: ModelRequest, cancellation: CancellationToken) -> ModelEventStream;
}

#[derive(Debug, Clone)]
pub struct CancellationToken {
    sender: watch::Sender<bool>,
}

impl Default for CancellationToken {
    fn default() -> Self {
        Self::new()
    }
}

impl CancellationToken {
    pub fn new() -> Self {
        let (sender, _receiver) = watch::channel(false);
        Self { sender }
    }

    pub fn cancel(&self) {
        self.sender.send_replace(true);
    }

    pub fn is_cancelled(&self) -> bool {
        *self.sender.borrow()
    }

    pub async fn cancelled(&self) {
        wait_for_cancellation(self.sender.subscribe()).await;
    }
}

async fn wait_for_cancellation(mut receiver: watch::Receiver<bool>) {
    loop {
        // Subscribe before this check. A cancellation racing with the check is
        // either observed here or recorded as an unseen watch update that
        // makes `changed` complete immediately.
        if *receiver.borrow_and_update() {
            return;
        }
        if receiver.changed().await.is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod cancellation_tests {
    use std::time::Duration;

    use super::{CancellationToken, wait_for_cancellation};

    #[tokio::test]
    async fn cancellation_after_subscription_before_initial_check_is_observed() {
        let cancellation = CancellationToken::new();
        let receiver = cancellation.sender.subscribe();
        cancellation.cancel();

        tokio::time::timeout(Duration::from_secs(1), wait_for_cancellation(receiver))
            .await
            .expect("a cancellation published after subscribe must not be missed");
    }
}
