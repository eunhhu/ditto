//! Bounded, provider-neutral retrieval primitives.
//!
//! This crate owns the versioned query shape shared by context and capability
//! retrieval.  It deliberately contains no provider implementation: an
//! embedding provider is an explicitly injected, local dependency of a caller
//! that wants the optional embedded mode.

use std::{collections::BTreeSet, fmt, ops::Deref, str::FromStr};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Version of the shared opaque task query.
pub const TASK_QUERY_VERSION: u16 = 1;

/// Maximum UTF-8 byte length of the required request component.
pub const MAX_REQUEST_BYTES: usize = 65_536;
/// Maximum UTF-8 byte length of each optional/set component.
pub const MAX_COMPONENT_BYTES: usize = 4_096;
/// Maximum number of entries accepted in each set-like signature field.
pub const MAX_SET_ENTRIES: usize = 64;
/// Maximum UTF-8 byte length of canonical query text.
pub const MAX_CANONICAL_QUERY_BYTES: usize = 131_072;
/// Maximum number of sorted, unique lexical tokens.
pub const MAX_LEXICAL_TOKENS: usize = 4_096;
/// Maximum UTF-8 byte length of a retrieval document.
pub const MAX_RETRIEVAL_DOCUMENT_BYTES: usize = 65_536;
/// Maximum UTF-8 byte length of an embedding descriptor.
pub const MAX_EMBEDDING_DESCRIPTOR_BYTES: usize = 256;
/// Minimum embedding dimension.
pub const MIN_EMBEDDING_DIMENSION: usize = 1;
/// Maximum embedding dimension.
pub const MAX_EMBEDDING_DIMENSION: usize = 4_096;
/// Maximum provider failure detail length.
pub const MAX_PROVIDER_FAILURE_BYTES: usize = 4_096;
/// Maximum number of V2 candidates inspected before filtering.
pub const MAX_CANDIDATE_COUNT: usize = 10_000;
/// Minimum context result count.
pub const MIN_CONTEXT_RESULT_LIMIT: usize = 1;
/// Maximum context result count.
pub const MAX_CONTEXT_RESULT_LIMIT: usize = 256;
/// Minimum capability root count.
pub const MIN_CAPABILITY_ROOT_LIMIT: usize = 1;
/// Maximum capability root count.
pub const MAX_CAPABILITY_ROOT_LIMIT: usize = 256;
/// Minimum expanded execution epoch count.
pub const MIN_EXECUTION_EPOCH_LIMIT: usize = 1;
/// Maximum expanded execution epoch count.
pub const MAX_EXECUTION_EPOCH_LIMIT: usize = 512;

