//! Deterministic task-context compiler. No model call is made on the hot path.

use std::fmt::Write as _;

use ditto_context_graph::{ContextGraph, ContextNode, Epistemic, NodeKind, Origin};
use ditto_protocol::{ContextReceipt, ContextReceiptItem, EffectClass};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct TaskSignature {
    pub normalized_request: String,
    pub active_goal: Option<String>,
    pub explicit_entities: Vec<String>,
    pub unresolved_constraints: Vec<String>,
    pub expected_effect: Option<EffectClass>,
}

#[derive(Clone, Debug)]
pub struct ContextCompiler {
    token_budget: usize,
}

impl ContextCompiler {
    pub fn new(token_budget: usize) -> Self {
        Self { token_budget }
    }

    pub fn compile(
        &self,
        signature: &TaskSignature,
        graph: &ContextGraph,
        now_ms: i64,
    ) -> ContextReceipt {
        let mut candidates = graph
            .nodes()
            .filter(|node| node.valid_until_ms.is_none_or(|until| until >= now_ms))
            .collect::<Vec<_>>();
        candidates.sort_by(|left, right| {
            right
                .pinned
                .cmp(&left.pinned)
                .then_with(|| {
                    right
                        .utility
                        .score(right.token_cost)
                        .total_cmp(&left.utility.score(left.token_cost))
                })
                .then_with(|| left.id.cmp(&right.id))
        });

        let mut selected = Vec::new();
        let mut spent = estimate_tokens(&signature.normalized_request);
        for node in candidates {
            let forced = node.pinned || matches!(node.kind, NodeKind::Risk | NodeKind::Evidence);
            if forced || spent.saturating_add(node.token_cost) <= self.token_budget {
                spent = spent.saturating_add(node.token_cost);
                selected.push(node);
            }
        }

        let capsule = render_capsule(signature, &selected);
        let included = selected
            .iter()
            .map(|node| receipt_item(node))
            .collect::<Vec<_>>();

        ContextReceipt {
            token_estimate: estimate_tokens(&capsule),
            capsule,
            included,
        }
    }
}

impl Default for ContextCompiler {
    fn default() -> Self {
        Self::new(900)
    }
}

fn render_capsule(signature: &TaskSignature, nodes: &[&ContextNode]) -> String {
    let mut output = format!("[TASK]\n{}\n", signature.normalized_request.trim());
    render_section(
        &mut output,
        "ACTIVE CONSTRAINTS",
        signature
            .unresolved_constraints
            .iter()
            .map(String::as_str)
            .chain(labels(nodes, NodeKind::Constraint)),
    );
    render_section(
        &mut output,
        "CURRENT STATE",
        labels_for(
            nodes,
            &[NodeKind::Decision, NodeKind::Action, NodeKind::Resource],
        ),
    );
    render_section(
        &mut output,
        "RELEVANT CONTEXT",
        labels_for(
            nodes,
            &[
                NodeKind::Claim,
                NodeKind::Preference,
                NodeKind::Capability,
                NodeKind::Goal,
            ],
        ),
    );
    render_section(
        &mut output,
        "UNCERTAINTY",
        labels_for(
            nodes,
            &[NodeKind::Assumption, NodeKind::OpenQuestion, NodeKind::Risk],
        ),
    );
    render_section(
        &mut output,
        "COMPLETION EVIDENCE",
        labels(nodes, NodeKind::Evidence),
    );
    output
}

fn render_section<'a>(output: &mut String, heading: &str, values: impl Iterator<Item = &'a str>) {
    write!(output, "\n[{heading}]\n").expect("writing to a String cannot fail");
    let values = values.collect::<Vec<_>>();
    if values.is_empty() {
        output.push_str("- None\n");
    } else {
        for value in values {
            output.push_str("- ");
            output.push_str(value);
            output.push('\n');
        }
    }
}

fn labels<'a>(nodes: &'a [&'a ContextNode], kind: NodeKind) -> impl Iterator<Item = &'a str> {
    nodes
        .iter()
        .filter(move |node| node.kind == kind)
        .map(|node| node.label.as_str())
}

fn labels_for<'a>(
    nodes: &'a [&'a ContextNode],
    kinds: &'a [NodeKind],
) -> impl Iterator<Item = &'a str> {
    nodes
        .iter()
        .filter(move |node| kinds.contains(&node.kind))
        .map(|node| node.label.as_str())
}

fn receipt_item(node: &ContextNode) -> ContextReceiptItem {
    let source = if node.source_event_ids.is_empty() {
        "current turn".to_owned()
    } else {
        format!("events:{:?}", node.source_event_ids)
    };
    ContextReceiptItem {
        node_id: node.id.clone(),
        label: node.label.clone(),
        source,
        epistemic: format!(
            "{} {}",
            origin_name(node.origin),
            epistemic_name(node.epistemic)
        ),
        reason: if node.pinned {
            "pinned task context".to_owned()
        } else {
            "task utility score".to_owned()
        },
    }
}

fn origin_name(origin: Origin) -> &'static str {
    match origin {
        Origin::User => "user",
        Origin::Model => "model",
        Origin::Tool => "tool",
        Origin::Policy => "policy",
    }
}

fn epistemic_name(epistemic: Epistemic) -> &'static str {
    match epistemic {
        Epistemic::Asserted => "asserted",
        Epistemic::Inferred => "inferred",
        Epistemic::Verified => "verified",
        Epistemic::Disputed => "disputed",
    }
}

fn estimate_tokens(text: &str) -> usize {
    text.chars().count().div_ceil(4).max(1)
}

#[cfg(test)]
mod tests {
    use ditto_context_graph::ContextNode;

    use super::*;

    #[test]
    fn receipt_preserves_source_and_reason() {
        let mut graph = ContextGraph::default();
        graph
            .insert_node(ContextNode::user_goal("Keep context small", 7, 0))
            .unwrap();
        let signature = TaskSignature {
            normalized_request: "Scaffold runtime".to_owned(),
            ..TaskSignature::default()
        };
        let receipt = ContextCompiler::new(10).compile(&signature, &graph, 0);
        assert_eq!(receipt.included.len(), 1);
        assert_eq!(receipt.included[0].source, "events:[7]");
        assert!(receipt.capsule.contains("Keep context small"));
    }
}
