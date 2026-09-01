use std::{
    collections::{HashMap, HashSet},
    fmt,
};

use chrono::{DateTime, Utc};
use ditto_retrieval::{
    CandidateCount, ContextResultLimit, EmbeddingProvider, RetrievalMode, RetrievalWorkBudget,
    cosine_similarity,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub use ditto_retrieval::{
    MAX_REQUEST_BYTES, MAX_RETRIEVAL_DOCUMENT_BYTES, RetrievalDocument, RetrievalError, TaskQuery,
    TaskSignatureV2,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextNodeKind {
    Goal,
    Constraint,
    Entity,
    Resource,
    Claim,
    Preference,
    Decision,
    Assumption,
    OpenQuestion,
    Action,
    Evidence,
    Risk,
    Capability,
}

impl ContextNodeKind {
    /// Return the closed version-1 wire token used by retrieval documents.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Goal => "goal",
            Self::Constraint => "constraint",
            Self::Entity => "entity",
            Self::Resource => "resource",
            Self::Claim => "claim",
            Self::Preference => "preference",
            Self::Decision => "decision",
            Self::Assumption => "assumption",
            Self::OpenQuestion => "open_question",
            Self::Action => "action",
            Self::Evidence => "evidence",
            Self::Risk => "risk",
            Self::Capability => "capability",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextOrigin {
    User,
    Model,
    Capability,
    Policy,
    System,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EpistemicStatus {
    Asserted,
    Inferred,
    Verified,
    Disputed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextScope {
    Turn,
    Session,
    Task,
    Project,
    Device,
    Global,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextLens {
    Personal,
    #[default]
    Task,
    Environment,
    Conversation,
}

/// Durable semantic content. Compiler authority is intentionally not stored here.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContextNode {
    pub id: String,
    pub kind: ContextNodeKind,
    pub summary: String,
    pub origin: ContextOrigin,
    pub epistemic: EpistemicStatus,
    pub scope: ContextScope,
    #[serde(default)]
    pub lens: ContextLens,
    pub confidence: f32,
    #[serde(default)]
    pub source_event_ids: Vec<String>,
    #[serde(default)]
    pub supersedes: Vec<String>,
    #[serde(default)]
    pub valid_from: Option<DateTime<Utc>>,
    #[serde(default)]
    pub valid_until: Option<DateTime<Utc>>,
}

impl ContextNode {
    pub fn validate(&self) -> Result<(), ContextValidationError> {
        if self.id.trim().is_empty() {
            return Err(ContextValidationError::EmptyId);
        }
        if self.summary.trim().is_empty() {
            return Err(ContextValidationError::EmptySummary {
                node_id: self.id.clone(),
            });
        }
        if !self.confidence.is_finite() || !(0.0..=1.0).contains(&self.confidence) {
            return Err(ContextValidationError::InvalidConfidence {
                node_id: self.id.clone(),
                confidence: self.confidence,
            });
        }
        if self.scope != ContextScope::Turn && !has_provenance(&self.source_event_ids) {
            return Err(ContextValidationError::MissingProvenance {
                subject_id: self.id.clone(),
            });
        }
        if self.origin == ContextOrigin::Model && self.epistemic == EpistemicStatus::Asserted {
            return Err(ContextValidationError::ModelCannotAssert {
                node_id: self.id.clone(),
            });
        }
        if let (Some(valid_from), Some(valid_until)) = (self.valid_from, self.valid_until)
            && valid_until <= valid_from
        {
            return Err(ContextValidationError::InvalidValidityWindow {
                node_id: self.id.clone(),
            });
        }
        Ok(())
    }

    pub fn is_valid_at(&self, now: DateTime<Utc>) -> bool {
        self.valid_from.as_ref().is_none_or(|start| start <= &now)
            && self.valid_until.as_ref().is_none_or(|end| end > &now)
            && self.epistemic != EpistemicStatus::Disputed
    }

    /// Build the canonical bounded V2 retrieval document for this node.
    pub fn retrieval_document(&self) -> Result<RetrievalDocument, RetrievalError> {
        context_retrieval_document(self)
    }
}

/// Build the canonical bounded V2 context document.
///
/// Fields are copied verbatim and are intentionally not escaped.  The fixed
/// shape is part of the V2 retrieval contract and has no trailing newline:
/// `id=<raw id>\nkind=<snake_case kind>\nsummary=<raw summary>`.
pub fn context_retrieval_document(node: &ContextNode) -> Result<RetrievalDocument, RetrievalError> {
    let document_len = context_retrieval_document_len(node)?;
    let mut document = String::with_capacity(document_len);
    document.push_str("id=");
    document.push_str(&node.id);
    document.push_str("\nkind=");
    document.push_str(node.kind.as_str());
    document.push_str("\nsummary=");
    document.push_str(&node.summary);
    RetrievalDocument::new(document)
}

fn context_retrieval_document_len(node: &ContextNode) -> Result<usize, RetrievalError> {
    let actual = "id="
        .len()
        .saturating_add(node.id.len())
        .saturating_add("\nkind=".len())
        .saturating_add(node.kind.as_str().len())
        .saturating_add("\nsummary=".len())
        .saturating_add(node.summary.len());
    if actual > MAX_RETRIEVAL_DOCUMENT_BYTES {
        return Err(RetrievalError::RetrievalDocumentTooLong {
            actual,
            maximum: MAX_RETRIEVAL_DOCUMENT_BYTES,
        });
    }
    Ok(actual)
}

// Defense-in-depth mirrors of ADR 0010's node-local durable V1 bounds.
// `ditto-context-projection` remains the authoritative event-admission owner;
// ranking rechecks plain projected nodes so cache corruption fails closed.
const MAX_CONTEXT_NODE_ID_BYTES: usize = 256;
const MAX_CONTEXT_NODE_SUMMARY_BYTES: usize = 65_000;
const MAX_CONTEXT_REFERENCE_ID_BYTES: usize = 256;
const MAX_CONTEXT_SOURCE_EVENT_IDS: usize = 64;
const MAX_CONTEXT_SUPERSEDES: usize = 64;
const MAX_SERIALIZED_CONTEXT_NODE_BYTES: usize = 131_072;
const MAX_CONTEXT_RANKING_ERROR_DETAIL_BYTES: usize = 4_096;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextEdgeKind {
    Supports,
    Contradicts,
    Supersedes,
    DerivedFrom,
    RelatedTo,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextEdge {
    pub id: String,
    pub from: String,
    pub to: String,
    pub relation: ContextEdgeKind,
    #[serde(default)]
    pub source_event_ids: Vec<String>,
}

impl ContextEdge {
    pub fn validate(&self) -> Result<(), ContextValidationError> {
        if self.id.trim().is_empty() {
            return Err(ContextValidationError::EmptyId);
        }
        if !has_provenance(&self.source_event_ids) {
            return Err(ContextValidationError::MissingProvenance {
                subject_id: self.id.clone(),
            });
        }
        Ok(())
    }
}

#[derive(Debug, Error, PartialEq)]
pub enum ContextValidationError {
    #[error("context id is empty")]
    EmptyId,
    #[error("context node {node_id} has an empty summary")]
    EmptySummary { node_id: String },
    #[error("context {subject_id} has no source event provenance")]
    MissingProvenance { subject_id: String },
    #[error("model-origin context {node_id} cannot be asserted")]
    ModelCannotAssert { node_id: String },
    #[error("context {node_id} has invalid confidence {confidence}")]
    InvalidConfidence { node_id: String, confidence: f32 },
    #[error("context {node_id} has an invalid validity window")]
    InvalidValidityWindow { node_id: String },
    #[error("context edge {edge_id} references missing node {node_id}")]
    MissingNode { edge_id: String, node_id: String },
    #[error("context node {node_id} already exists")]
    DuplicateNode { node_id: String },
}

#[derive(Debug, Clone, Default)]
pub struct ContextGraph {
    nodes: HashMap<String, ContextNode>,
    edges: Vec<ContextEdge>,
}

impl ContextGraph {
    pub fn insert_node(&mut self, node: ContextNode) -> Result<(), ContextValidationError> {
        node.validate()?;
        if self.nodes.contains_key(&node.id) {
            return Err(ContextValidationError::DuplicateNode {
                node_id: node.id.clone(),
            });
        }
        self.nodes.insert(node.id.clone(), node);
        Ok(())
    }

    pub fn insert_edge(&mut self, edge: ContextEdge) -> Result<(), ContextValidationError> {
        edge.validate()?;
        for node_id in [&edge.from, &edge.to] {
            if !self.nodes.contains_key(node_id) {
                return Err(ContextValidationError::MissingNode {
                    edge_id: edge.id.clone(),
                    node_id: node_id.clone(),
                });
            }
        }
        self.edges.push(edge);
        Ok(())
    }

    pub fn nodes(&self) -> impl Iterator<Item = &ContextNode> {
        self.nodes.values()
    }

    pub fn edges(&self) -> &[ContextEdge] {
        &self.edges
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TaskSignature {
    pub request: String,
    #[serde(default)]
    pub active_goal: Option<String>,
    #[serde(default)]
    pub entities: Vec<String>,
    #[serde(default)]
    pub constraints: Vec<String>,
    #[serde(default)]
    pub expected_effect: Option<String>,
}

impl TaskSignature {
    pub fn searchable_text(&self) -> String {
        let mut parts = vec![self.request.clone()];
        if let Some(active_goal) = &self.active_goal {
            parts.push(active_goal.clone());
        }
        parts.extend(self.entities.iter().cloned());
        parts.extend(self.constraints.iter().cloned());
        if let Some(expected_effect) = &self.expected_effect {
            parts.push(expected_effect.clone());
        }
        parts.join(" ")
    }

    /// Explicitly migrate a legacy V1 signature into the bounded V2 shape.
    ///
    /// The legacy compiler never calls this method implicitly.  Constructing a
    /// `TaskQuery` first makes the returned signature the normalized V2 value
    /// and applies every V2 field, canonical-query, and lexical-token bound.
    pub fn try_to_v2(&self) -> Result<TaskSignatureV2, RetrievalError> {
        let signature = TaskSignatureV2 {
            request: self.request.clone(),
            active_goal: self.active_goal.clone(),
            entities: self.entities.clone(),
            resources: Vec::new(),
            constraints: self.constraints.clone(),
            expected_effect: self.expected_effect.clone(),
        };
        Ok(TaskQuery::new(signature)?.signature().clone())
    }
}

impl TryFrom<&TaskSignature> for TaskSignatureV2 {
    type Error = RetrievalError;

    fn try_from(value: &TaskSignature) -> Result<Self, Self::Error> {
        value.try_to_v2()
    }
}

impl TryFrom<TaskSignature> for TaskSignatureV2 {
    type Error = RetrievalError;

    fn try_from(value: TaskSignature) -> Result<Self, Self::Error> {
        value.try_to_v2()
    }
}

/// Trusted, ephemeral compiler directive. It is not deserializable from model output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContextDirective {
    Ranked,
    UserPinned,
    PolicyRequired { reason: String },
}

impl ContextDirective {
    const fn priority(&self) -> u8 {
        match self {
            Self::Ranked => 0,
            Self::UserPinned => 1,
            Self::PolicyRequired { .. } => 2,
        }
    }

    fn receipt_reason(&self) -> String {
        match self {
            Self::Ranked => "task-relevance".into(),
            Self::UserPinned => "user-pinned".into(),
            Self::PolicyRequired { reason } => format!("policy-required: {reason}"),
        }
    }

    const fn is_required(&self) -> bool {
        !matches!(self, Self::Ranked)
    }

    fn validate(&self) -> Result<(), ()> {
        match self {
            Self::PolicyRequired { reason } if reason.trim().is_empty() => Err(()),
            Self::Ranked | Self::UserPinned | Self::PolicyRequired { .. } => Ok(()),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ContextCandidate {
    pub node: ContextNode,
    pub directive: ContextDirective,
}

impl ContextCandidate {
    pub fn ranked(node: ContextNode) -> Self {
        Self {
            node,
            directive: ContextDirective::Ranked,
        }
    }

    pub fn user_pinned(node: ContextNode) -> Self {
        Self {
            node,
            directive: ContextDirective::UserPinned,
        }
    }

    pub fn policy_required(node: ContextNode, reason: impl Into<String>) -> Self {
        Self {
            node,
            directive: ContextDirective::PolicyRequired {
                reason: reason.into(),
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContextReceiptEntry {
    pub node_id: String,
    pub source_event_ids: Vec<String>,
    pub epistemic: EpistemicStatus,
    pub reason: String,
    pub score: f32,
    pub token_cost: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextExclusionReason {
    Invalid,
    DisputedOrExpired,
    Irrelevant,
    TokenBudget,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextExclusion {
    pub node_id: String,
    pub reason: ContextExclusionReason,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContextReceipt {
    pub included: Vec<ContextReceiptEntry>,
    pub excluded: Vec<ContextExclusion>,
    pub total_token_cost: u32,
    pub token_budget: u32,
    pub absolute_budget: u32,
    pub over_soft_budget: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompiledContext {
    pub nodes: Vec<ContextNode>,
    pub receipt: ContextReceipt,
}

/// A compact, model-facing projection of one durable context node.
///
/// Compiler directives, receipts, and durable supersession/lens metadata remain
/// harness-owned. The remaining metadata is intentionally retained so a
/// deserialized capsule can be validated before it reaches a model boundary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContextCapsuleItem {
    pub id: String,
    pub kind: ContextNodeKind,
    pub summary: String,
    pub origin: ContextOrigin,
    pub epistemic: EpistemicStatus,
    pub scope: ContextScope,
    pub confidence: f32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_event_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub valid_from: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub valid_until: Option<DateTime<Utc>>,
}

impl From<&ContextNode> for ContextCapsuleItem {
    fn from(node: &ContextNode) -> Self {
        Self {
            id: node.id.clone(),
            kind: node.kind,
            summary: node.summary.clone(),
            origin: node.origin,
            epistemic: node.epistemic,
            scope: node.scope,
            confidence: node.confidence,
            source_event_ids: node.source_event_ids.clone(),
            valid_from: node.valid_from,
            valid_until: node.valid_until,
        }
    }
}

impl ContextCapsuleItem {
    /// Validate this model-facing item at a specific wall-clock instant.
    pub fn validate_at(&self, now: DateTime<Utc>) -> Result<(), ContextCapsuleValidationError> {
        let node = self.as_context_node();
        node.validate()
            .map_err(|error| ContextCapsuleValidationError::InvalidItem {
                item_id: self.id.clone(),
                reason: error.to_string(),
            })?;
        if !node.is_valid_at(now) {
            return Err(ContextCapsuleValidationError::NotValidAt {
                item_id: self.id.clone(),
            });
        }
        Ok(())
    }

    /// Return the exact serialized item size used by the compiler's estimate.
    ///
    /// An item that cannot be represented as JSON is charged as maximally large
    /// so callers fail closed rather than panicking or undercounting it.
    pub fn serialized_len(&self) -> usize {
        serde_json::to_vec(self).map_or(usize::MAX, |serialized| serialized.len())
    }

    /// Return the conservative token estimate charged for this item.
    ///
    /// The fixed overhead covers the surrounding capsule array and separators;
    /// charging it per item keeps the sum conservative for every capsule size.
    pub fn token_cost(&self) -> u32 {
        estimate_serialized_tokens(self.serialized_len(), CAPSULE_ITEM_FIXED_OVERHEAD_TOKENS)
    }

    fn as_context_node(&self) -> ContextNode {
        ContextNode {
            id: self.id.clone(),
            kind: self.kind,
            summary: self.summary.clone(),
            origin: self.origin,
            epistemic: self.epistemic,
            scope: self.scope,
            // Lens and supersession are durable/harness metadata and are not
            // part of the model projection. Their values are irrelevant to
            // the validation invariants enforced here.
            lens: ContextLens::default(),
            confidence: self.confidence,
            source_event_ids: self.source_event_ids.clone(),
            supersedes: Vec::new(),
            valid_from: self.valid_from,
            valid_until: self.valid_until,
        }
    }
}

/// Validation failures for model-facing context projections.
#[derive(Debug, Error, PartialEq)]
pub enum ContextCapsuleValidationError {
    #[error("context capsule item {item_id} is invalid: {reason}")]
    InvalidItem { item_id: String, reason: String },
    #[error("context capsule item {item_id} is disputed or not valid at the requested time")]
    NotValidAt { item_id: String },
    #[error(
        "context capsule costs {used} tokens, exceeding the absolute ceiling {absolute_budget}"
    )]
    TokenBudgetExceeded { used: u32, absolute_budget: u32 },
}

/// Fixed token overhead charged for each serialized capsule item.
///
/// This is deliberately conservative: it covers the capsule's `nodes` field,
/// array punctuation, and separators in addition to the serialized item body.
pub const CAPSULE_ITEM_FIXED_OVERHEAD_TOKENS: u32 = 16;

/// Default soft context budget in estimated tokens.
pub const DEFAULT_CONTEXT_TOKEN_BUDGET: u32 = 900;

/// Default absolute context ceiling in estimated tokens.
pub const DEFAULT_CONTEXT_ABSOLUTE_BUDGET: u32 = 1_800;

/// Model-facing context projection. Compiler receipts remain harness-owned.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ContextCapsule {
    #[serde(default)]
    pub nodes: Vec<ContextCapsuleItem>,
}

impl From<&CompiledContext> for ContextCapsule {
    fn from(compiled: &CompiledContext) -> Self {
        Self {
            nodes: compiled
                .nodes
                .iter()
                .map(ContextCapsuleItem::from)
                .collect(),
        }
    }
}

impl ContextCapsule {
    /// Validate every item and enforce the default absolute model-context cap.
    pub fn validate_at(&self, now: DateTime<Utc>) -> Result<(), ContextCapsuleValidationError> {
        self.validate_at_with_budget(now, DEFAULT_CONTEXT_ABSOLUTE_BUDGET)
    }

    /// Validate every item and enforce an explicit absolute token ceiling.
    ///
    /// This variant lets callers using a non-default `ContextCompiler`
    /// validate a capsule against that compiler's configured ceiling.
    pub fn validate_at_with_budget(
        &self,
        now: DateTime<Utc>,
        absolute_budget: u32,
    ) -> Result<(), ContextCapsuleValidationError> {
        for item in &self.nodes {
            item.validate_at(now)?;
        }

        let used = self.token_cost();
        if used > absolute_budget {
            return Err(ContextCapsuleValidationError::TokenBudgetExceeded {
                used,
                absolute_budget,
            });
        }
        Ok(())
    }

    /// Return the conservative token estimate for the exact serialized items.
    pub fn token_cost(&self) -> u32 {
        self.nodes
            .iter()
            .fold(0_u32, |total, item| total.saturating_add(item.token_cost()))
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ContextCompileError {
    #[error("context candidate {node_id} appears more than once")]
    DuplicateCandidate { node_id: String },
    #[error("policy-required context {node_id} has an empty reason")]
    InvalidPolicyReason { node_id: String },
    #[error("required context {node_id} is invalid: {reason}")]
    InvalidRequiredContext { node_id: String, reason: String },
    #[error(
        "required context costs {used} tokens, exceeding the absolute ceiling {absolute_budget}"
    )]
    RequiredContextBudgetExceeded { used: u32, absolute_budget: u32 },
    #[error("shared retrieval query or document is invalid: {0}")]
    Retrieval(#[from] RetrievalError),
}

/// Failures while deriving an authenticated V2 context ranking.
///
/// Provider/query pairing is checked before the candidate iterator is touched.
/// Once candidate processing starts, every structural or provider failure
/// aborts the whole operation without returning a partial ranking.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ContextQueryRankingError {
    #[error("shared retrieval query or context document is invalid: {0}")]
    Retrieval(#[from] RetrievalError),
    #[error(
        "task query mode {mode} does not match the embedding provider presence ({provider_present})"
    )]
    ProviderModeMismatch {
        mode: RetrievalMode,
        provider_present: bool,
    },
    #[error("context ranking candidate {node_id} appears more than once")]
    DuplicateCandidate { node_id: String },
    #[error("context ranking candidate {node_id} is invalid: {reason}")]
    InvalidCandidate { node_id: String, reason: String },
    #[error("context ranking candidate id is {actual} bytes, exceeding the {maximum}-byte limit")]
    CandidateIdTooLong { actual: usize, maximum: usize },
    #[error(
        "context ranking candidate {node_id} summary is {actual} bytes, exceeding the {maximum}-byte limit"
    )]
    CandidateSummaryTooLong {
        node_id: String,
        actual: usize,
        maximum: usize,
    },
}

/// An authenticated, bounded V2 ranking derived only by `ditto-context`.
///
/// The plan deliberately has no public rank-component, directive, or ordering
/// fields and implements neither [`Serialize`] nor [`Deserialize`].  Callers
/// can clone a valid plan, but cannot deserialize or construct a substitute
/// ordering for [`ContextCompiler::compile_ranked_query`].
///
/// ```compile_fail
/// fn requires_serialize<T: serde::Serialize>() {}
/// requires_serialize::<ditto_context::ContextQueryRanking>();
/// ```
///
/// ```compile_fail
/// let _ = ditto_context::ContextQueryRanking {};
/// ```
#[derive(Clone)]
pub struct ContextQueryRanking {
    query: TaskQuery,
    evaluated_at: DateTime<Utc>,
    result_limit: ContextResultLimit,
    ranked: Vec<RankedQueryCandidate>,
    excluded: Vec<ContextExclusion>,
}

impl fmt::Debug for ContextQueryRanking {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ContextQueryRanking")
            .field("query_mode", &self.query.mode())
            .field("evaluated_at", &self.evaluated_at)
            .field("result_limit", &self.result_limit.get())
            .field("ranked_count", &self.ranked.len())
            .field("excluded_count", &self.excluded.len())
            .finish_non_exhaustive()
    }
}

#[derive(Clone)]
struct RankedQueryCandidate {
    node: ContextNode,
    exact: bool,
    embedding_similarity: Option<f32>,
    relevance_score: f32,
}

impl ContextQueryRanking {
    /// Derive a bounded context ranking from one already-validated shared
    /// query and plain projected nodes.
    ///
    /// Structurally invalid nodes fail closed. Disputed, expired, and
    /// not-yet-valid nodes are hard-filtered before document or provider work;
    /// active lexical misses are likewise excluded before provider work. Every
    /// remaining document is embedded in node-ID order, including exact
    /// matches, before the fixed ranking tuple and result limit are applied.
    pub fn new(
        query: &TaskQuery,
        candidates: impl IntoIterator<Item = ContextNode>,
        evaluated_at: DateTime<Utc>,
        result_limit: ContextResultLimit,
        provider: Option<&dyn EmbeddingProvider>,
    ) -> Result<Self, ContextQueryRankingError> {
        let mut budget = RetrievalWorkBudget::new();
        Self::new_with_budget(
            query,
            candidates,
            evaluated_at,
            result_limit,
            provider,
            &mut budget,
        )
    }

    /// Derive a ranking while sharing the caller's cumulative retrieval work
    /// envelope. Candidate nodes are bounded before retention; documents are
    /// then constructed, scored, optionally embedded, and dropped one at a
    /// time while at most `result_limit` ranked nodes are retained.
    pub fn new_with_budget(
        query: &TaskQuery,
        candidates: impl IntoIterator<Item = ContextNode>,
        evaluated_at: DateTime<Utc>,
        result_limit: ContextResultLimit,
        provider: Option<&dyn EmbeddingProvider>,
        budget: &mut RetrievalWorkBudget,
    ) -> Result<Self, ContextQueryRankingError> {
        validate_context_query_provider(query, provider)?;

        let mut bounded_candidates = Vec::new();
        let mut candidate_ids = HashSet::new();
        let mut first_candidate_error = None;
        let mut candidate_count = 0_usize;
        for candidate in candidates {
            candidate_count = candidate_count.saturating_add(1);
            CandidateCount::new(candidate_count)?;

            // Preserve the V2 count gate's precedence by remembering the
            // first bounded candidate error until the iterator is exhausted.
            // After an error, later nodes are counted and immediately dropped;
            // no attacker-sized field is retained merely to discover 10,001.
            if first_candidate_error.is_some() {
                continue;
            }
            let candidate_bytes = match validate_ranked_context_node(&candidate) {
                Ok(candidate_bytes) => candidate_bytes,
                Err(error) => {
                    first_candidate_error = Some(error);
                    continue;
                }
            };
            if !candidate_ids.insert(candidate.id.clone()) {
                first_candidate_error = Some(ContextQueryRankingError::DuplicateCandidate {
                    node_id: candidate.id.clone(),
                });
                continue;
            }
            if let Err(error) = budget.charge_candidate_bytes(candidate_bytes) {
                first_candidate_error = Some(error.into());
                continue;
            }
            bounded_candidates.push(candidate);
        }
        if let Some(error) = first_candidate_error {
            return Err(error);
        }

        bounded_candidates.sort_unstable_by(|left, right| left.id.cmp(&right.id));
        let mut ranked = Vec::with_capacity(result_limit.get());
        let mut excluded = Vec::new();
        for node in bounded_candidates {
            if !node.is_valid_at(evaluated_at) {
                excluded.push(ContextExclusion {
                    node_id: node.id,
                    reason: ContextExclusionReason::DisputedOrExpired,
                    detail: None,
                });
                continue;
            }

            let document_len = context_retrieval_document_len(&node)?;
            budget.charge_document_bytes(document_len)?;
            let document = context_retrieval_document(&node)?;
            let (relevance_score, exact) =
                v2_relevance_from_document_with_budget(&node, query, &document, budget)?;
            if !exact && relevance_score == 0.0 {
                excluded.push(ContextExclusion {
                    node_id: node.id,
                    reason: ContextExclusionReason::Irrelevant,
                    detail: None,
                });
                continue;
            }

            let embedding_similarity = if let Some(provider) = provider {
                let query_vector = query
                    .query_embedding()
                    .ok_or(RetrievalError::EmbeddingNotConfigured)?;
                let vector = query.embed_document_with_budget(provider, &document, budget)?;
                Some(cosine_similarity(query_vector, &vector)?)
            } else {
                None
            };
            ranked.push(RankedQueryCandidate {
                node,
                exact,
                embedding_similarity,
                relevance_score,
            });
            ranked.sort_unstable_by(ranked_query_candidate_order);
            if ranked.len() > result_limit.get() {
                ranked.pop();
            }
        }

        excluded.sort_unstable_by(|left, right| left.node_id.cmp(&right.node_id));

        Ok(Self {
            query: query.clone(),
            evaluated_at,
            result_limit,
            ranked,
            excluded,
        })
    }

    /// Number of eligible results retained after the authenticated result
    /// limit. This reveals no rank components or mutable ordering surface.
    pub fn len(&self) -> usize {
        self.ranked.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ranked.is_empty()
    }
}

fn validate_ranked_context_node(node: &ContextNode) -> Result<usize, ContextQueryRankingError> {
    if node.id.len() > MAX_CONTEXT_NODE_ID_BYTES {
        return Err(ContextQueryRankingError::CandidateIdTooLong {
            actual: node.id.len(),
            maximum: MAX_CONTEXT_NODE_ID_BYTES,
        });
    }
    if node.summary.len() > MAX_CONTEXT_NODE_SUMMARY_BYTES {
        return Err(ContextQueryRankingError::CandidateSummaryTooLong {
            node_id: node.id.clone(),
            actual: node.summary.len(),
            maximum: MAX_CONTEXT_NODE_SUMMARY_BYTES,
        });
    }
    node.validate()
        .map_err(|error| invalid_ranked_candidate(&node.id, error.to_string()))?;
    validate_ranked_reference_list(
        node,
        "source_event_ids",
        &node.source_event_ids,
        1,
        MAX_CONTEXT_SOURCE_EVENT_IDS,
        false,
    )?;
    validate_ranked_reference_list(
        node,
        "supersedes",
        &node.supersedes,
        0,
        MAX_CONTEXT_SUPERSEDES,
        true,
    )?;

    let serialized = serde_json::to_vec(node)
        .map_err(|_| invalid_ranked_candidate(&node.id, "context node is not serializable"))?;
    if serialized.len() > MAX_SERIALIZED_CONTEXT_NODE_BYTES {
        return Err(invalid_ranked_candidate(
            &node.id,
            format!(
                "serialized context node is {} bytes, exceeding the {}-byte limit",
                serialized.len(),
                MAX_SERIALIZED_CONTEXT_NODE_BYTES
            ),
        ));
    }
    Ok(serialized.len())
}

fn validate_ranked_reference_list(
    node: &ContextNode,
    field: &'static str,
    values: &[String],
    minimum: usize,
    maximum: usize,
    reject_self: bool,
) -> Result<(), ContextQueryRankingError> {
    if !(minimum..=maximum).contains(&values.len()) {
        return Err(invalid_ranked_candidate(
            &node.id,
            format!(
                "{field} contains {} entries, outside the inclusive range {minimum}..={maximum}",
                values.len()
            ),
        ));
    }

    let mut seen = HashSet::with_capacity(values.len());
    for (index, value) in values.iter().enumerate() {
        if value.trim().is_empty() {
            return Err(invalid_ranked_candidate(
                &node.id,
                format!("{field} reference at index {index} is empty"),
            ));
        }
        if value.len() > MAX_CONTEXT_REFERENCE_ID_BYTES {
            return Err(invalid_ranked_candidate(
                &node.id,
                format!(
                    "{field} reference at index {index} is {} bytes, exceeding the {}-byte limit",
                    value.len(),
                    MAX_CONTEXT_REFERENCE_ID_BYTES
                ),
            ));
        }
        if !seen.insert(value.as_str()) {
            return Err(invalid_ranked_candidate(
                &node.id,
                format!("{field} contains duplicate reference {value}"),
            ));
        }
        if reject_self && value == &node.id {
            return Err(invalid_ranked_candidate(
                &node.id,
                "supersedes contains the node's own id",
            ));
        }
    }
    Ok(())
}

fn invalid_ranked_candidate(node_id: &str, reason: impl AsRef<str>) -> ContextQueryRankingError {
    debug_assert!(node_id.len() <= MAX_CONTEXT_NODE_ID_BYTES);
    ContextQueryRankingError::InvalidCandidate {
        node_id: node_id.to_owned(),
        reason: bounded_context_ranking_error_detail(reason.as_ref()),
    }
}

fn bounded_context_ranking_error_detail(detail: &str) -> String {
    if detail.len() <= MAX_CONTEXT_RANKING_ERROR_DETAIL_BYTES {
        return detail.to_owned();
    }
    let mut end = MAX_CONTEXT_RANKING_ERROR_DETAIL_BYTES;
    while !detail.is_char_boundary(end) {
        end -= 1;
    }
    detail[..end].to_owned()
}

fn validate_context_query_provider(
    query: &TaskQuery,
    provider: Option<&dyn EmbeddingProvider>,
) -> Result<(), ContextQueryRankingError> {
    let provider_present = provider.is_some();
    let pairing_valid = match query.mode() {
        RetrievalMode::LexicalOnly => !provider_present,
        RetrievalMode::Embedded => provider_present,
    };
    if pairing_valid {
        Ok(())
    } else {
        Err(ContextQueryRankingError::ProviderModeMismatch {
            mode: query.mode(),
            provider_present,
        })
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ContextCompiler {
    pub default_budget: u32,
    pub absolute_budget: u32,
}

impl Default for ContextCompiler {
    fn default() -> Self {
        Self {
            default_budget: DEFAULT_CONTEXT_TOKEN_BUDGET,
            absolute_budget: DEFAULT_CONTEXT_ABSOLUTE_BUDGET,
        }
    }
}

impl ContextCompiler {
    pub fn compile(
        &self,
        signature: &TaskSignature,
        candidates: impl IntoIterator<Item = ContextCandidate>,
        token_budget: Option<u32>,
        now: DateTime<Utc>,
    ) -> Result<CompiledContext, ContextCompileError> {
        let token_budget = token_budget.unwrap_or(self.default_budget);
        let selection_budget = token_budget.min(self.absolute_budget);
        let query_tokens = tokenize(&signature.searchable_text());
        let candidates = candidates.into_iter().collect::<Vec<_>>();
        let mut candidate_ids = HashSet::with_capacity(candidates.len());
        for candidate in &candidates {
            if !candidate_ids.insert(candidate.node.id.clone()) {
                return Err(ContextCompileError::DuplicateCandidate {
                    node_id: candidate.node.id.clone(),
                });
            }
            if candidate.directive.validate().is_err() {
                return Err(ContextCompileError::InvalidPolicyReason {
                    node_id: candidate.node.id.clone(),
                });
            }
        }
        let mut required = Vec::new();
        let mut ranked = Vec::new();
        let mut excluded = Vec::new();

        for candidate in candidates {
            let node_id = candidate.node.id.clone();
            if let Err(error) = candidate.node.validate() {
                if candidate.directive.is_required() {
                    return Err(ContextCompileError::InvalidRequiredContext {
                        node_id,
                        reason: error.to_string(),
                    });
                }
                excluded.push(ContextExclusion {
                    node_id,
                    reason: ContextExclusionReason::Invalid,
                    detail: Some(error.to_string()),
                });
                continue;
            }
            if !candidate.node.is_valid_at(now) {
                if candidate.directive.is_required() {
                    return Err(ContextCompileError::InvalidRequiredContext {
                        node_id,
                        reason: invalid_at_reason(&candidate.node, now).into(),
                    });
                }
                excluded.push(ContextExclusion {
                    node_id,
                    reason: ContextExclusionReason::DisputedOrExpired,
                    detail: None,
                });
                continue;
            }

            let node = candidate.node;
            let capsule_item = ContextCapsuleItem::from(&node);
            let token_cost = capsule_item.token_cost();
            let score = relevance_score(&node, &query_tokens);
            if candidate.directive.is_required() {
                required.push(PreparedCandidate {
                    directive: candidate.directive,
                    score,
                    token_cost,
                    node,
                });
            } else if score > 0.0 {
                ranked.push(PreparedCandidate {
                    directive: candidate.directive,
                    score,
                    token_cost,
                    node,
                });
            } else {
                excluded.push(ContextExclusion {
                    node_id,
                    reason: ContextExclusionReason::Irrelevant,
                    detail: None,
                });
            }
        }

        required.sort_by(candidate_order);
        ranked.sort_by(candidate_order);

        let required_cost = required.iter().fold(0_u32, |total, candidate| {
            total.saturating_add(candidate.token_cost)
        });
        if required_cost > self.absolute_budget {
            return Err(ContextCompileError::RequiredContextBudgetExceeded {
                used: required_cost,
                absolute_budget: self.absolute_budget,
            });
        }

        let mut selected = Vec::new();
        let mut included = Vec::new();
        let mut used = 0_u32;

        for candidate in required {
            used = used.saturating_add(candidate.token_cost);
            included.push(receipt_entry(
                &candidate.node,
                &candidate.directive,
                candidate.score,
                candidate.token_cost,
            ));
            selected.push(candidate.node);
        }

        for candidate in ranked {
            if used.saturating_add(candidate.token_cost) > selection_budget {
                excluded.push(ContextExclusion {
                    node_id: candidate.node.id,
                    reason: ContextExclusionReason::TokenBudget,
                    detail: None,
                });
                continue;
            }
            used += candidate.token_cost;
            included.push(receipt_entry(
                &candidate.node,
                &candidate.directive,
                candidate.score,
                candidate.token_cost,
            ));
            selected.push(candidate.node);
        }

        Ok(CompiledContext {
            nodes: selected,
            receipt: ContextReceipt {
                included,
                excluded,
                total_token_cost: used,
                token_budget,
                absolute_budget: self.absolute_budget,
                over_soft_budget: used > token_budget,
            },
        })
    }

    /// Validate a compiled context and its model-facing capsule against the
    /// compiler's deterministic selection contract.
    ///
    /// The compiler is the authority for ranking, tokenization, receipt
    /// grammar, and token accounting. Keeping this check here lets runtime and
    /// replay use the same pure implementation without persisting ephemeral
    /// directives or candidate inputs.
    pub fn validate_compiled(
        &self,
        signature: &TaskSignature,
        compiled: &CompiledContext,
        capsule: &ContextCapsule,
        token_budget: Option<u32>,
        accepted_at: DateTime<Utc>,
    ) -> Result<(), CompiledContextValidationError> {
        capsule
            .validate_at_with_budget(accepted_at, self.absolute_budget)
            .map_err(CompiledContextValidationError::InvalidCapsule)?;

        if ContextCapsule::from(compiled) != *capsule {
            return Err(CompiledContextValidationError::CapsuleMismatch);
        }
        if compiled.nodes.len() != compiled.receipt.included.len()
            || compiled.nodes.len() != capsule.nodes.len()
        {
            return Err(CompiledContextValidationError::NodeReceiptLengthMismatch {
                nodes: compiled.nodes.len(),
                included: compiled.receipt.included.len(),
                capsule: capsule.nodes.len(),
            });
        }

        let expected_token_budget = token_budget.unwrap_or(self.default_budget);
        if compiled.receipt.token_budget != expected_token_budget
            || compiled.receipt.absolute_budget != self.absolute_budget
        {
            return Err(CompiledContextValidationError::BudgetMismatch {
                expected_token_budget,
                actual_token_budget: compiled.receipt.token_budget,
                expected_absolute_budget: self.absolute_budget,
                actual_absolute_budget: compiled.receipt.absolute_budget,
            });
        }

        let query_tokens = tokenize(&signature.searchable_text());
        let mut total_token_cost = 0_u32;
        let selection_budget = expected_token_budget.min(self.absolute_budget);
        let mut required_token_cost = 0_u32;
        let mut selection_token_cost = 0_u32;
        let mut seen_ids = HashSet::with_capacity(
            compiled
                .receipt
                .included
                .len()
                .saturating_add(compiled.receipt.excluded.len()),
        );

        for entry in &compiled.receipt.excluded {
            if !seen_ids.insert(entry.node_id.clone()) {
                return Err(CompiledContextValidationError::DuplicateReceiptId {
                    node_id: entry.node_id.clone(),
                });
            }
        }

        let mut previous_key = None;
        for ((node, receipt), capsule_item) in compiled
            .nodes
            .iter()
            .zip(&compiled.receipt.included)
            .zip(&capsule.nodes)
        {
            if !seen_ids.insert(receipt.node_id.clone()) {
                return Err(CompiledContextValidationError::DuplicateReceiptId {
                    node_id: receipt.node_id.clone(),
                });
            }

            node.validate()
                .map_err(|error| CompiledContextValidationError::InvalidNode {
                    node_id: node.id.clone(),
                    reason: error.to_string(),
                })?;
            if !node.is_valid_at(accepted_at) {
                return Err(CompiledContextValidationError::NodeNotValidAt {
                    node_id: node.id.clone(),
                });
            }

            let expected_score = relevance_score(node, &query_tokens);
            let expected_token_cost = capsule_item.token_cost();
            let priority = receipt_reason_priority(&receipt.reason).ok_or_else(|| {
                CompiledContextValidationError::InvalidReceiptReason {
                    node_id: node.id.clone(),
                    reason: receipt.reason.clone(),
                }
            })?;

            if receipt.node_id != node.id
                || receipt.source_event_ids != node.source_event_ids
                || receipt.epistemic != node.epistemic
            {
                return Err(CompiledContextValidationError::ReceiptNodeMismatch {
                    node_id: node.id.clone(),
                });
            }
            if receipt.token_cost != expected_token_cost {
                return Err(CompiledContextValidationError::TokenCostMismatch {
                    node_id: node.id.clone(),
                    expected: expected_token_cost,
                    actual: receipt.token_cost,
                });
            }
            if receipt.score.to_bits() != expected_score.to_bits() {
                return Err(CompiledContextValidationError::ScoreMismatch {
                    node_id: node.id.clone(),
                });
            }
            if receipt.reason == "task-relevance" && receipt.score <= 0.0 {
                return Err(CompiledContextValidationError::NonPositiveTaskRelevance {
                    node_id: node.id.clone(),
                });
            }

            let key = (priority, receipt.score, receipt.node_id.as_str());
            if let Some((previous_priority, previous_score, previous_id)) = previous_key
                && (priority > previous_priority
                    || (priority == previous_priority
                        && (receipt.score.total_cmp(&previous_score).is_gt()
                            || (receipt.score.to_bits() == previous_score.to_bits()
                                && receipt.node_id.as_str() < previous_id))))
            {
                return Err(CompiledContextValidationError::NonCanonicalOrder {
                    node_id: node.id.clone(),
                });
            }

            if priority > 0 {
                required_token_cost = required_token_cost.saturating_add(receipt.token_cost);
                if required_token_cost > self.absolute_budget {
                    return Err(
                        CompiledContextValidationError::RequiredSelectionBudgetExceeded {
                            used: required_token_cost,
                            absolute_budget: self.absolute_budget,
                        },
                    );
                }
            } else if required_token_cost > selection_budget
                || selection_token_cost.saturating_add(receipt.token_cost) > selection_budget
            {
                return Err(CompiledContextValidationError::SelectionBudgetExceeded {
                    node_id: node.id.clone(),
                    used: selection_token_cost.saturating_add(receipt.token_cost),
                    token_budget: selection_budget,
                });
            }
            selection_token_cost = selection_token_cost.saturating_add(receipt.token_cost);
            previous_key = Some(key);
            total_token_cost = total_token_cost.saturating_add(receipt.token_cost);
        }

        if compiled.receipt.total_token_cost != total_token_cost
            || capsule.token_cost() != total_token_cost
            || total_token_cost > self.absolute_budget
            || compiled.receipt.over_soft_budget != (total_token_cost > expected_token_budget)
        {
            return Err(CompiledContextValidationError::TokenAccountingMismatch {
                expected: total_token_cost,
                actual: compiled.receipt.total_token_cost,
            });
        }

        Ok(())
    }

    /// Compile a V2 query against context candidates.
    ///
    /// The query is built by the shared retrieval crate before it reaches this
    /// method.  Candidate counting therefore happens before any candidate is
    /// validated, documented, scored, or selected, and a V2 scan-limit error
    /// can never be accompanied by a partial compiled result.
    pub fn compile_query(
        &self,
        query: &TaskQuery,
        candidates: impl IntoIterator<Item = ContextCandidate>,
        token_budget: Option<u32>,
        now: DateTime<Utc>,
    ) -> Result<CompiledContext, ContextCompileError> {
        let mut bounded_candidates = Vec::new();
        for candidate in candidates {
            bounded_candidates.push(candidate);
            if let Err(error) = CandidateCount::new(bounded_candidates.len()) {
                return Err(error.into());
            }
        }
        let candidates = bounded_candidates;

        let token_budget = token_budget.unwrap_or(self.default_budget);
        let selection_budget = token_budget.min(self.absolute_budget);
        let mut candidate_ids = HashSet::with_capacity(candidates.len());
        for candidate in &candidates {
            if !candidate_ids.insert(candidate.node.id.clone()) {
                return Err(ContextCompileError::DuplicateCandidate {
                    node_id: candidate.node.id.clone(),
                });
            }
            if candidate.directive.validate().is_err() {
                return Err(ContextCompileError::InvalidPolicyReason {
                    node_id: candidate.node.id.clone(),
                });
            }
        }

        let mut required = Vec::new();
        let mut ranked = Vec::new();
        let mut excluded = Vec::new();

        for candidate in candidates {
            let node_id = candidate.node.id.clone();
            if let Err(error) = candidate.node.validate() {
                if candidate.directive.is_required() {
                    return Err(ContextCompileError::InvalidRequiredContext {
                        node_id,
                        reason: error.to_string(),
                    });
                }
                excluded.push(ContextExclusion {
                    node_id,
                    reason: ContextExclusionReason::Invalid,
                    detail: Some(error.to_string()),
                });
                continue;
            }
            if !candidate.node.is_valid_at(now) {
                if candidate.directive.is_required() {
                    return Err(ContextCompileError::InvalidRequiredContext {
                        node_id,
                        reason: invalid_at_reason(&candidate.node, now).into(),
                    });
                }
                excluded.push(ContextExclusion {
                    node_id,
                    reason: ContextExclusionReason::DisputedOrExpired,
                    detail: None,
                });
                continue;
            }

            let node = candidate.node;
            let capsule_item = ContextCapsuleItem::from(&node);
            let token_cost = capsule_item.token_cost();
            let (score, exact) = v2_relevance(&node, query)?;
            if candidate.directive.is_required() {
                required.push(V2PreparedCandidate {
                    directive: candidate.directive,
                    exact,
                    score,
                    token_cost,
                    node,
                });
            } else if exact || score > 0.0 {
                ranked.push(V2PreparedCandidate {
                    directive: candidate.directive,
                    exact,
                    score,
                    token_cost,
                    node,
                });
            } else {
                excluded.push(ContextExclusion {
                    node_id,
                    reason: ContextExclusionReason::Irrelevant,
                    detail: None,
                });
            }
        }

        required.sort_by(v2_candidate_order);
        ranked.sort_by(v2_candidate_order);

        let required_cost = required.iter().fold(0_u32, |total, candidate| {
            total.saturating_add(candidate.token_cost)
        });
        if required_cost > self.absolute_budget {
            return Err(ContextCompileError::RequiredContextBudgetExceeded {
                used: required_cost,
                absolute_budget: self.absolute_budget,
            });
        }

        let mut selected = Vec::new();
        let mut included = Vec::new();
        let mut used = 0_u32;

        for candidate in required {
            used = used.saturating_add(candidate.token_cost);
            included.push(receipt_entry(
                &candidate.node,
                &candidate.directive,
                candidate.score,
                candidate.token_cost,
            ));
            selected.push(candidate.node);
        }

        for candidate in ranked {
            if used.saturating_add(candidate.token_cost) > selection_budget {
                excluded.push(ContextExclusion {
                    node_id: candidate.node.id,
                    reason: ContextExclusionReason::TokenBudget,
                    detail: None,
                });
                continue;
            }
            used += candidate.token_cost;
            included.push(receipt_entry(
                &candidate.node,
                &candidate.directive,
                candidate.score,
                candidate.token_cost,
            ));
            selected.push(candidate.node);
        }

        Ok(CompiledContext {
            nodes: selected,
            receipt: ContextReceipt {
                included,
                excluded,
                total_token_cost: used,
                token_budget,
                absolute_budget: self.absolute_budget,
                over_soft_budget: used > token_budget,
            },
        })
    }

    /// Validate a compiled context made from one prebuilt V2 query.
    ///
    /// This is the V2 counterpart to [`Self::validate_compiled`].  It keeps the
    /// legacy validation path separate while re-deriving the fixed retrieval
    /// document, normalized exactness, lexical score, receipt order, and token
    /// accounting from the shared query.
    pub fn validate_compiled_query(
        &self,
        query: &TaskQuery,
        compiled: &CompiledContext,
        capsule: &ContextCapsule,
        token_budget: Option<u32>,
        accepted_at: DateTime<Utc>,
    ) -> Result<(), CompiledContextValidationError> {
        capsule
            .validate_at_with_budget(accepted_at, self.absolute_budget)
            .map_err(CompiledContextValidationError::InvalidCapsule)?;

        if ContextCapsule::from(compiled) != *capsule {
            return Err(CompiledContextValidationError::CapsuleMismatch);
        }
        if compiled.nodes.len() != compiled.receipt.included.len()
            || compiled.nodes.len() != capsule.nodes.len()
        {
            return Err(CompiledContextValidationError::NodeReceiptLengthMismatch {
                nodes: compiled.nodes.len(),
                included: compiled.receipt.included.len(),
                capsule: capsule.nodes.len(),
            });
        }

        let expected_token_budget = token_budget.unwrap_or(self.default_budget);
        if compiled.receipt.token_budget != expected_token_budget
            || compiled.receipt.absolute_budget != self.absolute_budget
        {
            return Err(CompiledContextValidationError::BudgetMismatch {
                expected_token_budget,
                actual_token_budget: compiled.receipt.token_budget,
                expected_absolute_budget: self.absolute_budget,
                actual_absolute_budget: compiled.receipt.absolute_budget,
            });
        }

        let selection_budget = expected_token_budget.min(self.absolute_budget);
        let mut total_token_cost = 0_u32;
        let mut required_token_cost = 0_u32;
        let mut selection_token_cost = 0_u32;
        let mut seen_ids = HashSet::with_capacity(
            compiled
                .receipt
                .included
                .len()
                .saturating_add(compiled.receipt.excluded.len()),
        );

        for entry in &compiled.receipt.excluded {
            if !seen_ids.insert(entry.node_id.clone()) {
                return Err(CompiledContextValidationError::DuplicateReceiptId {
                    node_id: entry.node_id.clone(),
                });
            }
        }

        let mut previous_key: Option<(u8, bool, f32, String)> = None;
        for ((node, receipt), capsule_item) in compiled
            .nodes
            .iter()
            .zip(&compiled.receipt.included)
            .zip(&capsule.nodes)
        {
            if !seen_ids.insert(receipt.node_id.clone()) {
                return Err(CompiledContextValidationError::DuplicateReceiptId {
                    node_id: receipt.node_id.clone(),
                });
            }

            node.validate()
                .map_err(|error| CompiledContextValidationError::InvalidNode {
                    node_id: node.id.clone(),
                    reason: error.to_string(),
                })?;
            if !node.is_valid_at(accepted_at) {
                return Err(CompiledContextValidationError::NodeNotValidAt {
                    node_id: node.id.clone(),
                });
            }

            let (expected_score, exact) = v2_relevance(node, query)?;
            let expected_token_cost = capsule_item.token_cost();
            let priority = receipt_reason_priority(&receipt.reason).ok_or_else(|| {
                CompiledContextValidationError::InvalidReceiptReason {
                    node_id: node.id.clone(),
                    reason: receipt.reason.clone(),
                }
            })?;

            if receipt.node_id != node.id
                || receipt.source_event_ids != node.source_event_ids
                || receipt.epistemic != node.epistemic
            {
                return Err(CompiledContextValidationError::ReceiptNodeMismatch {
                    node_id: node.id.clone(),
                });
            }
            if receipt.token_cost != expected_token_cost {
                return Err(CompiledContextValidationError::TokenCostMismatch {
                    node_id: node.id.clone(),
                    expected: expected_token_cost,
                    actual: receipt.token_cost,
                });
            }
            if receipt.score.to_bits() != expected_score.to_bits() {
                return Err(CompiledContextValidationError::ScoreMismatch {
                    node_id: node.id.clone(),
                });
            }
            if receipt.reason == "task-relevance" && receipt.score <= 0.0 {
                return Err(CompiledContextValidationError::NonPositiveTaskRelevance {
                    node_id: node.id.clone(),
                });
            }

            if let Some((previous_priority, previous_exact, previous_score, previous_id)) =
                &previous_key
                && (priority > *previous_priority
                    || (priority == *previous_priority
                        && (exact && !*previous_exact
                            || (exact == *previous_exact
                                && (receipt.score.total_cmp(previous_score).is_gt()
                                    || (receipt.score.to_bits() == previous_score.to_bits()
                                        && receipt.node_id.as_str() < previous_id))))))
            {
                return Err(CompiledContextValidationError::NonCanonicalOrder {
                    node_id: node.id.clone(),
                });
            }

            if priority > 0 {
                required_token_cost = required_token_cost.saturating_add(receipt.token_cost);
                if required_token_cost > self.absolute_budget {
                    return Err(
                        CompiledContextValidationError::RequiredSelectionBudgetExceeded {
                            used: required_token_cost,
                            absolute_budget: self.absolute_budget,
                        },
                    );
                }
            } else if required_token_cost > selection_budget
                || selection_token_cost.saturating_add(receipt.token_cost) > selection_budget
            {
                return Err(CompiledContextValidationError::SelectionBudgetExceeded {
                    node_id: node.id.clone(),
                    used: selection_token_cost.saturating_add(receipt.token_cost),
                    token_budget: selection_budget,
                });
            }
            selection_token_cost = selection_token_cost.saturating_add(receipt.token_cost);
            previous_key = Some((priority, exact, receipt.score, receipt.node_id.clone()));
            total_token_cost = total_token_cost.saturating_add(receipt.token_cost);
        }

        if compiled.receipt.total_token_cost != total_token_cost
            || capsule.token_cost() != total_token_cost
            || total_token_cost > self.absolute_budget
            || compiled.receipt.over_soft_budget != (total_token_cost > expected_token_budget)
        {
            return Err(CompiledContextValidationError::TokenAccountingMismatch {
                expected: total_token_cost,
                actual: compiled.receipt.total_token_cost,
            });
        }

        Ok(())
    }

    /// Compile an opaque V2 ranking under the requested token budget.
    ///
    /// Ranked entries are considered greedily in their authenticated order.
    /// An entry that does not fit is recorded as token-budget excluded and the
    /// compiler continues to lower-ranked entries, allowing a smaller result
    /// to backfill without changing the ranking itself. Plain ranked-query
    /// inputs cannot acquire pinned or policy-required authority.
    pub fn compile_ranked_query(
        &self,
        ranking: &ContextQueryRanking,
        token_budget: Option<u32>,
    ) -> Result<CompiledContext, ContextCompileError> {
        Ok(self.compile_ranked_query_inner(ranking, token_budget))
    }

    /// Validate a compiled result against the exact opaque ranking that
    /// authorized it.
    ///
    /// The captured evaluation time, lexical receipt scores, embedded order,
    /// result limit, exclusion sequence, and exact capsule token costs all
    /// remain bound to the plan; validation never accepts a caller-supplied
    /// query, score, directive, or replacement order.
    pub fn validate_compiled_ranked_query(
        &self,
        ranking: &ContextQueryRanking,
        compiled: &CompiledContext,
        capsule: &ContextCapsule,
        token_budget: Option<u32>,
    ) -> Result<(), CompiledContextValidationError> {
        capsule
            .validate_at_with_budget(ranking.evaluated_at, self.absolute_budget)
            .map_err(CompiledContextValidationError::InvalidCapsule)?;

        if ContextCapsule::from(compiled) != *capsule {
            return Err(CompiledContextValidationError::CapsuleMismatch);
        }
        if compiled.nodes.len() != compiled.receipt.included.len()
            || compiled.nodes.len() != capsule.nodes.len()
        {
            return Err(CompiledContextValidationError::NodeReceiptLengthMismatch {
                nodes: compiled.nodes.len(),
                included: compiled.receipt.included.len(),
                capsule: capsule.nodes.len(),
            });
        }

        let expected_token_budget = token_budget.unwrap_or(self.default_budget);
        if compiled.receipt.token_budget != expected_token_budget
            || compiled.receipt.absolute_budget != self.absolute_budget
        {
            return Err(CompiledContextValidationError::BudgetMismatch {
                expected_token_budget,
                actual_token_budget: compiled.receipt.token_budget,
                expected_absolute_budget: self.absolute_budget,
                actual_absolute_budget: compiled.receipt.absolute_budget,
            });
        }

        let expected = self.compile_ranked_query_inner(ranking, token_budget);
        if compiled.nodes.len() != expected.nodes.len() {
            return Err(
                CompiledContextValidationError::RankedSelectionLengthMismatch {
                    expected: expected.nodes.len(),
                    actual: compiled.nodes.len(),
                },
            );
        }

        let mut seen_ids = HashSet::with_capacity(
            compiled
                .receipt
                .included
                .len()
                .saturating_add(compiled.receipt.excluded.len()),
        );
        for entry in &compiled.receipt.excluded {
            if !seen_ids.insert(entry.node_id.clone()) {
                return Err(CompiledContextValidationError::DuplicateReceiptId {
                    node_id: entry.node_id.clone(),
                });
            }
        }

        for ((node, receipt), (expected_node, expected_receipt)) in compiled
            .nodes
            .iter()
            .zip(&compiled.receipt.included)
            .zip(expected.nodes.iter().zip(&expected.receipt.included))
        {
            if !seen_ids.insert(receipt.node_id.clone()) {
                return Err(CompiledContextValidationError::DuplicateReceiptId {
                    node_id: receipt.node_id.clone(),
                });
            }
            if node.id != expected_node.id {
                return Err(CompiledContextValidationError::NonCanonicalOrder {
                    node_id: node.id.clone(),
                });
            }
            if node != expected_node
                || receipt.node_id != node.id
                || receipt.node_id != expected_receipt.node_id
                || receipt.source_event_ids != node.source_event_ids
                || receipt.source_event_ids != expected_receipt.source_event_ids
                || receipt.epistemic != node.epistemic
                || receipt.epistemic != expected_receipt.epistemic
            {
                return Err(CompiledContextValidationError::ReceiptNodeMismatch {
                    node_id: node.id.clone(),
                });
            }
            if receipt.reason != expected_receipt.reason {
                return Err(CompiledContextValidationError::InvalidReceiptReason {
                    node_id: node.id.clone(),
                    reason: receipt.reason.clone(),
                });
            }
            if receipt.score.to_bits() != expected_receipt.score.to_bits() {
                return Err(CompiledContextValidationError::ScoreMismatch {
                    node_id: node.id.clone(),
                });
            }
            if receipt.token_cost != expected_receipt.token_cost {
                return Err(CompiledContextValidationError::TokenCostMismatch {
                    node_id: node.id.clone(),
                    expected: expected_receipt.token_cost,
                    actual: receipt.token_cost,
                });
            }
        }

        if compiled.receipt.excluded.len() != expected.receipt.excluded.len() {
            return Err(CompiledContextValidationError::ExclusionReceiptMismatch {
                index: compiled
                    .receipt
                    .excluded
                    .len()
                    .min(expected.receipt.excluded.len()),
            });
        }
        for (index, (actual, expected)) in compiled
            .receipt
            .excluded
            .iter()
            .zip(&expected.receipt.excluded)
            .enumerate()
        {
            if actual != expected {
                return Err(CompiledContextValidationError::ExclusionReceiptMismatch { index });
            }
        }

        if compiled.receipt.total_token_cost != expected.receipt.total_token_cost
            || compiled.receipt.over_soft_budget != expected.receipt.over_soft_budget
            || capsule.token_cost() != expected.receipt.total_token_cost
        {
            return Err(CompiledContextValidationError::TokenAccountingMismatch {
                expected: expected.receipt.total_token_cost,
                actual: compiled.receipt.total_token_cost,
            });
        }

        Ok(())
    }

    fn compile_ranked_query_inner(
        &self,
        ranking: &ContextQueryRanking,
        token_budget: Option<u32>,
    ) -> CompiledContext {
        let token_budget = token_budget.unwrap_or(self.default_budget);
        let selection_budget = token_budget.min(self.absolute_budget);
        let mut selected = Vec::new();
        let mut included = Vec::new();
        let mut excluded = ranking.excluded.clone();
        let mut used = 0_u32;

        for candidate in &ranking.ranked {
            let token_cost = ContextCapsuleItem::from(&candidate.node).token_cost();
            if used.saturating_add(token_cost) > selection_budget {
                excluded.push(ContextExclusion {
                    node_id: candidate.node.id.clone(),
                    reason: ContextExclusionReason::TokenBudget,
                    detail: None,
                });
                continue;
            }

            used = used.saturating_add(token_cost);
            included.push(ContextReceiptEntry {
                node_id: candidate.node.id.clone(),
                source_event_ids: candidate.node.source_event_ids.clone(),
                epistemic: candidate.node.epistemic,
                reason: "task-relevance".into(),
                score: candidate.relevance_score,
                token_cost,
            });
            selected.push(candidate.node.clone());
        }

        CompiledContext {
            nodes: selected,
            receipt: ContextReceipt {
                included,
                excluded,
                total_token_cost: used,
                token_budget,
                absolute_budget: self.absolute_budget,
                over_soft_budget: used > token_budget,
            },
        }
    }
}

#[derive(Debug, Error, PartialEq)]
pub enum CompiledContextValidationError {
    #[error("compiled context capsule is invalid: {0}")]
    InvalidCapsule(ContextCapsuleValidationError),
    #[error("compiled context, receipt, and capsule do not describe the same nodes")]
    CapsuleMismatch,
    #[error(
        "compiled context node/receipt lengths differ (nodes={nodes}, included={included}, capsule={capsule})"
    )]
    NodeReceiptLengthMismatch {
        nodes: usize,
        included: usize,
        capsule: usize,
    },
    #[error("compiled context node {node_id} is invalid: {reason}")]
    InvalidNode { node_id: String, reason: String },
    #[error("compiled context node {node_id} is not valid at the acceptance time")]
    NodeNotValidAt { node_id: String },
    #[error("compiled context receipt id {node_id} occurs more than once")]
    DuplicateReceiptId { node_id: String },
    #[error("compiled context receipt for node {node_id} does not match the node")]
    ReceiptNodeMismatch { node_id: String },
    #[error(
        "compiled context receipt for node {node_id} has token cost {actual}, expected {expected}"
    )]
    TokenCostMismatch {
        node_id: String,
        expected: u32,
        actual: u32,
    },
    #[error("compiled context receipt for node {node_id} has a non-canonical score")]
    ScoreMismatch { node_id: String },
    #[error("compiled context receipt for node {node_id} has a non-positive task-relevance score")]
    NonPositiveTaskRelevance { node_id: String },
    #[error("compiled context receipt for node {node_id} has an invalid reason {reason:?}")]
    InvalidReceiptReason { node_id: String, reason: String },
    #[error("compiled context receipt is not in canonical order at node {node_id}")]
    NonCanonicalOrder { node_id: String },
    #[error(
        "compiled ranked context selected {actual} nodes, but the authenticated plan selects {expected}"
    )]
    RankedSelectionLengthMismatch { expected: usize, actual: usize },
    #[error("compiled ranked context exclusion at index {index} is not canonical")]
    ExclusionReceiptMismatch { index: usize },
    #[error(
        "required compiled context costs {used} tokens, exceeding the absolute ceiling {absolute_budget}"
    )]
    RequiredSelectionBudgetExceeded { used: u32, absolute_budget: u32 },
    #[error(
        "ranked compiled context node {node_id} would exceed the selection budget ({used} > {token_budget})"
    )]
    SelectionBudgetExceeded {
        node_id: String,
        used: u32,
        token_budget: u32,
    },
    #[error(
        "compiled context budget differs from the compiler (token={actual_token_budget}, expected={expected_token_budget}; absolute={actual_absolute_budget}, expected={expected_absolute_budget})"
    )]
    BudgetMismatch {
        expected_token_budget: u32,
        actual_token_budget: u32,
        expected_absolute_budget: u32,
        actual_absolute_budget: u32,
    },
    #[error(
        "compiled context token accounting is inconsistent (actual={actual}, expected={expected})"
    )]
    TokenAccountingMismatch { expected: u32, actual: u32 },
    #[error("shared retrieval query or document is invalid: {0}")]
    Retrieval(#[from] RetrievalError),
}

