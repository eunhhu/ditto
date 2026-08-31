use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

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
}

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextReceiptEntry {
    pub node_id: String,
    pub source_event_ids: Vec<String>,
    pub epistemic: EpistemicStatus,
    pub reason: String,
    pub score: f32,
    pub token_cost: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextExclusionReason {
    Invalid,
    DisputedOrExpired,
    Irrelevant,
    TokenBudget,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextExclusion {
    pub node_id: String,
    pub reason: ContextExclusionReason,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextReceipt {
    pub included: Vec<ContextReceiptEntry>,
    pub excluded: Vec<ContextExclusion>,
    pub total_token_cost: u32,
    pub token_budget: u32,
    pub absolute_budget: u32,
    pub over_soft_budget: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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
}

struct PreparedCandidate {
    directive: ContextDirective,
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
    use chrono::{Duration, Utc};

    use super::{
        CAPSULE_ITEM_FIXED_OVERHEAD_TOKENS, CompiledContextValidationError, ContextCandidate,
        ContextCapsule, ContextCapsuleItem, ContextCapsuleValidationError, ContextCompileError,
        ContextCompiler, ContextEdge, ContextEdgeKind, ContextExclusion, ContextExclusionReason,
        ContextGraph, ContextLens, ContextNode, ContextNodeKind, ContextOrigin, ContextScope,
        ContextValidationError, DEFAULT_CONTEXT_ABSOLUTE_BUDGET, EpistemicStatus, TaskSignature,
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
}
