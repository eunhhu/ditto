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
#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ContextCompileError {
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
            default_budget: 900,
            absolute_budget: 1_800,
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
        let query_tokens = tokenize(&signature.searchable_text());
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
                excluded.push(ContextExclusion {
                    node_id,
                    reason: ContextExclusionReason::DisputedOrExpired,
                    detail: None,
                });
                continue;
            }

            let token_cost = estimate_tokens(&candidate.node.summary);
            let score = relevance_score(&candidate.node, &query_tokens);
            if candidate.directive.is_required() {
                required.push((candidate.directive, score, token_cost, candidate.node));
            } else if score > 0.0 {
                ranked.push((candidate.directive, score, token_cost, candidate.node));
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

        let required_cost = required
            .iter()
            .fold(0_u32, |total, (_, _, cost, _)| total.saturating_add(*cost));
        if required_cost > self.absolute_budget {
            return Err(ContextCompileError::RequiredContextBudgetExceeded {
                used: required_cost,
                absolute_budget: self.absolute_budget,
            });
        }

        let mut selected = Vec::new();
        let mut included = Vec::new();
        let mut used = 0_u32;

        for (directive, score, token_cost, node) in required {
            used = used.saturating_add(token_cost);
            included.push(receipt_entry(&node, &directive, score, token_cost));
            selected.push(node);
        }

        for (directive, score, token_cost, node) in ranked {
            if used.saturating_add(token_cost) > token_budget {
                excluded.push(ContextExclusion {
                    node_id: node.id,
                    reason: ContextExclusionReason::TokenBudget,
                    detail: None,
                });
                continue;
            }
            used += token_cost;
            included.push(receipt_entry(&node, &directive, score, token_cost));
            selected.push(node);
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
}

fn candidate_order(
    (left_directive, left_score, _, left): &(ContextDirective, f32, u32, ContextNode),
    (right_directive, right_score, _, right): &(ContextDirective, f32, u32, ContextNode),
) -> std::cmp::Ordering {
    right_directive
        .priority()
        .cmp(&left_directive.priority())
        .then_with(|| right_score.total_cmp(left_score))
        .then_with(|| left.id.cmp(&right.id))
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

fn estimate_tokens(text: &str) -> u32 {
    let estimate = text.chars().count().div_ceil(4).max(1);
    u32::try_from(estimate).unwrap_or(u32::MAX)
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
    use chrono::Utc;

    use super::{
        ContextCandidate, ContextCompileError, ContextCompiler, ContextEdge, ContextEdgeKind,
        ContextGraph, ContextLens, ContextNode, ContextNodeKind, ContextOrigin, ContextScope,
        ContextValidationError, EpistemicStatus, TaskSignature,
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
            absolute_budget: 50,
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
}