struct PreparedCandidate {
    directive: ContextDirective,
    score: f32,
    token_cost: u32,
    node: ContextNode,
}

struct V2PreparedCandidate {
    directive: ContextDirective,
    exact: bool,
    score: f32,
    token_cost: u32,
    node: ContextNode,
}

fn candidate_order(left: &PreparedCandidate, right: &PreparedCandidate) -> std::cmp::Ordering {
    right
        .directive
        .priority()
        .cmp(&left.directive.priority())
        .then_with(|| right.score.total_cmp(&left.score))
        .then_with(|| left.node.id.cmp(&right.node.id))
}

fn v2_candidate_order(
    left: &V2PreparedCandidate,
    right: &V2PreparedCandidate,
) -> std::cmp::Ordering {
    right
        .directive
        .priority()
        .cmp(&left.directive.priority())
        .then_with(|| right.exact.cmp(&left.exact))
        .then_with(|| right.score.total_cmp(&left.score))
        .then_with(|| left.node.id.cmp(&right.node.id))
}

fn ranked_query_candidate_order(
    left: &RankedQueryCandidate,
    right: &RankedQueryCandidate,
) -> std::cmp::Ordering {
    right
        .exact
        .cmp(&left.exact)
        .then_with(
            || match (left.embedding_similarity, right.embedding_similarity) {
                (Some(left_similarity), Some(right_similarity)) => {
                    right_similarity.total_cmp(&left_similarity)
                }
                (None, None) => std::cmp::Ordering::Equal,
                // A ranking has one query mode, so mixed variants cannot be
                // constructed through the public API. Keep the comparator total
                // and fail-closed toward the embedded entry if that invariant is
                // ever changed internally.
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
            },
        )
        .then_with(|| right.relevance_score.total_cmp(&left.relevance_score))
        .then_with(|| left.node.id.cmp(&right.node.id))
}

