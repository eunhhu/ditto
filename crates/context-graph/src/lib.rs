//! Typed task-context intermediate representation with explicit provenance.

use std::collections::BTreeMap;

use ditto_protocol::new_id;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Lens {
    Personal,
    Task,
    Environment,
    Conversation,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
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

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    User,
    Model,
    Tool,
    Policy,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Epistemic {
    Asserted,
    Inferred,
    Verified,
    Disputed,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    Turn,
    Session,
    Task,
    Project,
    Device,
    Global,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ContextNode {
    pub id: String,
    pub lens: Lens,
    pub kind: NodeKind,
    pub label: String,
    pub origin: Origin,
    pub epistemic: Epistemic,
    pub scope: Scope,
    pub confidence: f32,
    pub source_event_ids: Vec<i64>,
    pub valid_from_ms: i64,
    pub valid_until_ms: Option<i64>,
    pub supersedes: Vec<String>,
    pub token_cost: usize,
    pub pinned: bool,
    pub utility: Utility,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct Utility {
    pub semantic_relevance: f32,
    pub graph_proximity: f32,
    pub source_authority: f32,
    pub task_urgency: f32,
    pub risk_relevance: f32,
    pub staleness: f32,
    pub contradiction_penalty: f32,
}

impl Utility {
    pub fn score(self, token_cost: usize) -> f32 {
        let bounded_token_cost = u16::try_from(token_cost).unwrap_or(u16::MAX);
        self.semantic_relevance
            + self.graph_proximity
            + self.source_authority
            + self.task_urgency
            + self.risk_relevance
            - self.staleness
            - self.contradiction_penalty
            - f32::from(bounded_token_cost) * 0.001
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ContextEdge {
    pub from: String,
    pub relation: String,
    pub to: String,
    pub source_event_ids: Vec<i64>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct ContextGraph {
    nodes: BTreeMap<String, ContextNode>,
    edges: Vec<ContextEdge>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GraphValidationError {
    MissingProvenance(String),
    ModelAssertion(String),
    InvalidConfidence(String),
}

impl ContextGraph {
    /// Inserts a provenance-valid context node.
    ///
    /// # Errors
    ///
    /// Rejects missing provenance, invalid confidence, and model-authored assertions.
    pub fn insert_node(&mut self, node: ContextNode) -> Result<(), GraphValidationError> {
        validate_node(&node)?;
        self.nodes.insert(node.id.clone(), node);
        Ok(())
    }

    /// Inserts an edge whose origin can be traced to at least one event.
    ///
    /// # Errors
    ///
    /// Rejects edges without source event IDs.
    pub fn insert_edge(&mut self, edge: ContextEdge) -> Result<(), GraphValidationError> {
        if edge.source_event_ids.is_empty() {
            return Err(GraphValidationError::MissingProvenance(format!(
                "edge {} -> {}",
                edge.from, edge.to
            )));
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

impl ContextNode {
    pub fn user_goal(label: impl Into<String>, source_event_id: i64, timestamp_ms: i64) -> Self {
        let label = label.into();
        Self {
            id: new_id("ctx"),
            lens: Lens::Task,
            kind: NodeKind::Goal,
            token_cost: label.chars().count().div_ceil(4).max(1),
            label,
            origin: Origin::User,
            epistemic: Epistemic::Asserted,
            scope: Scope::Task,
            confidence: 1.0,
            source_event_ids: vec![source_event_id],
            valid_from_ms: timestamp_ms,
            valid_until_ms: None,
            supersedes: Vec::new(),
            pinned: true,
            utility: Utility {
                semantic_relevance: 1.0,
                source_authority: 1.0,
                task_urgency: 1.0,
                ..Utility::default()
            },
        }
    }
}

fn validate_node(node: &ContextNode) -> Result<(), GraphValidationError> {
    if !(0.0..=1.0).contains(&node.confidence) {
        return Err(GraphValidationError::InvalidConfidence(node.id.clone()));
    }
    if node.scope != Scope::Turn && node.source_event_ids.is_empty() {
        return Err(GraphValidationError::MissingProvenance(node.id.clone()));
    }
    if node.origin == Origin::Model && node.epistemic == Epistemic::Asserted {
        return Err(GraphValidationError::ModelAssertion(node.id.clone()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_inference_cannot_be_presented_as_asserted_fact() {
        let mut node = ContextNode::user_goal("ship", 1, 0);
        node.origin = Origin::Model;
        let error = ContextGraph::default().insert_node(node).unwrap_err();
        assert!(matches!(error, GraphValidationError::ModelAssertion(_)));
    }

    #[test]
    fn durable_nodes_require_provenance() {
        let mut node = ContextNode::user_goal("ship", 1, 0);
        node.source_event_ids.clear();
        let error = ContextGraph::default().insert_node(node).unwrap_err();
        assert!(matches!(error, GraphValidationError::MissingProvenance(_)));
    }
}
