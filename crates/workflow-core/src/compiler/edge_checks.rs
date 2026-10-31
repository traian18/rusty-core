//! Checks on the edges between nodes, and on the routes each node leaves open.

use std::collections::BTreeSet;

use super::{
    graph::{Edges, Nodes},
    Issues,
};
use crate::definition::{EdgeCondition, OrchestrationDefinition, OrchestrationNodeKind};

/// Checks every edge and returns the edges by source (outgoing) and by target (incoming). An edge
/// whose ends do not both exist is reported and left out.
pub(super) fn build_edges(
    definition: &OrchestrationDefinition,
    nodes: &Nodes,
    issues: &mut Issues,
) -> (Edges, Edges) {
    let mut outgoing: Edges = nodes.keys().cloned().map(|id| (id, Vec::new())).collect();
    let mut incoming = outgoing.clone();
    let mut edge_ids = BTreeSet::new();
    let mut transition_keys = BTreeSet::new();
    for (index, edge) in definition.edges.iter().enumerate() {
        let path = format!("edges[{index}]");
        if !edge_ids.insert(edge.id.clone()) {
            issues.add(
                format!("{path}.id"),
                "duplicate_edge_id",
                format!("duplicate edge id {}", edge.id),
            );
        }
        if !nodes.contains_key(&edge.source) {
            issues.add(
                format!("{path}.source"),
                "dangling_edge",
                format!("source node {} does not exist", edge.source),
            );
            continue;
        }
        if !nodes.contains_key(&edge.target) {
            issues.add(
                format!("{path}.target"),
                "dangling_edge",
                format!("target node {} does not exist", edge.target),
            );
            continue;
        }
        if !transition_keys.insert((edge.source.clone(), edge.condition)) {
            issues.add(
                &path,
                "ambiguous_transition",
                format!(
                    "node {} has multiple {:?} edges",
                    edge.source, edge.condition
                ),
            );
        }
        outgoing
            .entry(edge.source.clone())
            .or_default()
            .push(edge.clone());
        incoming
            .entry(edge.target.clone())
            .or_default()
            .push(edge.clone());
    }
    (outgoing, incoming)
}

/// Output nodes end the flow; every other node needs an `on_success` route.
pub(super) fn check_routes(nodes: &Nodes, outgoing: &Edges, issues: &mut Issues) {
    for node in nodes.values() {
        let routes = outgoing
            .get(&node.id)
            .map(Vec::as_slice)
            .unwrap_or_default();
        let path = format!("nodes.{}.outgoing", node.id);
        match &node.kind {
            OrchestrationNodeKind::Output(_) => {
                if !routes.is_empty() {
                    issues.add(path, "terminal_has_route", "output nodes must be terminal");
                }
            }
            _ => {
                let has_success_route = routes
                    .iter()
                    .any(|edge| edge.condition == EdgeCondition::OnSuccess);
                if !has_success_route {
                    issues.add(
                        path,
                        "missing_success_route",
                        "non-terminal nodes require an on_success route",
                    );
                }
            }
        }
    }
}