fn receipt_entry(
    node: &ContextNode,
    directive: &ContextDirective,
    score: f32,
    token_cost: u32,
) -> ContextReceiptEntry {
    ContextReceiptEntry {
        node_id: node.id.clone(),
        source_event_ids: node.source_event_ids.clone(),
        epistemic: node.epistemic,
        reason: directive.receipt_reason(),
        score,
        token_cost,
    }
}

fn receipt_reason_priority(reason: &str) -> Option<u8> {
    match reason {
        "task-relevance" => Some(0),
        "user-pinned" => Some(1),
        _ if reason
            .strip_prefix("policy-required: ")
            .is_some_and(|policy_reason| !policy_reason.trim().is_empty()) =>
        {
            Some(2)
        }
        _ => None,
    }
}

fn relevance_score(node: &ContextNode, query_tokens: &HashSet<String>) -> f32 {
    let node_tokens = tokenize(&node.summary);
    let overlap = if query_tokens.is_empty() {
        0.0
    } else {
        query_tokens.intersection(&node_tokens).count() as f32 / query_tokens.len() as f32
    };

    if overlap == 0.0 {
        return 0.0;
    }

    let authority = match (node.origin, node.epistemic) {
        (ContextOrigin::User, EpistemicStatus::Verified | EpistemicStatus::Asserted) => 1.0,
        (_, EpistemicStatus::Verified) => 0.8,
        (_, EpistemicStatus::Asserted) => 0.5,
        (_, EpistemicStatus::Inferred) => 0.2,
        (_, EpistemicStatus::Disputed) => -10.0,
    };

    overlap * 5.0 + authority + node.confidence
}