/// Errors raised when a V2 query, document, limit, vector, or provider
/// violates a closed retrieval contract.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RetrievalError {
    #[error("request is empty after normalization")]
    EmptyRequest,
    #[error("{field} component at index {index:?} is empty after normalization")]
    EmptyComponent {
        field: &'static str,
        index: Option<usize>,
    },
    #[error("{field} contains a control character")]
    ControlCharacter { field: &'static str },
    #[error("{field} is {actual} bytes, exceeding the {maximum}-byte limit")]
    ComponentTooLong {
        field: &'static str,
        actual: usize,
        maximum: usize,
    },
    #[error("{field} contains {actual} entries, exceeding the {maximum}-entry limit")]
    TooManyEntries {
        field: &'static str,
        actual: usize,
        maximum: usize,
    },
    #[error("canonical query is {actual} bytes, exceeding the {maximum}-byte limit")]
    CanonicalQueryTooLong { actual: usize, maximum: usize },
    #[error(
        "canonical query has {actual} unique lexical tokens, exceeding the {maximum}-token limit"
    )]
    TooManyLexicalTokens { actual: usize, maximum: usize },
    #[error("retrieval document is {actual} bytes, exceeding the {maximum}-byte limit")]
    RetrievalDocumentTooLong { actual: usize, maximum: usize },
    #[error("candidate scan count {actual} exceeds the maximum of {maximum}")]
    CandidateCountExceeded { actual: usize, maximum: usize },
    #[error("{kind} limit {requested} is outside the inclusive range {minimum}..={maximum}")]
    ResultLimitOutOfRange {
        kind: &'static str,
        requested: usize,
        minimum: usize,
        maximum: usize,
    },
    #[error("embedding descriptor is empty")]
    EmptyEmbeddingDescriptor,
    #[error("embedding descriptor is {actual} bytes, exceeding the {maximum}-byte limit")]
    EmbeddingDescriptorTooLong { actual: usize, maximum: usize },
    #[error("embedding dimension {actual} is outside the inclusive range {minimum}..={maximum}")]
    EmbeddingDimensionOutOfRange {
        actual: usize,
        minimum: usize,
        maximum: usize,
    },
    #[error("embedding value at index {index} is not finite")]
    NonFiniteEmbeddingValue { index: usize },
    #[error("embedding vector has a zero norm")]
    ZeroEmbeddingVector,
    #[error("embedding dimensions differ: expected {expected}, got {actual}")]
    EmbeddingDimensionMismatch { expected: usize, actual: usize },
    #[error("embedding descriptors differ: expected {expected}, got {actual}")]
    EmbeddingDescriptorMismatch { expected: String, actual: String },
    #[error("embedding is not configured for this lexical-only query")]
    EmbeddingNotConfigured,
    #[error("embedding provider failed: {detail}")]
    ProviderFailure { detail: String },
    #[error(
        "embedding provider failure detail is {actual} bytes, exceeding the {maximum}-byte limit"
    )]
    ProviderFailureDetailTooLong { actual: usize, maximum: usize },
}

/// The canonical V2 task signature.  The existing context crate's five-field
/// `TaskSignature` remains a separate legacy type; resources intentionally live
/// here instead of being added to that public struct.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskSignatureV2 {
    pub request: String,
    #[serde(default)]
    pub active_goal: Option<String>,
    #[serde(default)]
    pub entities: Vec<String>,
    #[serde(default)]
    pub resources: Vec<String>,
    #[serde(default)]
    pub constraints: Vec<String>,
    #[serde(default)]
    pub expected_effect: Option<String>,
}

impl TaskSignatureV2 {
    /// Build a request with no optional components.
    pub fn new(request: impl Into<String>) -> Self {
        Self {
            request: request.into(),
            ..Self::default()
        }
    }

    /// Validate and normalize every signature component into the canonical V2
    /// representation.  No component or collection is silently truncated.
    pub fn normalize(&self) -> Result<Self, RetrievalError> {
        let request = normalize_required("request", &self.request, MAX_REQUEST_BYTES)?;
        let active_goal = normalize_optional("active_goal", self.active_goal.as_deref())?;
        let expected_effect =
            normalize_optional("expected_effect", self.expected_effect.as_deref())?;
        let entities = normalize_set("entities", &self.entities)?;
        let resources = normalize_set("resources", &self.resources)?;
        let constraints = normalize_set("constraints", &self.constraints)?;

        Ok(Self {
            request,
            active_goal,
            entities,
            resources,
            constraints,
            expected_effect,
        })
    }

    /// Validate this signature without retaining the normalized copy.
    pub fn validate(&self) -> Result<(), RetrievalError> {
        self.normalize().map(|_| ())
    }

    /// Create the opaque version-1 query in lexical-only mode.
    pub fn try_into_query(&self) -> Result<TaskQuery, RetrievalError> {
        TaskQuery::new(self.clone())
    }
}

impl From<&TaskSignatureV2> for TaskSignatureV2 {
    fn from(value: &TaskSignatureV2) -> Self {
        value.clone()
    }
}

/// Retrieval mode reported by a validated task query.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetrievalMode {
    LexicalOnly,
    Embedded,
}

impl RetrievalMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::LexicalOnly => "lexical_only",
            Self::Embedded => "embedded",
        }
    }
}

impl fmt::Display for RetrievalMode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A canonical, validated, immutable retrieval document.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct RetrievalDocument(String);

