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
    pub token_cost: u32,
    #[serde(default)]
    pub pinned: bool,
    #[serde(default)]
    pub force_include: bool,
}

impl ContextNode {
    pub fn validate(&self) -> Result<(), ContextValidationError> {
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextReceiptEntry {
    pub node_id: String,
    pub reason: String,
    pub score: f32,
    pub token_cost: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextReceipt {
    pub included: Vec<ContextReceiptEntry>,
    pub excluded_count: usize,
    #[serde(default)]
    pub invalid_count: usize,
    pub total_token_cost: u32,
    pub token_budget: u32,
    pub over_budget: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompiledContext {
    pub nodes: Vec<ContextNode>,
    pub receipt: ContextReceipt,
}

#[derive(Debug, Clone, Copy)]
pub struct ContextCompiler {
    pub default_budget: u32,
}

impl Default for ContextCompiler {
    fn default() -> Self {
        Self {
            default_budget: 900,
        }
    }
}

impl ContextCompiler {
    pub fn compile(
        &self,
        signature: &TaskSignature,
        nodes: impl IntoIterator<Item = ContextNode>,
        token_budget: Option<u32>,
        now: DateTime<Utc>,
    ) -> CompiledContext {
        let token_budget = token_budget.unwrap_or(self.default_budget);
        let query_tokens = tokenize(&signature.searchable_text());
        let mut forced = Vec::new();
        let mut candidates = Vec::new();
        let mut excluded_count = 0;
        let mut invalid_count = 0;

        for node in nodes {
            if node.validate().is_err() {
                excluded_count += 1;
                invalid_count += 1;
                continue;
            }
            if !node.is_valid_at(now) {
                excluded_count += 1;
                continue;
            }

            let score = relevance_score(&node, &query_tokens);
            if node.pinned || node.force_include {
                forced.push((score, node));
            } else if score > 0.0 {
                candidates.push((score, node));
            } else {
                excluded_count += 1;
            }
        }

        forced.sort_by(stable_score_order);
        candidates.sort_by(stable_score_order);

        let mut selected = Vec::new();
        let mut receipt = Vec::new();
        let mut used = 0_u32;

        for (score, node) in forced {
            used = used.saturating_add(node.token_cost);
            receipt.push(ContextReceiptEntry {
                node_id: node.id.clone(),
                reason: if node.pinned {
                    "user-pinned".into()
                } else {
                    "forced-by-compiler-rule".into()
                },
                score,
                token_cost: node.token_cost,
            });
            selected.push(node);
        }

        for (score, node) in candidates {
            if used.saturating_add(node.token_cost) > token_budget {
                excluded_count += 1;
                continue;
            }
            used += node.token_cost;
            receipt.push(ContextReceiptEntry {
                node_id: node.id.clone(),
                reason: "task-relevance".into(),
                score,
                token_cost: node.token_cost,
            });
            selected.push(node);
        }

        CompiledContext {
            nodes: selected,
            receipt: ContextReceipt {
                included: receipt,
                excluded_count,
                invalid_count,
                total_token_cost: used,
                token_budget,
                over_budget: used > token_budget,
            },
        }
    }
}

fn stable_score_order(
    (left_score, left): &(f32, ContextNode),
    (right_score, right): &(f32, ContextNode),
) -> std::cmp::Ordering {
    right_score
        .total_cmp(left_score)
        .then_with(|| left.id.cmp(&right.id))
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

    overlap * 5.0 + authority + node.confidence.clamp(0.0, 1.0)
}

fn has_provenance(source_event_ids: &[String]) -> bool {
    !source_event_ids.is_empty() && source_event_ids.iter().all(|id| !id.trim().is_empty())
}

fn tokenize(input: &str) -> HashSet<String> {
    input
        .split(|character: char| !character.is_alphanumeric())
        .filter(|token| token.len() > 1)
        .map(str::to_lowercase)
        .collect()
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::{
        ContextCompiler, ContextEdge, ContextEdgeKind, ContextGraph, ContextLens, ContextNode,
        ContextNodeKind, ContextOrigin, ContextScope, ContextValidationError, EpistemicStatus,
        TaskSignature,
    };

    fn node(id: &str, summary: &str, token_cost: u32, pinned: bool) -> ContextNode {
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
            token_cost,
            pinned,
            force_include: false,
        }
    }

    #[test]
    fn pinned_context_survives_the_budget() {
        let compiler = ContextCompiler::default();
        let signature = TaskSignature {
            request: "restart the home server service".into(),
            ..TaskSignature::default()
        };
        let compiled = compiler.compile(
            &signature,
            [
                node("pinned", "always ask before sudo", 60, true),
                node("relevant", "home server service uses systemd", 60, false),
            ],
            Some(80),
            Utc::now(),
        );

        assert!(compiled.nodes.iter().any(|item| item.id == "pinned"));
        assert_eq!(compiled.receipt.total_token_cost, 60);
    }

    #[test]
    fn rejects_context_without_valid_provenance() {
        let mut graph = ContextGraph::default();
        let mut durable = node("durable", "project constraint", 10, false);
        durable.source_event_ids.clear();
        assert!(matches!(
            graph.insert_node(durable),
            Err(ContextValidationError::MissingProvenance { .. })
        ));

        let mut model_assertion = node("model", "model claim", 10, false);
        model_assertion.origin = ContextOrigin::Model;
        assert!(matches!(
            graph.insert_node(model_assertion),
            Err(ContextValidationError::ModelCannotAssert { .. })
        ));

        let mut invalid_confidence = node("confidence", "bad confidence", 10, false);
        invalid_confidence.confidence = 1.1;
        assert!(matches!(
            graph.insert_node(invalid_confidence),
            Err(ContextValidationError::InvalidConfidence { .. })
        ));
    }

    #[test]
    fn validates_edge_provenance_and_endpoints() {
        let mut graph = ContextGraph::default();
        graph
            .insert_node(node("claim", "service state", 10, false))
            .expect("insert claim");
        graph
            .insert_node(node("evidence", "service output", 10, false))
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
        let compiler = ContextCompiler::default();
        let signature = TaskSignature {
            request: "restart database service".into(),
            ..TaskSignature::default()
        };
        let compiled = compiler.compile(
            &signature,
            [node(
                "irrelevant",
                "preferred vacation destination",
                10,
                false,
            )],
            None,
            Utc::now(),
        );

        assert!(compiled.nodes.is_empty());
        assert_eq!(compiled.receipt.excluded_count, 1);
    }
}