fn v2_relevance(node: &ContextNode, query: &TaskQuery) -> Result<(f32, bool), RetrievalError> {
    let document = context_retrieval_document(node)?;
    v2_relevance_from_document(node, query, &document)
}

fn v2_relevance_from_document(
    node: &ContextNode,
    query: &TaskQuery,
    document: &RetrievalDocument,
) -> Result<(f32, bool), RetrievalError> {
    let lexical_overlap = query.lexical_overlap(document);
    let exact = query.matches_exact_term(&node.id)?;

    if !exact && lexical_overlap == 0.0 {
        return Ok((0.0, false));
    }

    let authority = match (node.origin, node.epistemic) {
        (ContextOrigin::User, EpistemicStatus::Verified | EpistemicStatus::Asserted) => 1.0,
        (_, EpistemicStatus::Verified) => 0.8,
        (_, EpistemicStatus::Asserted) => 0.5,
        (_, EpistemicStatus::Inferred) => 0.2,
        (_, EpistemicStatus::Disputed) => -10.0,
    };

    Ok((lexical_overlap * 5.0 + authority + node.confidence, exact))
}

fn v2_relevance_from_document_with_budget(
    node: &ContextNode,
    query: &TaskQuery,
    document: &RetrievalDocument,
    budget: &mut RetrievalWorkBudget,
) -> Result<(f32, bool), RetrievalError> {
    let lexical_overlap = query.lexical_overlap_with_budget(document, budget)?;
    budget.charge_lexical_bytes(node.id.len())?;
    let exact = query.matches_exact_term(&node.id)?;

    if !exact && lexical_overlap == 0.0 {
        return Ok((0.0, false));
    }

    let authority = match (node.origin, node.epistemic) {
        (ContextOrigin::User, EpistemicStatus::Verified | EpistemicStatus::Asserted) => 1.0,
        (_, EpistemicStatus::Verified) => 0.8,
        (_, EpistemicStatus::Asserted) => 0.5,
        (_, EpistemicStatus::Inferred) => 0.2,
        (_, EpistemicStatus::Disputed) => -10.0,
    };

    Ok((lexical_overlap * 5.0 + authority + node.confidence, exact))
}

fn invalid_at_reason(node: &ContextNode, now: DateTime<Utc>) -> &'static str {
    if node.epistemic == EpistemicStatus::Disputed {
        "epistemic status is disputed"
    } else if node
        .valid_until
        .as_ref()
        .is_some_and(|valid_until| valid_until <= &now)
    {
        "validity window has expired"
    } else if node
        .valid_from
        .as_ref()
        .is_some_and(|valid_from| valid_from > &now)
    {
        "validity window has not started"
    } else {
        "not valid at the requested time"
    }
}

fn estimate_serialized_tokens(serialized_len: usize, fixed_overhead: u32) -> u32 {
    let estimate = serialized_len.div_ceil(4).max(1);
    u32::try_from(estimate)
        .unwrap_or(u32::MAX)
        .saturating_add(fixed_overhead)
}

fn has_provenance(source_event_ids: &[String]) -> bool {
    !source_event_ids.is_empty() && source_event_ids.iter().all(|id| !id.trim().is_empty())
}

fn tokenize(input: &str) -> HashSet<String> {
    input
        .split(|character: char| !character.is_alphanumeric())
        .filter(|token| token.chars().count() > 1)
        .map(str::to_lowercase)
        .collect()
}

#[cfg(test)]
mod tests {
    use std::{
        cell::Cell,
        collections::HashMap,
        sync::{Arc, Mutex},
    };

    use chrono::{Duration, Utc};
    use ditto_retrieval::{
        ContextResultLimit, Embedding, EmbeddingProvider, EmbeddingProviderError, EmbeddingPurpose,
        MAX_CANDIDATE_COUNT, MAX_CONTEXT_RESULT_LIMIT, MAX_PROVIDER_CALLS,
        MAX_TOTAL_CANDIDATE_BYTES, RetrievalMode, RetrievalWorkBudget, RetrievalWorkKind,
    };

    use super::{
        CAPSULE_ITEM_FIXED_OVERHEAD_TOKENS, CompiledContextValidationError, ContextCandidate,
        ContextCapsule, ContextCapsuleItem, ContextCapsuleValidationError, ContextCompileError,
        ContextCompiler, ContextEdge, ContextEdgeKind, ContextExclusion, ContextExclusionReason,
        ContextGraph, ContextLens, ContextNode, ContextNodeKind, ContextOrigin,
        ContextQueryRanking, ContextQueryRankingError, ContextScope, ContextValidationError,
        DEFAULT_CONTEXT_ABSOLUTE_BUDGET, EpistemicStatus, MAX_CONTEXT_NODE_ID_BYTES,
        MAX_CONTEXT_NODE_SUMMARY_BYTES, MAX_CONTEXT_REFERENCE_ID_BYTES,
        MAX_CONTEXT_SOURCE_EVENT_IDS, MAX_CONTEXT_SUPERSEDES, MAX_REQUEST_BYTES,
        MAX_RETRIEVAL_DOCUMENT_BYTES, MAX_SERIALIZED_CONTEXT_NODE_BYTES, RetrievalError, TaskQuery,
        TaskSignature, TaskSignatureV2, context_retrieval_document,
    };