impl RetrievalDocument {
    pub fn new(value: impl Into<String>) -> Result<Self, RetrievalError> {
        let value = value.into();
        let actual = value.len();
        if actual > MAX_RETRIEVAL_DOCUMENT_BYTES {
            return Err(RetrievalError::RetrievalDocumentTooLong {
                actual,
                maximum: MAX_RETRIEVAL_DOCUMENT_BYTES,
            });
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn as_bytes(&self) -> &[u8] {
        self.0.as_bytes()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl AsRef<str> for RetrievalDocument {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl Deref for RetrievalDocument {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        self.as_str()
    }
}

impl fmt::Display for RetrievalDocument {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl TryFrom<String> for RetrievalDocument {
    type Error = RetrievalError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl TryFrom<&str> for RetrievalDocument {
    type Error = RetrievalError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl<'de> Deserialize<'de> for RetrievalDocument {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

/// A bounded number of candidates inspected before domain filtering.  Zero is
/// valid for an empty catalogue; 10,001 is rejected before any partial result
/// can be returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct CandidateCount(usize);

impl CandidateCount {
    pub fn new(value: usize) -> Result<Self, RetrievalError> {
        if value > MAX_CANDIDATE_COUNT {
            return Err(RetrievalError::CandidateCountExceeded {
                actual: value,
                maximum: MAX_CANDIDATE_COUNT,
            });
        }
        Ok(Self(value))
    }

    pub const fn get(self) -> usize {
        self.0
    }
}

impl TryFrom<usize> for CandidateCount {
    type Error = RetrievalError;

    fn try_from(value: usize) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl<'de> Deserialize<'de> for CandidateCount {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = usize::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

/// A requested context-result limit in the closed V2 range 1..=256.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct ContextResultLimit(usize);

impl ContextResultLimit {
    pub fn new(value: usize) -> Result<Self, RetrievalError> {
        bounded_result_limit(
            "context result",
            value,
            MIN_CONTEXT_RESULT_LIMIT,
            MAX_CONTEXT_RESULT_LIMIT,
        )
        .map(Self)
    }

    pub const fn get(self) -> usize {
        self.0
    }
}

impl TryFrom<usize> for ContextResultLimit {
    type Error = RetrievalError;

    fn try_from(value: usize) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl<'de> Deserialize<'de> for ContextResultLimit {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = usize::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

/// A requested capability-root limit in the closed V2 range 1..=256.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct CapabilityRootLimit(usize);

impl CapabilityRootLimit {
    pub fn new(value: usize) -> Result<Self, RetrievalError> {
        bounded_result_limit(
            "capability root",
            value,
            MIN_CAPABILITY_ROOT_LIMIT,
            MAX_CAPABILITY_ROOT_LIMIT,
        )
        .map(Self)
    }

    pub const fn get(self) -> usize {
        self.0
    }
}

impl TryFrom<usize> for CapabilityRootLimit {
    type Error = RetrievalError;

    fn try_from(value: usize) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl<'de> Deserialize<'de> for CapabilityRootLimit {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = usize::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

/// A requested expanded execution-epoch limit in the closed V2 range 1..=512.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct ExecutionEpochLimit(usize);

impl ExecutionEpochLimit {
    pub fn new(value: usize) -> Result<Self, RetrievalError> {
        bounded_result_limit(
            "execution epoch",
            value,
            MIN_EXECUTION_EPOCH_LIMIT,
            MAX_EXECUTION_EPOCH_LIMIT,
        )
        .map(Self)
    }

    pub const fn get(self) -> usize {
        self.0
    }
}

impl TryFrom<usize> for ExecutionEpochLimit {
    type Error = RetrievalError;

    fn try_from(value: usize) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl<'de> Deserialize<'de> for ExecutionEpochLimit {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = usize::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

/// A validated, non-empty embedding descriptor.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct EmbeddingDescriptor(String);

impl EmbeddingDescriptor {
    pub fn new(value: impl Into<String>) -> Result<Self, RetrievalError> {
        let value = value.into();
        if value.is_empty() {
            return Err(RetrievalError::EmptyEmbeddingDescriptor);
        }
        let actual = value.len();
        if actual > MAX_EMBEDDING_DESCRIPTOR_BYTES {
            return Err(RetrievalError::EmbeddingDescriptorTooLong {
                actual,
                maximum: MAX_EMBEDDING_DESCRIPTOR_BYTES,
            });
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl AsRef<str> for EmbeddingDescriptor {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for EmbeddingDescriptor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for EmbeddingDescriptor {
    type Err = RetrievalError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl TryFrom<String> for EmbeddingDescriptor {
    type Error = RetrievalError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl TryFrom<&str> for EmbeddingDescriptor {
    type Error = RetrievalError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl<'de> Deserialize<'de> for EmbeddingDescriptor {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

/// A finite, non-zero, unit-normalized vector with a bounded dimension.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EmbeddingVector(Vec<f32>);

impl EmbeddingVector {
    pub fn new(values: impl Into<Vec<f32>>) -> Result<Self, RetrievalError> {
        let values = values.into();
        validate_dimension(values.len())?;
        for (index, value) in values.iter().copied().enumerate() {
            if !value.is_finite() {
                return Err(RetrievalError::NonFiniteEmbeddingValue { index });
            }
        }

        let norm_squared = values
            .iter()
            .map(|value| f64::from(*value) * f64::from(*value))
            .sum::<f64>();
        if !norm_squared.is_finite() || norm_squared == 0.0 {
            return Err(RetrievalError::ZeroEmbeddingVector);
        }
        let norm = norm_squared.sqrt();
        let normalized = values
            .into_iter()
            .map(|value| (f64::from(value) / norm) as f32)
            .collect::<Vec<_>>();
        // The source vector was finite and non-zero, and division by a finite
        // norm preserves those properties.  Keep the invariant explicit in
        // case this implementation is changed later.
        for (index, value) in normalized.iter().copied().enumerate() {
            if !value.is_finite() {
                return Err(RetrievalError::NonFiniteEmbeddingValue { index });
            }
        }
        Ok(Self(normalized))
    }

    pub fn as_slice(&self) -> &[f32] {
        &self.0
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn norm(&self) -> f32 {
        self.0
            .iter()
            .map(|value| f64::from(*value) * f64::from(*value))
            .sum::<f64>()
            .sqrt() as f32
    }

    pub fn cosine_similarity(&self, other: &Self) -> Result<f32, RetrievalError> {
        if self.len() != other.len() {
            return Err(RetrievalError::EmbeddingDimensionMismatch {
                expected: self.len(),
                actual: other.len(),
            });
        }
        let dot = self
            .0
            .iter()
            .zip(other.0.iter())
            .map(|(left, right)| f64::from(*left) * f64::from(*right))
            .sum::<f64>();
        Ok(dot as f32)
    }
}

impl AsRef<[f32]> for EmbeddingVector {
    fn as_ref(&self) -> &[f32] {
        self.as_slice()
    }
}

impl TryFrom<Vec<f32>> for EmbeddingVector {
    type Error = RetrievalError;

    fn try_from(value: Vec<f32>) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl TryFrom<&[f32]> for EmbeddingVector {
    type Error = RetrievalError;

    fn try_from(value: &[f32]) -> Result<Self, Self::Error> {
        Self::new(value.to_vec())
    }
}

impl<'de> Deserialize<'de> for EmbeddingVector {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let values = Vec::<f32>::deserialize(deserializer)?;
        Self::new(values).map_err(serde::de::Error::custom)
    }
}

/// Compute cosine similarity for two validated vectors.
pub fn cosine_similarity(
    left: &EmbeddingVector,
    right: &EmbeddingVector,
) -> Result<f32, RetrievalError> {
    left.cosine_similarity(right)
}

/// Compute cosine similarity after applying the same bounded vector
/// validation/normalization used for provider output.
pub fn cosine_similarity_values(
    left: impl Into<Vec<f32>>,
    right: impl Into<Vec<f32>>,
) -> Result<f32, RetrievalError> {
    let left = EmbeddingVector::new(left)?;
    let right = EmbeddingVector::new(right)?;
    left.cosine_similarity(&right)
}

/// Why a provider is being invoked.  Query is called at most once when a
/// `TaskQuery` enters embedded mode; document calls validate descriptor and
/// dimension continuity against that query embedding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EmbeddingPurpose {
    Query,
    Document,
}

impl EmbeddingPurpose {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Query => "query",
            Self::Document => "document",
        }
    }
}

/// Raw provider output.  Fields are intentionally public so deterministic
/// fixtures can exercise invalid output; `TaskQuery` validates every field
/// before accepting it.  The descriptor and vector are never persisted by
/// this crate.
#[derive(Debug, Clone, PartialEq)]
pub struct Embedding {
    pub descriptor: String,
    pub vector: Vec<f32>,
}

impl Embedding {
    pub fn new(descriptor: impl Into<String>, vector: impl Into<Vec<f32>>) -> Self {
        Self {
            descriptor: descriptor.into(),
            vector: vector.into(),
        }
    }
}

/// Typed error returned by an injected embedding provider.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum EmbeddingProviderError {
    #[error("{detail}")]
    Failure { detail: String },
}

impl EmbeddingProviderError {
    pub fn failure(detail: impl Into<String>) -> Self {
        Self::Failure {
            detail: detail.into(),
        }
    }

    pub fn detail(&self) -> &str {
        match self {
            Self::Failure { detail } => detail,
        }
    }
}

/// Object-safe provider boundary.  Ditto supplies no production provider;
/// callers inject a local implementation only when they explicitly request
/// embedded retrieval.
pub trait EmbeddingProvider: Send + Sync {
    fn embed(
        &self,
        purpose: EmbeddingPurpose,
        text: &str,
    ) -> Result<Embedding, EmbeddingProviderError>;
}

#[derive(Debug, Clone, PartialEq)]
struct ValidatedEmbedding {
    descriptor: EmbeddingDescriptor,
    vector: EmbeddingVector,
}

/// Opaque version-1 canonical task query shared by context and capability
/// retrieval.  Its fields are private so no consumer can bypass normalization,
/// lexical bounds, or embedding validation.
#[derive(Debug, Clone, PartialEq)]
pub struct TaskQuery {
    version: u16,
    signature: TaskSignatureV2,
    canonical_text: String,
    lexical_tokens: Vec<String>,
    exact_terms: Vec<String>,
    mode: RetrievalMode,
    query_embedding: Option<ValidatedEmbedding>,
}

impl TaskQuery {
    /// Build a validated lexical-only query.
    pub fn new(signature: impl Into<TaskSignatureV2>) -> Result<Self, RetrievalError> {
        let signature = signature.into().normalize()?;
        let canonical_text = canonical_text(&signature);
        if canonical_text.len() > MAX_CANONICAL_QUERY_BYTES {
            return Err(RetrievalError::CanonicalQueryTooLong {
                actual: canonical_text.len(),
                maximum: MAX_CANONICAL_QUERY_BYTES,
            });
        }
        let lexical_tokens = lexical_tokens(&canonical_text);
        if lexical_tokens.len() > MAX_LEXICAL_TOKENS {
            return Err(RetrievalError::TooManyLexicalTokens {
                actual: lexical_tokens.len(),
                maximum: MAX_LEXICAL_TOKENS,
            });
        }
        let exact_terms = signature
            .entities
            .iter()
            .chain(signature.resources.iter())
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();

        Ok(Self {
            version: TASK_QUERY_VERSION,
            signature,
            canonical_text,
            lexical_tokens,
            exact_terms,
            mode: RetrievalMode::LexicalOnly,
            query_embedding: None,
        })
    }

    /// Build a query and, when a provider is present, perform exactly one
    /// query embedding call.
    pub fn with_provider(
        signature: impl Into<TaskSignatureV2>,
        provider: Option<&dyn EmbeddingProvider>,
    ) -> Result<Self, RetrievalError> {
        let query = Self::new(signature)?;
        if let Some(provider) = provider {
            return query.with_embedded_provider(provider);
        }
        Ok(query)
    }

    fn with_embedded_provider(
        self,
        provider: &dyn EmbeddingProvider,
    ) -> Result<Self, RetrievalError> {
        let output = provider
            .embed(EmbeddingPurpose::Query, &self.canonical_text)
            .map_err(provider_error)?;
        let embedding = validate_embedding(output)?;
        Ok(Self {
            mode: RetrievalMode::Embedded,
            query_embedding: Some(embedding),
            ..self
        })
    }

    /// Validate a document embedding against the query descriptor and
    /// dimension.  This does not call a provider.
    pub fn validate_document_embedding(
        &self,
        output: Embedding,
    ) -> Result<EmbeddingVector, RetrievalError> {
        let expected = self
            .query_embedding
            .as_ref()
            .ok_or(RetrievalError::EmbeddingNotConfigured)?;
        let actual = validate_embedding(output)?;
        ensure_embedding_continuity(expected, &actual)?;
        Ok(actual.vector)
    }

    /// Ask the injected provider for one document embedding, validating its
    /// output and descriptor/dimension continuity against this query.
    pub fn embed_document(
        &self,
        provider: &dyn EmbeddingProvider,
        document: &RetrievalDocument,
    ) -> Result<EmbeddingVector, RetrievalError> {
        let _ = self
            .query_embedding
            .as_ref()
            .ok_or(RetrievalError::EmbeddingNotConfigured)?;
        let output = provider
            .embed(EmbeddingPurpose::Document, document.as_str())
            .map_err(provider_error)?;
        self.validate_document_embedding(output)
    }

    pub const fn version(&self) -> u16 {
        self.version
    }

    pub const fn mode(&self) -> RetrievalMode {
        self.mode
    }

    pub fn signature(&self) -> &TaskSignatureV2 {
        &self.signature
    }

    pub fn canonical_text(&self) -> &str {
        &self.canonical_text
    }

    pub fn lexical_tokens(&self) -> &[String] {
        &self.lexical_tokens
    }

    pub fn exact_terms(&self) -> &[String] {
        &self.exact_terms
    }

    pub fn query_embedding(&self) -> Option<&EmbeddingVector> {
        self.query_embedding
            .as_ref()
            .map(|embedding| &embedding.vector)
    }

    pub fn embedding_descriptor(&self) -> Option<&EmbeddingDescriptor> {
        self.query_embedding
            .as_ref()
            .map(|embedding| &embedding.descriptor)
    }

    pub fn embedding_dimension(&self) -> Option<usize> {
        self.query_embedding().map(EmbeddingVector::len)
    }

    pub fn is_embedded(&self) -> bool {
        self.mode == RetrievalMode::Embedded
    }
}

fn normalize_required(
    field: &'static str,
    value: &str,
    maximum: usize,
) -> Result<String, RetrievalError> {
    if value.len() > maximum {
        return Err(RetrievalError::ComponentTooLong {
            field,
            actual: value.len(),
            maximum,
        });
    }
    let normalized = normalize_text(field, value)?;
    if normalized.is_empty() {
        if field == "request" {
            Err(RetrievalError::EmptyRequest)
        } else {
            Err(RetrievalError::EmptyComponent { field, index: None })
        }
    } else if normalized.len() > maximum {
        Err(RetrievalError::ComponentTooLong {
            field,
            actual: normalized.len(),
            maximum,
        })
    } else {
        Ok(normalized)
    }
}

fn normalize_optional(
    field: &'static str,
    value: Option<&str>,
) -> Result<Option<String>, RetrievalError> {
    value
        .map(|value| normalize_required(field, value, MAX_COMPONENT_BYTES))
        .transpose()
}

fn normalize_set(field: &'static str, values: &[String]) -> Result<Vec<String>, RetrievalError> {
    if values.len() > MAX_SET_ENTRIES {
        return Err(RetrievalError::TooManyEntries {
            field,
            actual: values.len(),
            maximum: MAX_SET_ENTRIES,
        });
    }
    let mut normalized = BTreeSet::new();
    for (index, value) in values.iter().enumerate() {
        if value.len() > MAX_COMPONENT_BYTES {
            return Err(RetrievalError::ComponentTooLong {
                field,
                actual: value.len(),
                maximum: MAX_COMPONENT_BYTES,
            });
        }
        let value = normalize_text(field, value)?;
        if value.is_empty() {
            return Err(RetrievalError::EmptyComponent {
                field,
                index: Some(index),
            });
        }
        if value.len() > MAX_COMPONENT_BYTES {
            return Err(RetrievalError::ComponentTooLong {
                field,
                actual: value.len(),
                maximum: MAX_COMPONENT_BYTES,
            });
        }
        normalized.insert(value);
    }
    Ok(normalized.into_iter().collect())
}

fn normalize_text(field: &'static str, value: &str) -> Result<String, RetrievalError> {
    let mut lower = String::new();
    for character in value.chars().flat_map(char::to_lowercase) {
        if character.is_control() && !character.is_whitespace() {
            return Err(RetrievalError::ControlCharacter { field });
        }
        lower.push(character);
    }
    Ok(lower.split_whitespace().collect::<Vec<_>>().join(" "))
}

fn canonical_text(signature: &TaskSignatureV2) -> String {
    let mut parts = Vec::new();
    parts.push(signature.request.as_str());
    if let Some(value) = signature.active_goal.as_deref() {
        parts.push(value);
    }
    parts.extend(signature.entities.iter().map(String::as_str));
    parts.extend(signature.resources.iter().map(String::as_str));
    parts.extend(signature.constraints.iter().map(String::as_str));
    if let Some(value) = signature.expected_effect.as_deref() {
        parts.push(value);
    }
    parts.join(" ")
}

fn lexical_tokens(value: &str) -> Vec<String> {
    let mut tokens = BTreeSet::new();
    let mut current = String::new();
    for character in value.chars() {
        if character.is_alphanumeric() {
            current.push(character);
        } else if !current.is_empty() {
            tokens.insert(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        tokens.insert(current);
    }
    tokens.into_iter().collect()
}

fn bounded_result_limit(
    kind: &'static str,
    requested: usize,
    minimum: usize,
    maximum: usize,
) -> Result<usize, RetrievalError> {
    if !(minimum..=maximum).contains(&requested) {
        return Err(RetrievalError::ResultLimitOutOfRange {
            kind,
            requested,
            minimum,
            maximum,
        });
    }
    Ok(requested)
}

fn validate_dimension(dimension: usize) -> Result<(), RetrievalError> {
    if !(MIN_EMBEDDING_DIMENSION..=MAX_EMBEDDING_DIMENSION).contains(&dimension) {
        return Err(RetrievalError::EmbeddingDimensionOutOfRange {
            actual: dimension,
            minimum: MIN_EMBEDDING_DIMENSION,
            maximum: MAX_EMBEDDING_DIMENSION,
        });
    }
    Ok(())
}

fn validate_embedding(output: Embedding) -> Result<ValidatedEmbedding, RetrievalError> {
    let descriptor = EmbeddingDescriptor::new(output.descriptor)?;
    let vector = EmbeddingVector::new(output.vector)?;
    Ok(ValidatedEmbedding { descriptor, vector })
}

fn ensure_embedding_continuity(
    expected: &ValidatedEmbedding,
    actual: &ValidatedEmbedding,
) -> Result<(), RetrievalError> {
    if expected.descriptor != actual.descriptor {
        return Err(RetrievalError::EmbeddingDescriptorMismatch {
            expected: expected.descriptor.as_str().to_owned(),
            actual: actual.descriptor.as_str().to_owned(),
        });
    }
    if expected.vector.len() != actual.vector.len() {
        return Err(RetrievalError::EmbeddingDimensionMismatch {
            expected: expected.vector.len(),
            actual: actual.vector.len(),
        });
    }
    Ok(())
}

fn provider_error(error: EmbeddingProviderError) -> RetrievalError {
    let detail = error.detail();
    if detail.len() > MAX_PROVIDER_FAILURE_BYTES {
        return RetrievalError::ProviderFailureDetailTooLong {
            actual: detail.len(),
            maximum: MAX_PROVIDER_FAILURE_BYTES,
        };
    }
    RetrievalError::ProviderFailure {
        detail: detail.to_owned(),
    }
}