    fn node(id: &str, summary: &str) -> ContextNode {
        ContextNode {
            id: id.into(),
            kind: ContextNodeKind::Constraint,
            summary: summary.into(),
            origin: ContextOrigin::User,
            epistemic: EpistemicStatus::Asserted,
            scope: ContextScope::Project,
            lens: ContextLens::Task,
            confidence: 1.0,
            source_event_ids: vec!["event-1".into()],
            supersedes: Vec::new(),
            valid_from: None,
            valid_until: None,
        }
    }

    fn reference_at_bound(index: usize) -> String {
        let prefix = format!("event-{index:02}-");
        format!(
            "{prefix}{}",
            "r".repeat(MAX_CONTEXT_REFERENCE_ID_BYTES - prefix.len())
        )
    }

    fn node_with_serialized_len(target: usize) -> ContextNode {
        let mut candidate = node(
            "serialized-bound",
            &"s".repeat(MAX_CONTEXT_NODE_SUMMARY_BYTES),
        );
        candidate.source_event_ids = (0..5).map(reference_at_bound).collect();
        let baseline = serde_json::to_vec(&candidate)
            .expect("serialize baseline bounded node")
            .len();
        let extra_escapes = target
            .checked_sub(baseline)
            .expect("serialized target exceeds baseline");
        assert!(extra_escapes <= MAX_CONTEXT_NODE_SUMMARY_BYTES);
        candidate.summary = format!(
            "{}{}",
            "\\".repeat(extra_escapes),
            "s".repeat(MAX_CONTEXT_NODE_SUMMARY_BYTES - extra_escapes)
        );
        assert_eq!(
            serde_json::to_vec(&candidate)
                .expect("serialize exact bounded node")
                .len(),
            target
        );
        candidate
    }

    #[derive(Clone)]
    struct RankingProvider {
        calls: Arc<Mutex<Vec<(EmbeddingPurpose, String)>>>,
        descriptor: String,
        query_vector: Vec<f32>,
        document_vectors: HashMap<String, Vec<f32>>,
        document_descriptor: Option<String>,
        document_vector_override: Option<Vec<f32>>,
        fail_documents: bool,
    }

    impl RankingProvider {
        fn new() -> Self {
            Self {
                calls: Arc::new(Mutex::new(Vec::new())),
                descriptor: "context-ranking-v1".into(),
                query_vector: vec![1.0, 0.0],
                document_vectors: HashMap::new(),
                document_descriptor: None,
                document_vector_override: None,
                fail_documents: false,
            }
        }

        fn with_document_vector(mut self, node_id: &str, vector: Vec<f32>) -> Self {
            self.document_vectors.insert(node_id.into(), vector);
            self
        }

        fn calls(&self) -> Vec<(EmbeddingPurpose, String)> {
            self.calls.lock().expect("ranking provider calls").clone()
        }

        fn document_ids(&self) -> Vec<String> {
            self.calls()
                .into_iter()
                .filter_map(|(purpose, document)| {
                    (purpose == EmbeddingPurpose::Document).then(|| {
                        document
                            .lines()
                            .next()
                            .expect("context document has id line")
                            .strip_prefix("id=")
                            .expect("context document id prefix")
                            .to_owned()
                    })
                })
                .collect()
        }
    }

    impl EmbeddingProvider for RankingProvider {
        fn embed(
            &self,
            purpose: EmbeddingPurpose,
            text: &str,
        ) -> Result<Embedding, EmbeddingProviderError> {
            self.calls
                .lock()
                .expect("ranking provider calls")
                .push((purpose, text.to_owned()));
            if purpose == EmbeddingPurpose::Document && self.fail_documents {
                return Err(EmbeddingProviderError::failure(
                    "context document unavailable",
                ));
            }

            let descriptor = if purpose == EmbeddingPurpose::Document {
                self.document_descriptor
                    .as_deref()
                    .unwrap_or(&self.descriptor)
            } else {
                &self.descriptor
            };
            let vector = match purpose {
                EmbeddingPurpose::Query => self.query_vector.clone(),
                EmbeddingPurpose::Document => {
                    if let Some(vector) = &self.document_vector_override {
                        vector.clone()
                    } else {
                        let node_id = text
                            .lines()
                            .next()
                            .and_then(|line| line.strip_prefix("id="))
                            .expect("context document id");
                        self.document_vectors
                            .get(node_id)
                            .cloned()
                            .unwrap_or_else(|| vec![0.0, 1.0])
                    }
                }
            };
            Ok(Embedding::new(descriptor, vector))
        }
    }

    #[test]
    fn trusted_pin_survives_soft_budget_but_not_absolute_ceiling() {
        let compiler = ContextCompiler {
            default_budget: 5,
            absolute_budget: 100,
        };
        let signature = TaskSignature {
            request: "restart the home server service".into(),
            ..TaskSignature::default()
        };
        let compiled = compiler
            .compile(
                &signature,
                [
                    ContextCandidate::user_pinned(node("pinned", "always ask before sudo")),
                    ContextCandidate::ranked(node("relevant", "home server service uses systemd")),
                ],
                None,
                Utc::now(),
            )
            .expect("compile context");

        assert!(compiled.nodes.iter().any(|item| item.id == "pinned"));
        assert!(compiled.receipt.over_soft_budget);
    }

    #[test]
    fn required_invalid_context_blocks_compilation() {
        let mut invalid = node("policy", "never expose credentials");
        invalid.source_event_ids.clear();
        let error = ContextCompiler::default()
            .compile(
                &TaskSignature::default(),
                [ContextCandidate::policy_required(
                    invalid,
                    "credential boundary",
                )],
                None,
                Utc::now(),
            )
            .expect_err("required invalid context must block");
        assert!(matches!(
            error,
            ContextCompileError::InvalidRequiredContext { .. }
        ));
    }

    #[test]
    fn rejects_context_without_valid_provenance() {
        let mut graph = ContextGraph::default();
        let mut durable = node("durable", "project constraint");
        durable.source_event_ids.clear();
        assert!(matches!(
            graph.insert_node(durable),
            Err(ContextValidationError::MissingProvenance { .. })
        ));

        let mut model_assertion = node("model", "model claim");
        model_assertion.origin = ContextOrigin::Model;
        assert!(matches!(
            graph.insert_node(model_assertion),
            Err(ContextValidationError::ModelCannotAssert { .. })
        ));
    }

    #[test]
    fn validates_edge_provenance_and_endpoints() {
        let mut graph = ContextGraph::default();
        graph
            .insert_node(node("claim", "service state"))
            .expect("insert claim");
        graph
            .insert_node(node("evidence", "service output"))
            .expect("insert evidence");
        graph
            .insert_edge(ContextEdge {
                id: "edge-1".into(),
                from: "evidence".into(),
                to: "claim".into(),
                relation: ContextEdgeKind::Supports,
                source_event_ids: vec!["event-2".into()],
            })
            .expect("insert edge");
        assert_eq!(graph.edges().len(), 1);
    }

    #[test]
    fn authority_does_not_make_irrelevant_context_eligible() {
        let signature = TaskSignature {
            request: "restart database service".into(),
            ..TaskSignature::default()
        };
        let compiled = ContextCompiler::default()
            .compile(
                &signature,
                [ContextCandidate::ranked(node(
                    "irrelevant",
                    "preferred vacation destination",
                ))],
                None,
                Utc::now(),
            )
            .expect("compile context");

        assert!(compiled.nodes.is_empty());
        assert_eq!(compiled.receipt.excluded.len(), 1);
    }

    #[test]
    fn token_cost_is_derived_instead_of_accepted_from_input() {
        let signature = TaskSignature {
            request: "database".into(),
            ..TaskSignature::default()
        };
        let compiled = ContextCompiler::default()
            .compile(
                &signature,
                [ContextCandidate::ranked(node(
                    "cost",
                    "database database database database",
                ))],
                None,
                Utc::now(),
            )
            .expect("compile context");
        assert!(compiled.receipt.included[0].token_cost > 0);
    }

    #[test]
    fn context_capsule_projection_preserves_compiled_node_order() {
        let signature = TaskSignature {
            request: "database service".into(),
            ..TaskSignature::default()
        };
        let compiled = ContextCompiler::default()
            .compile(
                &signature,
                [
                    ContextCandidate::user_pinned(node("pinned", "database approval")),
                    ContextCandidate::ranked(node("ranked", "database service")),
                ],
                None,
                Utc::now(),
            )
            .expect("compile context");

        let capsule = ContextCapsule::from(&compiled);
        assert_eq!(
            capsule
                .nodes
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            ["pinned", "ranked"]
        );
        let wire = serde_json::to_value(&capsule).expect("serialize context capsule");
        assert!(wire.get("receipt").is_none());
        assert_eq!(wire["nodes"].as_array().map(Vec::len), Some(2));
        assert!(wire["nodes"][0].get("supersedes").is_none());
        assert!(wire["nodes"][0].get("lens").is_none());
        assert_eq!(capsule.token_cost(), compiled.receipt.total_token_cost);
        assert!(
            serde_json::to_vec(&capsule)
                .expect("serialize context capsule")
                .len()
                .div_ceil(4)
                <= compiled.receipt.total_token_cost as usize
        );
        capsule.validate_at(Utc::now()).expect("valid capsule");
    }

    #[test]
    fn capsule_cost_uses_exact_item_serialization_and_drops_durable_metadata() {
        let mut durable = node("project", "database service");
        durable.lens = ContextLens::Environment;
        durable.supersedes = (0..10_000).map(|index| format!("old-{index}")).collect();

        let item = ContextCapsuleItem::from(&durable);
        let compact = ContextCapsuleItem::from(&node("project", "database service"));
        let wire = serde_json::to_value(&item).expect("serialize capsule item");
        assert!(wire.get("supersedes").is_none());
        assert!(wire.get("lens").is_none());
        assert_eq!(
            item.serialized_len(),
            serde_json::to_vec(&serde_json::json!({
                "id": "project",
                "kind": "constraint",
                "summary": "database service",
                "origin": "user",
                "epistemic": "asserted",
                "scope": "project",
                "confidence": 1.0,
                "source_event_ids": ["event-1"]
            }))
            .expect("serialize expected capsule item")
            .len()
        );
        let expected_item_cost =
            item.serialized_len().div_ceil(4) as u32 + CAPSULE_ITEM_FIXED_OVERHEAD_TOKENS;
        assert_eq!(item.token_cost(), expected_item_cost);
        assert_eq!(item.source_event_ids, compact.source_event_ids);
        assert_eq!(item.token_cost(), compact.token_cost());

        let capsule = ContextCapsule { nodes: vec![item] };
        capsule
            .validate_at(Utc::now())
            .expect("dropped metadata cannot invalidate capsule");
        assert!(capsule.token_cost() <= DEFAULT_CONTEXT_ABSOLUTE_BUDGET);
    }

    #[test]
    fn huge_provenance_is_charged_and_cannot_cross_absolute_ceiling() {
        let mut huge = node("huge", "database service");
        huge.source_event_ids = (0..10_000).map(|index| format!("event-{index}")).collect();
        let compiler = ContextCompiler {
            default_budget: DEFAULT_CONTEXT_ABSOLUTE_BUDGET,
            absolute_budget: DEFAULT_CONTEXT_ABSOLUTE_BUDGET,
        };

        let compiled = compiler
            .compile(
                &TaskSignature {
                    request: "database service".into(),
                    ..TaskSignature::default()
                },
                [ContextCandidate::ranked(huge)],
                None,
                Utc::now(),
            )
            .expect("oversized ranked context is excluded");

        assert!(compiled.nodes.is_empty());
        assert_eq!(compiled.receipt.total_token_cost, 0);
        assert!(
            compiled
                .receipt
                .excluded
                .iter()
                .any(|entry| matches!(entry.reason, super::ContextExclusionReason::TokenBudget))
        );
    }

    #[test]
    fn ranked_selection_is_capped_even_when_soft_budget_is_larger() {
        let compiler = ContextCompiler {
            default_budget: 100_000,
            absolute_budget: 200,
        };
        let signature = TaskSignature {
            request: "database service".into(),
            ..TaskSignature::default()
        };
        let compiled = compiler
            .compile(
                &signature,
                (0..10).map(|index| {
                    ContextCandidate::ranked(node(
                        &format!("candidate-{index}"),
                        "database service",
                    ))
                }),
                None,
                Utc::now(),
            )
            .expect("ranked context compiles");

        assert!(compiled.receipt.total_token_cost <= compiler.absolute_budget);
        assert!(
            compiled
                .receipt
                .excluded
                .iter()
                .any(|entry| matches!(entry.reason, super::ContextExclusionReason::TokenBudget))
        );
    }

    #[test]
    fn capsule_validation_rejects_invalid_trust_metadata_and_time() {
        let now = Utc::now();

        let mut missing_provenance = node("missing-provenance", "database service");
        missing_provenance.source_event_ids.clear();
        let missing_provenance = ContextCapsule {
            nodes: vec![ContextCapsuleItem::from(&missing_provenance)],
        };
        assert!(matches!(
            missing_provenance.validate_at(now),
            Err(ContextCapsuleValidationError::InvalidItem { ref reason, .. })
                if reason.contains("no source event provenance")
        ));

        let mut model_assertion = node("model-assertion", "database service");
        model_assertion.origin = ContextOrigin::Model;
        let model_assertion = ContextCapsule {
            nodes: vec![ContextCapsuleItem::from(&model_assertion)],
        };
        assert!(matches!(
            model_assertion.validate_at(now),
            Err(ContextCapsuleValidationError::InvalidItem { ref reason, .. })
                if reason.contains("cannot be asserted")
        ));

        let mut disputed = node("disputed", "database service");
        disputed.epistemic = EpistemicStatus::Disputed;
        let disputed = ContextCapsule {
            nodes: vec![ContextCapsuleItem::from(&disputed)],
        };
        assert!(matches!(
            disputed.validate_at(now),
            Err(ContextCapsuleValidationError::NotValidAt { ref item_id })
                if item_id == "disputed"
        ));

        let mut expired = node("expired", "database service");
        expired.valid_until = Some(now - Duration::seconds(1));
        let expired = ContextCapsule {
            nodes: vec![ContextCapsuleItem::from(&expired)],
        };
        assert!(matches!(
            expired.validate_at(now),
            Err(ContextCapsuleValidationError::NotValidAt { ref item_id })
                if item_id == "expired"
        ));
    }

    #[test]
    fn required_time_invalid_context_blocks_but_ranked_context_is_excluded() {
        let now = Utc::now();
        let signature = TaskSignature {
            request: "database service".into(),
            ..TaskSignature::default()
        };

        let mut expired = node("expired-required", "database service");
        expired.valid_until = Some(now - Duration::seconds(1));
        let error = ContextCompiler::default()
            .compile(
                &signature,
                [ContextCandidate::user_pinned(expired)],
                None,
                now,
            )
            .expect_err("expired required context must block");
        assert!(matches!(
            error,
            ContextCompileError::InvalidRequiredContext { ref node_id, ref reason }
                if node_id == "expired-required" && reason.contains("expired")
        ));

        let mut future = node("future-required", "database service");
        future.valid_from = Some(now + Duration::seconds(1));
        let error = ContextCompiler::default()
            .compile(
                &signature,
                [ContextCandidate::policy_required(future, "fresh state")],
                None,
                now,
            )
            .expect_err("future required context must block");
        assert!(matches!(
            error,
            ContextCompileError::InvalidRequiredContext { ref node_id, ref reason }
                if node_id == "future-required" && reason.contains("not started")
        ));

        let mut disputed = node("disputed-required", "database service");
        disputed.epistemic = EpistemicStatus::Disputed;
        let error = ContextCompiler::default()
            .compile(
                &signature,
                [ContextCandidate::user_pinned(disputed)],
                None,
                now,
            )
            .expect_err("disputed required context must block");
        assert!(matches!(
            error,
            ContextCompileError::InvalidRequiredContext { ref node_id, ref reason }
                if node_id == "disputed-required" && reason.contains("disputed")
        ));

        let mut ranked_expired = node("expired-ranked", "database service");
        ranked_expired.valid_until = Some(now - Duration::seconds(1));
        let compiled = ContextCompiler::default()
            .compile(
                &signature,
                [ContextCandidate::ranked(ranked_expired)],
                None,
                now,
            )
            .expect("expired ranked context is excluded");
        assert!(compiled.nodes.is_empty());
        assert!(compiled.receipt.excluded.iter().any(|entry| {
            entry.node_id == "expired-ranked"
                && matches!(
                    entry.reason,
                    super::ContextExclusionReason::DisputedOrExpired
                )
        }));

        let mut ranked_future = node("future-ranked", "database service");
        ranked_future.valid_from = Some(now + Duration::seconds(1));
        let compiled = ContextCompiler::default()
            .compile(
                &signature,
                [ContextCandidate::ranked(ranked_future)],
                None,
                now,
            )
            .expect("future ranked context is excluded");
        assert!(compiled.nodes.is_empty());
        assert!(compiled.receipt.excluded.iter().any(|entry| {
            entry.node_id == "future-ranked"
                && matches!(
                    entry.reason,
                    super::ContextExclusionReason::DisputedOrExpired
                )
        }));
    }

    #[test]
    fn deserialized_over_budget_capsule_is_rejected() {
        let oversized =
            ContextCapsuleItem::from(&node("oversized", &"database service ".repeat(2_000)));
        let wire = serde_json::to_vec(&ContextCapsule {
            nodes: vec![oversized],
        })
        .expect("serialize oversized capsule");
        let decoded: ContextCapsule =
            serde_json::from_slice(&wire).expect("deserialize oversized capsule");

        assert!(matches!(
            decoded.validate_at(Utc::now()),
            Err(ContextCapsuleValidationError::TokenBudgetExceeded {
                absolute_budget: DEFAULT_CONTEXT_ABSOLUTE_BUDGET,
                ..
            })
        ));
    }

    fn validation_fixture() -> (
        ContextCompiler,
        TaskSignature,
        super::CompiledContext,
        ContextCapsule,
        chrono::DateTime<Utc>,
    ) {
        let compiler = ContextCompiler::default();
        let signature = TaskSignature {
            request: "database service".into(),
            ..TaskSignature::default()
        };
        let accepted_at = Utc::now();
        let compiled = compiler
            .compile(
                &signature,
                [
                    ContextCandidate::user_pinned(node("pinned", "database approval")),
                    ContextCandidate::ranked(node("ranked", "database service")),
                ],
                Some(compiler.default_budget),
                accepted_at,
            )
            .expect("valid context compiles");
        let capsule = ContextCapsule::from(&compiled);
        (compiler, signature, compiled, capsule, accepted_at)
    }

    #[test]
    fn compiler_rejects_duplicate_candidate_ids_before_selection() {
        let duplicate = ContextCompiler::default()
            .compile(
                &TaskSignature {
                    request: "database".into(),
                    ..TaskSignature::default()
                },
                [
                    ContextCandidate::ranked(node("duplicate", "database")),
                    ContextCandidate::user_pinned(node("duplicate", "database")),
                ],
                None,
                Utc::now(),
            )
            .expect_err("duplicate candidate ids must be rejected before selection");
        assert!(matches!(
            duplicate,
            ContextCompileError::DuplicateCandidate { ref node_id }
                if node_id == "duplicate"
        ));
    }

    #[test]
    fn compiler_rejects_empty_policy_reason_before_selection() {
        for reason in ["", "   ", "\n\t"] {
            let error = ContextCompiler::default()
                .compile(
                    &TaskSignature::default(),
                    [ContextCandidate::policy_required(
                        node("policy", "protect credentials"),
                        reason,
                    )],
                    None,
                    Utc::now(),
                )
                .expect_err("empty policy reason must be rejected");
            assert!(matches!(
                error,
                ContextCompileError::InvalidPolicyReason { ref node_id }
                    if node_id == "policy"
            ));
        }
    }

    #[test]
    fn compiled_context_validation_accepts_exact_compiler_output_and_caller_budget() {
        let (compiler, signature, compiled, capsule, accepted_at) = validation_fixture();
        compiler
            .validate_compiled(
                &signature,
                &compiled,
                &capsule,
                Some(compiler.default_budget),
                accepted_at,
            )
            .expect("exact compiled context validates");
    }

    #[test]
    fn compiled_context_validation_accepts_policy_reason_priority_order() {
        let compiler = ContextCompiler::default();
        let signature = TaskSignature {
            request: "database service".into(),
            ..TaskSignature::default()
        };
        let accepted_at = Utc::now();
        let compiled = compiler
            .compile(
                &signature,
                [
                    ContextCandidate::ranked(node("ranked", "database service")),
                    ContextCandidate::user_pinned(node("pinned", "database approval")),
                    ContextCandidate::policy_required(
                        node("policy", "database credentials"),
                        "protect secret material",
                    ),
                ],
                None,
                accepted_at,
            )
            .expect("policy context compiles");
        let capsule = ContextCapsule::from(&compiled);
        compiler
            .validate_compiled(&signature, &compiled, &capsule, None, accepted_at)
            .expect("policy reason and priority order validate");
        assert_eq!(
            compiled
                .receipt
                .included
                .iter()
                .map(|entry| entry.reason.as_str())
                .collect::<Vec<_>>(),
            [
                "policy-required: protect secret material",
                "user-pinned",
                "task-relevance"
            ]
        );
    }

    #[test]
    fn compiled_context_validation_rejects_score_mutation() {
        let (compiler, signature, mut compiled, capsule, accepted_at) = validation_fixture();
        compiled.receipt.included[0].score += 1.0;
        assert!(matches!(
            compiler.validate_compiled(
                &signature,
                &compiled,
                &capsule,
                Some(compiler.default_budget),
                accepted_at,
            ),
            Err(CompiledContextValidationError::ScoreMismatch { ref node_id })
                if node_id == "pinned"
        ));
    }

    #[test]
    fn compiled_context_validation_rejects_noncanonical_order() {
        let (compiler, signature, mut compiled, _, accepted_at) = validation_fixture();
        compiled.nodes.swap(0, 1);
        compiled.receipt.included.swap(0, 1);
        let capsule = ContextCapsule::from(&compiled);
        assert!(matches!(
            compiler.validate_compiled(
                &signature,
                &compiled,
                &capsule,
                Some(compiler.default_budget),
                accepted_at,
            ),
            Err(CompiledContextValidationError::NonCanonicalOrder { ref node_id })
                if node_id == "pinned"
        ));
    }

    #[test]
    fn compiled_context_validation_rejects_invalid_reason_grammar() {
        let (compiler, signature, mut compiled, capsule, accepted_at) = validation_fixture();
        compiled.receipt.included[0].reason = "policy-required: ".into();
        assert!(matches!(
            compiler.validate_compiled(
                &signature,
                &compiled,
                &capsule,
                Some(compiler.default_budget),
                accepted_at,
            ),
            Err(CompiledContextValidationError::InvalidReceiptReason {
                ref node_id,
                ref reason,
            }) if node_id == "pinned" && reason == "policy-required: "
        ));
    }

    #[test]
    fn compiled_context_validation_rejects_token_cost_mutation() {
        let (compiler, signature, mut compiled, capsule, accepted_at) = validation_fixture();
        compiled.receipt.included[0].token_cost += 1;
        assert!(matches!(
            compiler.validate_compiled(
                &signature,
                &compiled,
                &capsule,
                Some(compiler.default_budget),
                accepted_at,
            ),
            Err(CompiledContextValidationError::TokenCostMismatch { ref node_id, .. })
                if node_id == "pinned"
        ));
    }

    #[test]
    fn compiled_context_validation_rejects_capsule_mutation() {
        let (compiler, signature, compiled, mut capsule, accepted_at) = validation_fixture();
        capsule.nodes[0].summary.push_str(" forged");
        assert!(matches!(
            compiler.validate_compiled(
                &signature,
                &compiled,
                &capsule,
                Some(compiler.default_budget),
                accepted_at,
            ),
            Err(CompiledContextValidationError::CapsuleMismatch)
        ));
    }

    #[test]
    fn compiled_context_validation_rejects_node_receipt_length_mismatch() {
        let (compiler, signature, mut compiled, capsule, accepted_at) = validation_fixture();
        compiled.receipt.included.pop();
        assert!(matches!(
            compiler.validate_compiled(
                &signature,
                &compiled,
                &capsule,
                Some(compiler.default_budget),
                accepted_at,
            ),
            Err(CompiledContextValidationError::NodeReceiptLengthMismatch {
                nodes: 2,
                included: 1,
                capsule: 2,
            })
        ));
    }

    #[test]
    fn compiled_context_validation_rejects_budget_and_accounting_mutations() {
        let (compiler, signature, mut compiled, capsule, accepted_at) = validation_fixture();
        compiled.receipt.token_budget -= 1;
        assert!(matches!(
            compiler.validate_compiled(
                &signature,
                &compiled,
                &capsule,
                Some(compiler.default_budget),
                accepted_at,
            ),
            Err(CompiledContextValidationError::BudgetMismatch { .. })
        ));

        let (compiler, signature, mut compiled, capsule, accepted_at) = validation_fixture();
        compiled.receipt.total_token_cost += 1;
        assert!(matches!(
            compiler.validate_compiled(
                &signature,
                &compiled,
                &capsule,
                Some(compiler.default_budget),
                accepted_at,
            ),
            Err(CompiledContextValidationError::TokenAccountingMismatch { .. })
        ));
    }

    #[test]
    fn compiled_context_validation_rejects_ranked_selection_over_soft_budget() {
        let (compiler, signature, mut compiled, capsule, accepted_at) = validation_fixture();
        compiled.receipt.token_budget = 1;
        compiled.receipt.over_soft_budget = true;
        assert!(matches!(
            compiler.validate_compiled(&signature, &compiled, &capsule, Some(1), accepted_at),
            Err(CompiledContextValidationError::SelectionBudgetExceeded {
                ref node_id,
                token_budget: 1,
                ..
            }) if node_id == "ranked"
        ));
    }

    #[test]
    fn compiled_context_validation_rejects_duplicate_included_and_excluded_ids() {
        let (compiler, signature, mut compiled, capsule, accepted_at) = validation_fixture();
        compiled.receipt.excluded.push(ContextExclusion {
            node_id: "pinned".into(),
            reason: ContextExclusionReason::Irrelevant,
            detail: None,
        });
        assert!(matches!(
            compiler.validate_compiled(
                &signature,
                &compiled,
                &capsule,
                Some(compiler.default_budget),
                accepted_at,
            ),
            Err(CompiledContextValidationError::DuplicateReceiptId { ref node_id })
                if node_id == "pinned"
        ));
    }

    #[test]
    fn task_relevance_receipts_require_a_positive_score() {
        let compiler = ContextCompiler::default();
        let signature = TaskSignature {
            request: "database".into(),
            ..TaskSignature::default()
        };
        let accepted_at = Utc::now();
        let compiled = compiler
            .compile(
                &signature,
                [ContextCandidate::user_pinned(node(
                    "pinned",
                    "vacation destination",
                ))],
                None,
                accepted_at,
            )
            .expect("pinned context is included even when irrelevant");
        let mut mutated = compiled.clone();
        mutated.receipt.included[0].reason = "task-relevance".into();
        let capsule = ContextCapsule::from(&mutated);
        assert!(matches!(
            compiler.validate_compiled(
                &signature,
                &mutated,
                &capsule,
                None,
                accepted_at,
            ),
            Err(CompiledContextValidationError::NonPositiveTaskRelevance { ref node_id })
                if node_id == "pinned"
        ));
    }

    #[test]
    fn compiled_context_validation_rejects_capsule_expiry_at_acceptance() {
        let (compiler, signature, compiled, mut capsule, accepted_at) = validation_fixture();
        capsule.nodes[0].valid_until = Some(accepted_at - Duration::seconds(1));
        assert!(matches!(
            compiler.validate_compiled(
                &signature,
                &compiled,
                &capsule,
                Some(compiler.default_budget),
                accepted_at,
            ),
            Err(CompiledContextValidationError::InvalidCapsule(
                ContextCapsuleValidationError::NotValidAt { ref item_id }
            )) if item_id == "pinned"
        ));
    }

    #[test]
    fn legacy_task_signature_full_literal_stays_source_compatible_and_opt_in_v2_conversion_is_fallible()
     {
        let legacy = TaskSignature {
            request: "inspect the workspace".into(),
            active_goal: Some("prepare a local report".into()),
            entities: vec!["workspace".into()],
            constraints: vec!["no network".into()],
            expected_effect: Some("read-only".into()),
        };
        let migrated = legacy
            .try_to_v2()
            .expect("an in-bound legacy signature migrates explicitly");
        assert_eq!(migrated.resources, Vec::<String>::new());
        assert_eq!(migrated.request, "inspect the workspace");
        assert_eq!(migrated.entities, vec!["workspace"]);

        let over_v2_request = TaskSignature {
            request: "request ".repeat(MAX_REQUEST_BYTES / 8 + 1),
            ..TaskSignature::default()
        };
        assert!(matches!(
            over_v2_request.try_to_v2(),
            Err(RetrievalError::ComponentTooLong {
                field: "request",
                actual,
                maximum: MAX_REQUEST_BYTES,
            }) if actual > MAX_REQUEST_BYTES
        ));
    }

    #[test]
    fn legacy_context_compiler_retains_historical_normalization_bounds_and_does_not_delegate_to_v2()
    {
        let compiler = ContextCompiler::default();
        let now = Utc::now();
        let one_character = TaskSignature {
            request: "x".into(),
            ..TaskSignature::default()
        };
        let legacy_one_character = compiler
            .compile(
                &one_character,
                [ContextCandidate::ranked(node("one-character", "x"))],
                None,
                now,
            )
            .expect("legacy one-character compilation remains valid");
        assert!(legacy_one_character.nodes.is_empty());

        let v2_one_character = TaskQuery::new(TaskSignatureV2::new("x"))
            .expect("V2 retains one-character lexical tokens");
        let v2_compiled = compiler
            .compile_query(
                &v2_one_character,
                [ContextCandidate::ranked(node("one-character", "x"))],
                None,
                now,
            )
            .expect("V2 one-character compilation succeeds");
        assert_eq!(v2_compiled.nodes.len(), 1);

        let over_v2_request = "request ".repeat(MAX_REQUEST_BYTES / 8 + 1);
        let legacy_over_bound = TaskSignature {
            request: over_v2_request,
            ..TaskSignature::default()
        };
        let legacy_over_bound_compiled = compiler
            .compile(
                &legacy_over_bound,
                [ContextCandidate::ranked(node(
                    "legacy-over-bound",
                    "request",
                ))],
                None,
                now,
            )
            .expect("legacy compiler keeps its historical request bound");
        assert_eq!(legacy_over_bound_compiled.nodes.len(), 1);
        assert!(legacy_over_bound.try_to_v2().is_err());

        let legacy_many = compiler
            .compile(
                &TaskSignature {
                    request: "legacy candidate".into(),
                    ..TaskSignature::default()
                },
                (0..=10_000).map(|index| {
                    ContextCandidate::ranked(node(
                        &format!("legacy-candidate-{index}"),
                        "legacy candidate",
                    ))
                }),
                None,
                now,
            )
            .expect("legacy compiler retains its unbounded historical scan");
        assert!(!legacy_many.nodes.is_empty());
    }

    #[test]
    fn v2_context_document_is_exact_bounded_and_only_node_id_is_exact() {
        let mut raw = node("Id=Raw", "summary\nraw");
        raw.kind = ContextNodeKind::OpenQuestion;
        let document = context_retrieval_document(&raw).expect("raw fields form a document");
        assert_eq!(
            document.as_str(),
            "id=Id=Raw\nkind=open_question\nsummary=summary\nraw"
        );
        assert!(!document.as_str().ends_with('\n'));

        let max_id = "i".repeat(256);
        let max_summary = "s".repeat(65_000);
        let mut max_node = node(&max_id, &max_summary);
        max_node.kind = ContextNodeKind::OpenQuestion;
        let max_document = context_retrieval_document(&max_node).expect("maximum document fits");
        assert_eq!(max_document.len(), 65_287);
        assert!(max_document.len() <= MAX_RETRIEVAL_DOCUMENT_BYTES);

        let query = TaskQuery::new(TaskSignatureV2 {
            request: "zz".into(),
            entities: vec![" WORKSPACE ".into()],
            ..TaskSignatureV2::default()
        })
        .expect("query is valid");
        let exact = node(" Workspace ", "unrelated text");
        let summary_only = node("other", "workspace");
        let compiled = ContextCompiler::default()
            .compile_query(
                &query,
                [
                    ContextCandidate::ranked(summary_only),
                    ContextCandidate::ranked(exact),
                ],
                None,
                Utc::now(),
            )
            .expect("exact and lexical candidates compile");
        assert_eq!(compiled.nodes[0].id, " Workspace ");
        assert_eq!(compiled.nodes[1].id, "other");

        let kind_query = TaskQuery::new(TaskSignatureV2::new("constraint"))
            .expect("kind lexical query is valid");
        let kind_compiled = ContextCompiler::default()
            .compile_query(
                &kind_query,
                [ContextCandidate::ranked(node("kind-only", "unrelated"))],
                None,
                Utc::now(),
            )
            .expect("kind participates in positive lexical scoring");
        assert_eq!(kind_compiled.nodes[0].id, "kind-only");

        let summary_query =
            TaskQuery::new(TaskSignatureV2::new("needle")).expect("summary lexical query is valid");
        let summary_compiled = ContextCompiler::default()
            .compile_query(
                &summary_query,
                [ContextCandidate::ranked(node("summary-only", "needle"))],
                None,
                Utc::now(),
            )
            .expect("summary participates in positive lexical scoring");
        assert_eq!(summary_compiled.nodes[0].id, "summary-only");
    }

    #[test]
    fn v2_context_document_accepts_65536_and_rejects_65537_bytes() {
        let mut node_at_bound = node("i", "placeholder");
        node_at_bound.kind = ContextNodeKind::OpenQuestion;
        let prefix = format!(
            "id={}\nkind={}\nsummary=",
            node_at_bound.id,
            node_at_bound.kind.as_str()
        );
        let summary_at_bound = MAX_RETRIEVAL_DOCUMENT_BYTES - prefix.len();
        node_at_bound.summary = "s".repeat(summary_at_bound);
        let document = context_retrieval_document(&node_at_bound).expect("document at bound");
        assert_eq!(document.len(), MAX_RETRIEVAL_DOCUMENT_BYTES);

        let mut node_over_bound = node_at_bound;
        node_over_bound.summary.push('s');
        assert_eq!(
            context_retrieval_document(&node_over_bound),
            Err(RetrievalError::RetrievalDocumentTooLong {
                actual: MAX_RETRIEVAL_DOCUMENT_BYTES + 1,
                maximum: MAX_RETRIEVAL_DOCUMENT_BYTES,
            })
        );
    }

    #[test]
    fn v2_raw_document_fields_are_unescaped_but_exact_candidate_controls_are_rejected() {
        let mut whitespace = node(" Bad\nID ", "line\nsummary");
        whitespace.kind = ContextNodeKind::Resource;
        let document = context_retrieval_document(&whitespace).expect("raw whitespace fields");
        assert_eq!(
            document.as_str(),
            "id= Bad\nID \nkind=resource\nsummary=line\nsummary"
        );
        let query = TaskQuery::new(TaskSignatureV2 {
            request: "inspect".into(),
            resources: vec!["bad id".into()],
            ..TaskSignatureV2::default()
        })
        .expect("valid exact query");
        assert!(
            query
                .matches_exact_term(&whitespace.id)
                .expect("whitespace controls normalize")
        );

        let non_whitespace = node("bad\0id", "summary");
        let document = context_retrieval_document(&non_whitespace).expect("raw control field");
        assert_eq!(
            document.as_str(),
            "id=bad\0id\nkind=constraint\nsummary=summary"
        );
        assert_eq!(
            query
                .matches_exact_term(&non_whitespace.id)
                .expect_err("non-whitespace control must fail closed"),
            RetrievalError::ControlCharacter {
                field: "exact_term"
            }
        );
    }

    #[test]
    fn explicit_resources_match_normalized_context_node_ids() {
        let query = TaskQuery::new(TaskSignatureV2 {
            request: "inspect".into(),
            resources: vec![" Device:Kitchen ".into()],
            ..TaskSignatureV2::default()
        })
        .expect("resource query is valid");
        assert_eq!(query.signature().resources, vec!["device:kitchen"]);

        let exact = node("Device:Kitchen", "unrelated");
        let summary_only = node("other", "device:kitchen");
        let compiled = ContextCompiler::default()
            .compile_query(
                &query,
                [
                    ContextCandidate::ranked(summary_only),
                    ContextCandidate::ranked(exact),
                ],
                None,
                Utc::now(),
            )
            .expect("resource exact and lexical candidates compile");
        assert_eq!(compiled.nodes[0].id, "Device:Kitchen");
        assert_eq!(compiled.nodes[1].id, "other");
    }

    #[test]
    fn v2_context_candidates_accept_10000_and_reject_10001_before_scoring() {
        let query =
            TaskQuery::new(TaskSignatureV2::new("candidate")).expect("candidate query is valid");
        let compiler = ContextCompiler::default();
        let accepted = compiler.compile_query(
            &query,
            (0..10_000).map(|index| {
                ContextCandidate::ranked(node(&format!("candidate-{index}"), "candidate"))
            }),
            None,
            Utc::now(),
        );
        assert!(accepted.is_ok());

        let mut over = (0..10_000)
            .map(|index| ContextCandidate::ranked(node(&format!("candidate-{index}"), "candidate")))
            .collect::<Vec<_>>();
        over.push(ContextCandidate::ranked(node(
            "candidate-over-limit",
            &"x".repeat(MAX_RETRIEVAL_DOCUMENT_BYTES + 1),
        )));
        assert!(matches!(
            compiler.compile_query(&query, over, None, Utc::now()),
            Err(ContextCompileError::Retrieval(
                RetrievalError::CandidateCountExceeded {
                    actual: 10_001,
                    maximum: 10_000,
                }
            ))
        ));

        let consumed = std::cell::Cell::new(0_usize);
        assert!(matches!(
            compiler.compile_query(
                &query,
                std::iter::from_fn(|| {
                    let index = consumed.get();
                    consumed.set(index + 1);
                    Some(ContextCandidate::ranked(node(
                        &format!("streamed-candidate-{index}"),
                        "candidate",
                    )))
                }),
                None,
                Utc::now(),
            ),
            Err(ContextCompileError::Retrieval(
                RetrievalError::CandidateCountExceeded {
                    actual: 10_001,
                    maximum: 10_000,
                }
            ))
        ));
        assert_eq!(consumed.get(), 10_001);
    }

    #[test]
    fn v2_equal_score_ranked_nodes_use_ascending_id_tie_order() {
        let query = TaskQuery::new(TaskSignatureV2::new("target")).expect("target query is valid");
        let compiler = ContextCompiler::default();
        let compiled = compiler
            .compile_query(
                &query,
                [
                    ContextCandidate::ranked(node("z-node", "target")),
                    ContextCandidate::ranked(node("a-node", "target")),
                ],
                None,
                Utc::now(),
            )
            .expect("equal-score candidates compile");
        assert_eq!(
            compiled
                .nodes
                .iter()
                .map(|candidate| candidate.id.as_str())
                .collect::<Vec<_>>(),
            ["a-node", "z-node"]
        );
    }

    #[test]
    fn invalid_shared_query_fails_before_candidate_selection() {
        assert_eq!(
            TaskQuery::new(TaskSignatureV2::default()),
            Err(RetrievalError::EmptyRequest)
        );
        let mut oversized = TaskSignatureV2::new("valid");
        oversized.request = "x".repeat(MAX_REQUEST_BYTES + 1);
        assert!(matches!(
            TaskQuery::new(oversized),
            Err(RetrievalError::ComponentTooLong {
                field: "request",
                actual,
                maximum: MAX_REQUEST_BYTES,
            }) if actual == MAX_REQUEST_BYTES + 1
        ));
    }

    #[test]
    fn v2_compiled_context_validation_rederives_document_score_and_exact_order() {
        let compiler = ContextCompiler::default();
        let query = TaskQuery::new(TaskSignatureV2 {
            request: "inspect".into(),
            entities: vec!["target".into()],
            ..TaskSignatureV2::default()
        })
        .expect("query is valid");
        let accepted_at = Utc::now();
        let compiled = compiler
            .compile_query(
                &query,
                [
                    ContextCandidate::ranked(node("lexical", "target")),
                    ContextCandidate::ranked(node("target", "unrelated")),
                ],
                None,
                accepted_at,
            )
            .expect("V2 context compiles");
        let capsule = ContextCapsule::from(&compiled);
        compiler
            .validate_compiled_query(&query, &compiled, &capsule, None, accepted_at)
            .expect("V2 compiler output validates");
    }

    #[test]
    fn v2_compiled_context_validation_rejects_tampered_score_and_order() {
        let compiler = ContextCompiler::default();
        let query = TaskQuery::new(TaskSignatureV2::new("target")).expect("target query is valid");
        let accepted_at = Utc::now();
        let compiled = compiler
            .compile_query(
                &query,
                [
                    ContextCandidate::ranked(node("a-node", "target")),
                    ContextCandidate::ranked(node("b-node", "target")),
                ],
                None,
                accepted_at,
            )
            .expect("V2 context compiles");

        let mut tampered_score = compiled.clone();
        tampered_score.receipt.included[0].score += 1.0;
        let score_capsule = ContextCapsule::from(&tampered_score);
        assert!(matches!(
            compiler.validate_compiled_query(
                &query,
                &tampered_score,
                &score_capsule,
                None,
                accepted_at,
            ),
            Err(CompiledContextValidationError::ScoreMismatch { ref node_id })
                if node_id == "a-node"
        ));

        let mut tampered_order = compiled;
        tampered_order.nodes.swap(0, 1);
        tampered_order.receipt.included.swap(0, 1);
        let order_capsule = ContextCapsule::from(&tampered_order);
        assert!(matches!(
            compiler.validate_compiled_query(
                &query,
                &tampered_order,
                &order_capsule,
                None,
                accepted_at,
            ),
            Err(CompiledContextValidationError::NonCanonicalOrder { ref node_id })
                if node_id == "a-node"
        ));
    }

    #[test]
    fn embedded_rank_survives_token_budget_reversal_and_retains_lexical_scores() {
        let provider = RankingProvider::new()
            .with_document_vector("node-a", vec![0.0, 1.0])
            .with_document_vector("node-b", vec![1.0, 0.0]);
        let query = TaskQuery::with_provider(TaskSignatureV2::new("alpha beta"), Some(&provider))
            .expect("embedded query");
        let evaluated_at = Utc::now();
        let node_a = node("node-a", "alpha beta");
        let node_b = node("node-b", "alpha");
        let node_b_cost = ContextCapsuleItem::from(&node_b).token_cost();
        let ranking = ContextQueryRanking::new(
            &query,
            [node_a, node_b],
            evaluated_at,
            ContextResultLimit::new(2).expect("result limit"),
            Some(&provider),
        )
        .expect("context ranking");

        assert_eq!(provider.document_ids(), ["node-a", "node-b"]);
        let compiler = ContextCompiler::default();
        let full = compiler
            .compile_ranked_query(&ranking, None)
            .expect("full ranked context");
        assert_eq!(
            full.nodes
                .iter()
                .map(|candidate| candidate.id.as_str())
                .collect::<Vec<_>>(),
            ["node-b", "node-a"]
        );
        assert!(full.receipt.included[0].score < full.receipt.included[1].score);

        let constrained = compiler
            .compile_ranked_query(&ranking, Some(node_b_cost))
            .expect("budgeted ranked context");
        assert_eq!(
            constrained
                .nodes
                .iter()
                .map(|candidate| candidate.id.as_str())
                .collect::<Vec<_>>(),
            ["node-b"]
        );
        assert!(constrained.receipt.excluded.iter().any(|entry| {
            entry.node_id == "node-a" && entry.reason == ContextExclusionReason::TokenBudget
        }));
        let capsule = ContextCapsule::from(&constrained);
        compiler
            .validate_compiled_ranked_query(&ranking, &constrained, &capsule, Some(node_b_cost))
            .expect("authenticated embedded order validates");
    }

    #[test]
    fn ranked_query_backfills_lower_candidate_when_top_rank_does_not_fit() {
        let provider = RankingProvider::new()
            .with_document_vector("large-top", vec![1.0, 0.0])
            .with_document_vector("small-lower", vec![1.0, 1.0]);
        let query = TaskQuery::with_provider(TaskSignatureV2::new("alpha"), Some(&provider))
            .expect("embedded query");
        let large = node("large-top", &format!("alpha {}", "large ".repeat(800)));
        let small = node("small-lower", "alpha");
        let small_cost = ContextCapsuleItem::from(&small).token_cost();
        assert!(ContextCapsuleItem::from(&large).token_cost() > small_cost);
        let ranking = ContextQueryRanking::new(
            &query,
            [large, small],
            Utc::now(),
            ContextResultLimit::new(2).expect("result limit"),
            Some(&provider),
        )
        .expect("context ranking");

        let compiled = ContextCompiler::default()
            .compile_ranked_query(&ranking, Some(small_cost))
            .expect("backfilled context");
        assert_eq!(compiled.nodes.len(), 1);
        assert_eq!(compiled.nodes[0].id, "small-lower");
        assert_eq!(
            compiled
                .receipt
                .excluded
                .last()
                .map(|entry| entry.node_id.as_str()),
            Some("large-top")
        );
        assert_eq!(
            compiled.receipt.excluded.last().map(|entry| &entry.reason),
            Some(&ContextExclusionReason::TokenBudget)
        );
    }

    #[test]
    fn exact_context_id_beats_maximum_cosine_and_all_eligible_documents_embed_in_id_order() {
        let provider = RankingProvider::new()
            .with_document_vector("exact-node", vec![0.0, 1.0])
            .with_document_vector("lexical-id", vec![1.0, 0.0]);
        let query = TaskQuery::with_provider(
            TaskSignatureV2 {
                request: "target".into(),
                entities: vec!["exact-node".into()],
                ..TaskSignatureV2::default()
            },
            Some(&provider),
        )
        .expect("embedded query");
        let ranking = ContextQueryRanking::new(
            &query,
            [
                node("lexical-id", "target"),
                node("exact-node", "unrelated"),
                node("irrelevant-id", "nothing useful"),
            ],
            Utc::now(),
            ContextResultLimit::new(1).expect("result limit"),
            Some(&provider),
        )
        .expect("context ranking");
        assert_eq!(provider.document_ids(), ["exact-node", "lexical-id"]);
        assert_eq!(ranking.len(), 1);

        let compiled = ContextCompiler::default()
            .compile_ranked_query(&ranking, None)
            .expect("compile exact context");
        assert_eq!(compiled.nodes[0].id, "exact-node");
        assert!(compiled.receipt.excluded.iter().any(|entry| {
            entry.node_id == "irrelevant-id" && entry.reason == ContextExclusionReason::Irrelevant
        }));
    }

    #[test]
    fn context_ranking_shares_aggregate_and_provider_budgets_before_work_escapes() {
        let lexical_query = TaskQuery::new(TaskSignatureV2::new("target")).expect("query");
        let mut exhausted_candidates = RetrievalWorkBudget::new();
        exhausted_candidates
            .charge_candidate_bytes(MAX_TOTAL_CANDIDATE_BYTES)
            .expect("exact candidate-byte maximum");
        let error = ContextQueryRanking::new_with_budget(
            &lexical_query,
            [node("target", "target")],
            Utc::now(),
            ContextResultLimit::new(1).expect("result limit"),
            None,
            &mut exhausted_candidates,
        )
        .expect_err("candidate-byte N+1");
        assert!(matches!(
            error,
            ContextQueryRankingError::Retrieval(RetrievalError::WorkBudgetExceeded {
                kind: RetrievalWorkKind::CandidateBytes,
                maximum: MAX_TOTAL_CANDIDATE_BYTES,
                ..
            })
        ));

        let provider = RankingProvider::new().with_document_vector("target", vec![1.0, 0.0]);
        let mut provider_budget = RetrievalWorkBudget::new();
        for _ in 0..(MAX_PROVIDER_CALLS - 1) {
            provider_budget
                .charge_provider_call(0)
                .expect("preload provider budget");
        }
        let embedded_query = TaskQuery::with_provider_and_budget(
            TaskSignatureV2::new("target"),
            Some(&provider),
            &mut provider_budget,
        )
        .expect("Nth provider call");
        let error = ContextQueryRanking::new_with_budget(
            &embedded_query,
            [node("target", "target")],
            Utc::now(),
            ContextResultLimit::new(1).expect("result limit"),
            Some(&provider),
            &mut provider_budget,
        )
        .expect_err("provider-call N+1");
        assert!(matches!(
            error,
            ContextQueryRankingError::Retrieval(RetrievalError::WorkBudgetExceeded {
                kind: RetrievalWorkKind::ProviderCalls,
                attempted,
                maximum,
            }) if attempted == MAX_PROVIDER_CALLS + 1 && maximum == MAX_PROVIDER_CALLS
        ));
        assert!(
            provider.document_ids().is_empty(),
            "over-budget document input must not reach the provider"
        );
    }

    #[test]
    fn maximum_size_ten_thousand_candidate_generator_stops_at_the_shared_byte_budget() {
        let query = TaskQuery::new(TaskSignatureV2::new("target")).expect("lexical query");
        let maximum_summary = "s".repeat(MAX_CONTEXT_NODE_SUMMARY_BYTES);
        let yielded = Cell::new(0_usize);
        let candidates = (0..MAX_CANDIDATE_COUNT).map(|index| {
            yielded.set(yielded.get() + 1);
            node(&format!("maximum-{index:05}"), &maximum_summary)
        });
        let mut budget = RetrievalWorkBudget::new();

        let error = ContextQueryRanking::new_with_budget(
            &query,
            candidates,
            Utc::now(),
            ContextResultLimit::new(1).expect("result limit"),
            None,
            &mut budget,
        )
        .expect_err("maximum-size generator must hit the cumulative candidate budget");

        assert_eq!(yielded.get(), MAX_CANDIDATE_COUNT);
        assert!(matches!(
            error,
            ContextQueryRankingError::Retrieval(RetrievalError::WorkBudgetExceeded {
                kind: RetrievalWorkKind::CandidateBytes,
                maximum: MAX_TOTAL_CANDIDATE_BYTES,
                ..
            })
        ));
        assert!(budget.candidate_bytes() <= MAX_TOTAL_CANDIDATE_BYTES);
        assert_eq!(budget.document_bytes(), 0);
        assert_eq!(budget.lexical_bytes(), 0);
        assert_eq!(budget.provider_calls(), 0);
        assert_eq!(budget.provider_input_bytes(), 0);
    }

    #[test]
    fn lexical_ranking_filters_inactive_and_irrelevant_nodes_deterministically() {
        let evaluated_at = Utc::now();
        let query = TaskQuery::new(TaskSignatureV2::new("alpha beta")).expect("lexical query");
        let mut disputed = node("d-disputed", "alpha beta");
        disputed.epistemic = EpistemicStatus::Disputed;
        let mut expired = node("e-expired", "alpha beta");
        expired.valid_until = Some(evaluated_at);
        let mut future = node("f-future", "alpha beta");
        future.valid_from = Some(evaluated_at + Duration::seconds(1));
        let ranking = ContextQueryRanking::new(
            &query,
            [
                node("b-low", "alpha"),
                future,
                node("z-irrelevant", "gamma"),
                disputed,
                node("z-high", "alpha beta"),
                node("a-low", "alpha"),
                expired,
            ],
            evaluated_at,
            ContextResultLimit::new(3).expect("result limit"),
            None,
        )
        .expect("lexical ranking");
        assert_eq!(ranking.len(), 3);

        let compiled = ContextCompiler::default()
            .compile_ranked_query(&ranking, None)
            .expect("compile lexical ranking");
        assert_eq!(
            compiled
                .nodes
                .iter()
                .map(|candidate| candidate.id.as_str())
                .collect::<Vec<_>>(),
            ["z-high", "a-low", "b-low"]
        );
        assert_eq!(
            compiled
                .receipt
                .excluded
                .iter()
                .map(|entry| (entry.node_id.as_str(), &entry.reason))
                .collect::<Vec<_>>(),
            [
                ("d-disputed", &ContextExclusionReason::DisputedOrExpired),
                ("e-expired", &ContextExclusionReason::DisputedOrExpired),
                ("f-future", &ContextExclusionReason::DisputedOrExpired),
                ("z-irrelevant", &ContextExclusionReason::Irrelevant),
            ]
        );
    }

    #[test]
    fn ranked_query_rejects_provider_mode_mismatch_before_consuming_candidates() {
        let provider = RankingProvider::new();
        let lexical = TaskQuery::new(TaskSignatureV2::new("alpha")).expect("lexical query");
        let consumed = std::cell::Cell::new(0_usize);
        let lexical_error = ContextQueryRanking::new(
            &lexical,
            std::iter::from_fn(|| {
                consumed.set(consumed.get() + 1);
                Some(node("never", "alpha"))
            }),
            Utc::now(),
            ContextResultLimit::new(1).expect("result limit"),
            Some(&provider),
        )
        .expect_err("lexical query rejects provider");
        assert_eq!(consumed.get(), 0);
        assert_eq!(
            lexical_error,
            ContextQueryRankingError::ProviderModeMismatch {
                mode: RetrievalMode::LexicalOnly,
                provider_present: true,
            }
        );

        let embedded = TaskQuery::with_provider(TaskSignatureV2::new("alpha"), Some(&provider))
            .expect("embedded query");
        let consumed = std::cell::Cell::new(0_usize);
        let embedded_error = ContextQueryRanking::new(
            &embedded,
            std::iter::from_fn(|| {
                consumed.set(consumed.get() + 1);
                Some(node("never", "alpha"))
            }),
            Utc::now(),
            ContextResultLimit::new(1).expect("result limit"),
            None,
        )
        .expect_err("embedded query requires provider");
        assert_eq!(consumed.get(), 0);
        assert_eq!(
            embedded_error,
            ContextQueryRankingError::ProviderModeMismatch {
                mode: RetrievalMode::Embedded,
                provider_present: false,
            }
        );
    }

    #[test]
    fn ranked_query_rejects_provider_failures_and_invalid_document_vectors_without_partial_result()
    {
        let cases = [
            (
                "provider-failure",
                RankingProvider {
                    fail_documents: true,
                    ..RankingProvider::new()
                },
            ),
            (
                "descriptor",
                RankingProvider {
                    document_descriptor: Some("other-context-ranking".into()),
                    ..RankingProvider::new()
                },
            ),
            (
                "dimension",
                RankingProvider {
                    document_vector_override: Some(vec![1.0, 0.0, 0.0]),
                    ..RankingProvider::new()
                },
            ),
            (
                "nonfinite",
                RankingProvider {
                    document_vector_override: Some(vec![f32::NAN, 0.0]),
                    ..RankingProvider::new()
                },
            ),
            (
                "zero",
                RankingProvider {
                    document_vector_override: Some(vec![0.0, 0.0]),
                    ..RankingProvider::new()
                },
            ),
        ];

        for (case, provider) in cases {
            let query = TaskQuery::with_provider(TaskSignatureV2::new("alpha"), Some(&provider))
                .expect("query embedding remains valid");
            let error = ContextQueryRanking::new(
                &query,
                [node("a-node", "alpha"), node("b-node", "alpha")],
                Utc::now(),
                ContextResultLimit::new(2).expect("result limit"),
                Some(&provider),
            )
            .expect_err("invalid document provider output fails the whole ranking");
            assert!(
                matches!(
                    (&case, &error),
                    (
                        &"provider-failure",
                        ContextQueryRankingError::Retrieval(RetrievalError::ProviderFailure { .. })
                    ) | (
                        &"descriptor",
                        ContextQueryRankingError::Retrieval(
                            RetrievalError::EmbeddingDescriptorMismatch { .. }
                        )
                    ) | (
                        &"dimension",
                        ContextQueryRankingError::Retrieval(
                            RetrievalError::EmbeddingDimensionMismatch { .. }
                        )
                    ) | (
                        &"nonfinite",
                        ContextQueryRankingError::Retrieval(
                            RetrievalError::NonFiniteEmbeddingValue { .. }
                        )
                    ) | (
                        &"zero",
                        ContextQueryRankingError::Retrieval(RetrievalError::ZeroEmbeddingVector)
                    )
                ),
                "unexpected {case} error: {error:?}"
            );
            assert_eq!(provider.document_ids(), ["a-node"]);
        }
    }

    #[test]
    fn ranked_query_enforces_candidate_node_document_and_result_bounds() {
        assert!(ContextResultLimit::new(0).is_err());
        assert!(ContextResultLimit::new(MAX_CONTEXT_RESULT_LIMIT).is_ok());
        assert!(ContextResultLimit::new(MAX_CONTEXT_RESULT_LIMIT + 1).is_err());

        let query = TaskQuery::new(TaskSignatureV2::new("alpha")).expect("lexical query");
        let duplicate = ContextQueryRanking::new(
            &query,
            [node("duplicate", "alpha"), node("duplicate", "alpha")],
            Utc::now(),
            ContextResultLimit::new(1).expect("result limit"),
            None,
        )
        .expect_err("duplicate nodes fail");
        assert!(matches!(
            duplicate,
            ContextQueryRankingError::DuplicateCandidate { ref node_id }
                if node_id == "duplicate"
        ));

        let mut nonfinite = node("nonfinite", "alpha");
        nonfinite.confidence = f32::NAN;
        assert!(matches!(
            ContextQueryRanking::new(
                &query,
                [nonfinite],
                Utc::now(),
                ContextResultLimit::new(1).expect("result limit"),
                None,
            ),
            Err(ContextQueryRankingError::InvalidCandidate { ref node_id, .. })
                if node_id == "nonfinite"
        ));

        let mut missing_provenance = node("missing-provenance", "alpha");
        missing_provenance.source_event_ids.clear();
        assert!(matches!(
            ContextQueryRanking::new(
                &query,
                [missing_provenance],
                Utc::now(),
                ContextResultLimit::new(1).expect("result limit"),
                None,
            ),
            Err(ContextQueryRankingError::InvalidCandidate { ref node_id, .. })
                if node_id == "missing-provenance"
        ));

        let maximum_node = node(
            &"i".repeat(MAX_CONTEXT_NODE_ID_BYTES),
            &"s".repeat(MAX_CONTEXT_NODE_SUMMARY_BYTES),
        );
        assert!(
            ContextQueryRanking::new(
                &query,
                [maximum_node],
                Utc::now(),
                ContextResultLimit::new(1).expect("result limit"),
                None,
            )
            .is_ok()
        );

        let oversized_id = node(&"i".repeat(MAX_CONTEXT_NODE_ID_BYTES + 1), "alpha");
        assert!(matches!(
            ContextQueryRanking::new(
                &query,
                [oversized_id],
                Utc::now(),
                ContextResultLimit::new(1).expect("result limit"),
                None,
            ),
            Err(ContextQueryRankingError::CandidateIdTooLong {
                actual: 257,
                maximum: 256,
            })
        ));

        let attacker_id_bytes = 1024 * 1024;
        let attacker_id_error = ContextQueryRanking::new(
            &query,
            [node(&"x".repeat(attacker_id_bytes), "alpha")],
            Utc::now(),
            ContextResultLimit::new(1).expect("result limit"),
            None,
        )
        .expect_err("attacker-sized id fails before retention or cloning");
        assert_eq!(
            attacker_id_error,
            ContextQueryRankingError::CandidateIdTooLong {
                actual: attacker_id_bytes,
                maximum: MAX_CONTEXT_NODE_ID_BYTES,
            }
        );
        assert!(
            attacker_id_error.to_string().len() < MAX_CONTEXT_NODE_ID_BYTES,
            "bounded error must not copy the attacker-sized id"
        );

        let oversized_summary = node(
            "oversized-summary",
            &"s".repeat(MAX_CONTEXT_NODE_SUMMARY_BYTES + 1),
        );
        assert!(matches!(
            ContextQueryRanking::new(
                &query,
                [oversized_summary],
                Utc::now(),
                ContextResultLimit::new(1).expect("result limit"),
                None,
            ),
            Err(ContextQueryRankingError::CandidateSummaryTooLong {
                actual: 65_001,
                maximum: 65_000,
                ..
            })
        ));

        let mut reference_and_list_maximum = node("reference-bounds", "alpha");
        reference_and_list_maximum.source_event_ids = (0..MAX_CONTEXT_SOURCE_EVENT_IDS)
            .map(|index| format!("source-{index}"))
            .collect();
        reference_and_list_maximum.supersedes = (0..MAX_CONTEXT_SUPERSEDES)
            .map(|index| format!("superseded-{index}"))
            .collect();
        assert!(
            ContextQueryRanking::new(
                &query,
                [reference_and_list_maximum.clone()],
                Utc::now(),
                ContextResultLimit::new(1).expect("result limit"),
                None,
            )
            .is_ok()
        );

        let mut source_count_over = reference_and_list_maximum.clone();
        source_count_over
            .source_event_ids
            .push("source-over".into());
        assert!(matches!(
            ContextQueryRanking::new(
                &query,
                [source_count_over],
                Utc::now(),
                ContextResultLimit::new(1).expect("result limit"),
                None,
            ),
            Err(ContextQueryRankingError::InvalidCandidate { ref reason, .. })
                if reason.contains("source_event_ids contains 65 entries")
        ));

        let mut supersedes_count_over = reference_and_list_maximum;
        supersedes_count_over
            .supersedes
            .push("superseded-over".into());
        assert!(matches!(
            ContextQueryRanking::new(
                &query,
                [supersedes_count_over],
                Utc::now(),
                ContextResultLimit::new(1).expect("result limit"),
                None,
            ),
            Err(ContextQueryRankingError::InvalidCandidate { ref reason, .. })
                if reason.contains("supersedes contains 65 entries")
        ));

        let mut reference_maximum = node("reference-maximum", "alpha");
        reference_maximum.source_event_ids = vec![reference_at_bound(0)];
        reference_maximum.supersedes = vec!["s".repeat(MAX_CONTEXT_REFERENCE_ID_BYTES)];
        assert!(
            ContextQueryRanking::new(
                &query,
                [reference_maximum.clone()],
                Utc::now(),
                ContextResultLimit::new(1).expect("result limit"),
                None,
            )
            .is_ok()
        );
        let mut reference_over = reference_maximum;
        reference_over.source_event_ids = vec!["r".repeat(MAX_CONTEXT_REFERENCE_ID_BYTES + 1)];
        assert!(matches!(
            ContextQueryRanking::new(
                &query,
                [reference_over],
                Utc::now(),
                ContextResultLimit::new(1).expect("result limit"),
                None,
            ),
            Err(ContextQueryRankingError::InvalidCandidate { ref reason, .. })
                if reason.contains("257 bytes")
        ));

        let mut supersession_reference_over = node("supersession-reference-over", "alpha");
        supersession_reference_over.supersedes =
            vec!["s".repeat(MAX_CONTEXT_REFERENCE_ID_BYTES + 1)];
        assert!(matches!(
            ContextQueryRanking::new(
                &query,
                [supersession_reference_over],
                Utc::now(),
                ContextResultLimit::new(1).expect("result limit"),
                None,
            ),
            Err(ContextQueryRankingError::InvalidCandidate { ref reason, .. })
                if reason.contains("supersedes reference at index 0 is 257 bytes")
        ));

        for (node_id, sources, supersedes, expected_reason) in [
            (
                "empty-source",
                vec![" ".into()],
                Vec::new(),
                "no source event provenance",
            ),
            (
                "duplicate-source",
                vec!["event-a".into(), "event-a".into()],
                Vec::new(),
                "source_event_ids contains duplicate reference",
            ),
            (
                "duplicate-supersedes",
                vec!["event-a".into()],
                vec!["old".into(), "old".into()],
                "supersedes contains duplicate reference",
            ),
            (
                "empty-supersedes",
                vec!["event-a".into()],
                vec![" ".into()],
                "supersedes reference at index 0 is empty",
            ),
            (
                "self-supersedes",
                vec!["event-a".into()],
                vec!["self-supersedes".into()],
                "supersedes contains the node's own id",
            ),
        ] {
            let mut invalid = node(node_id, "alpha");
            invalid.source_event_ids = sources;
            invalid.supersedes = supersedes;
            assert!(matches!(
                ContextQueryRanking::new(
                    &query,
                    [invalid],
                    Utc::now(),
                    ContextResultLimit::new(1).expect("result limit"),
                    None,
                ),
                Err(ContextQueryRankingError::InvalidCandidate { ref reason, .. })
                    if reason.contains(expected_reason)
            ));
        }

        let serialized_maximum = node_with_serialized_len(MAX_SERIALIZED_CONTEXT_NODE_BYTES);
        assert!(
            ContextQueryRanking::new(
                &query,
                [serialized_maximum],
                Utc::now(),
                ContextResultLimit::new(1).expect("result limit"),
                None,
            )
            .is_ok()
        );
        let serialized_over = node_with_serialized_len(MAX_SERIALIZED_CONTEXT_NODE_BYTES + 1);
        assert!(matches!(
            ContextQueryRanking::new(
                &query,
                [serialized_over],
                Utc::now(),
                ContextResultLimit::new(1).expect("result limit"),
                None,
            ),
            Err(ContextQueryRankingError::InvalidCandidate { ref reason, .. })
                if reason.contains("131073 bytes")
        ));

        let over =
            (0..=MAX_CANDIDATE_COUNT).map(|index| node(&format!("candidate-{index}"), "alpha"));
        assert!(matches!(
            ContextQueryRanking::new(
                &query,
                over,
                Utc::now(),
                ContextResultLimit::new(1).expect("result limit"),
                None,
            ),
            Err(ContextQueryRankingError::Retrieval(
                RetrievalError::CandidateCountExceeded {
                    actual: 10_001,
                    maximum: 10_000,
                }
            ))
        ));

        let oversized_first =
            std::iter::once(node(&"x".repeat(MAX_CONTEXT_NODE_ID_BYTES + 1), "alpha")).chain(
                (0..MAX_CANDIDATE_COUNT)
                    .map(|index| node(&format!("count-precedence-{index}"), "alpha")),
            );
        assert!(matches!(
            ContextQueryRanking::new(
                &query,
                oversized_first,
                Utc::now(),
                ContextResultLimit::new(1).expect("result limit"),
                None,
            ),
            Err(ContextQueryRankingError::Retrieval(
                RetrievalError::CandidateCountExceeded {
                    actual: 10_001,
                    maximum: 10_000,
                }
            ))
        ));

        let accepted =
            (0..MAX_CANDIDATE_COUNT).map(|index| node(&format!("bounded-{index}"), "nothing"));
        assert!(
            ContextQueryRanking::new(
                &query,
                accepted,
                Utc::now(),
                ContextResultLimit::new(1).expect("result limit"),
                None,
            )
            .is_ok()
        );
    }

    fn ranked_validation_fixture() -> (
        ContextCompiler,
        ContextQueryRanking,
        super::CompiledContext,
        ContextCapsule,
    ) {
        let evaluated_at = Utc::now();
        let query = TaskQuery::new(TaskSignatureV2::new("alpha beta")).expect("lexical query");
        let mut disputed = node("y-disputed", "alpha beta");
        disputed.epistemic = EpistemicStatus::Disputed;
        let ranking = ContextQueryRanking::new(
            &query,
            [
                node("b-node", "alpha"),
                node("z-irrelevant", "gamma"),
                node("a-node", "alpha beta"),
                disputed,
            ],
            evaluated_at,
            ContextResultLimit::new(2).expect("result limit"),
            None,
        )
        .expect("context ranking");
        let compiler = ContextCompiler::default();
        let compiled = compiler
            .compile_ranked_query(&ranking, None)
            .expect("compiled ranked context");
        let capsule = ContextCapsule::from(&compiled);
        (compiler, ranking, compiled, capsule)
    }

    #[test]
    fn ranked_compiled_validation_rejects_order_score_token_exclusion_reason_and_accounting_tamper()
    {
        let (compiler, ranking, compiled, _) = ranked_validation_fixture();
        compiler
            .validate_compiled_ranked_query(
                &ranking,
                &compiled,
                &ContextCapsule::from(&compiled),
                None,
            )
            .expect("canonical ranked result validates");

        let mut order = compiled.clone();
        order.nodes.swap(0, 1);
        order.receipt.included.swap(0, 1);
        assert!(matches!(
            compiler.validate_compiled_ranked_query(
                &ranking,
                &order,
                &ContextCapsule::from(&order),
                None,
            ),
            Err(CompiledContextValidationError::NonCanonicalOrder { .. })
        ));

        let mut score = compiled.clone();
        score.receipt.included[0].score += 1.0;
        assert!(matches!(
            compiler.validate_compiled_ranked_query(
                &ranking,
                &score,
                &ContextCapsule::from(&score),
                None,
            ),
            Err(CompiledContextValidationError::ScoreMismatch { .. })
        ));

        let mut token = compiled.clone();
        token.receipt.included[0].token_cost += 1;
        assert!(matches!(
            compiler.validate_compiled_ranked_query(
                &ranking,
                &token,
                &ContextCapsule::from(&token),
                None,
            ),
            Err(CompiledContextValidationError::TokenCostMismatch { .. })
        ));

        let mut exclusion_order = compiled.clone();
        exclusion_order.receipt.excluded.swap(0, 1);
        assert!(matches!(
            compiler.validate_compiled_ranked_query(
                &ranking,
                &exclusion_order,
                &ContextCapsule::from(&exclusion_order),
                None,
            ),
            Err(CompiledContextValidationError::ExclusionReceiptMismatch { .. })
        ));

        let mut exclusion_reason = compiled.clone();
        exclusion_reason.receipt.excluded[0].reason = ContextExclusionReason::TokenBudget;
        assert!(matches!(
            compiler.validate_compiled_ranked_query(
                &ranking,
                &exclusion_reason,
                &ContextCapsule::from(&exclusion_reason),
                None,
            ),
            Err(CompiledContextValidationError::ExclusionReceiptMismatch { .. })
        ));

        let mut elevated_reason = compiled.clone();
        elevated_reason.receipt.included[0].reason = "user-pinned".into();
        assert!(matches!(
            compiler.validate_compiled_ranked_query(
                &ranking,
                &elevated_reason,
                &ContextCapsule::from(&elevated_reason),
                None,
            ),
            Err(CompiledContextValidationError::InvalidReceiptReason { .. })
        ));

        let mut accounting = compiled;
        accounting.receipt.total_token_cost += 1;
        assert!(matches!(
            compiler.validate_compiled_ranked_query(
                &ranking,
                &accounting,
                &ContextCapsule::from(&accounting),
                None,
            ),
            Err(CompiledContextValidationError::TokenAccountingMismatch { .. })
        ));
    }
}
