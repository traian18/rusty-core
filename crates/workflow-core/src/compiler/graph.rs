//! The shape of the graph: ordering, reachability, and who is upstream of whom.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use super::Issues;
use crate::definition::{OrchestrationEdge, OrchestrationNode, OrchestrationNodeId};

pub(super) type Nodes = BTreeMap<OrchestrationNodeId, OrchestrationNode>;
pub(super) type Edges = BTreeMap<OrchestrationNodeId, Vec<OrchestrationEdge>>;

/// Ranks the nodes in topological order (reporting a cycle) and reports nodes the input node
/// cannot reach. With a cycle the ranking is partial, which callers use to skip data-flow checks.
pub(super) fn rank_and_check_reachability(
    entry: &OrchestrationNodeId,
    nodes: &Nodes,
    outgoing: &Edges,
    issues: &mut Issues,
) -> BTreeMap<OrchestrationNodeId, usize> {
    let ranks = topological_ranks(nodes, outgoing);
    if ranks.len() != nodes.len() {
        issues.add(
            "edges",
            "cycle",
            "cycles are not supported in schema version 1",
        );
    }
    let reachable = descendants_or_self(entry, outgoing);
    for node_id in nodes.keys().filter(|id| !reachable.contains(id)) {
        issues.add(
            format!("nodes.{node_id}"),
            "unreachable_node",
            "node is not reachable from the input node",
        );
    }
    ranks
}

/// Kahn's algorithm: nodes come out in an order where every edge points forward.
fn topological_ranks(nodes: &Nodes, outgoing: &Edges) -> BTreeMap<OrchestrationNodeId, usize> {
    let mut indegree: BTreeMap<_, usize> = nodes.keys().cloned().map(|id| (id, 0)).collect();
    for edge in outgoing.values().flatten() {
        *indegree.entry(edge.target.clone()).or_default() += 1;
    }
    let mut queue: VecDeque<_> = indegree
        .iter()
        .filter_map(|(id, degree)| (*degree == 0).then_some(id.clone()))
        .collect();
    let mut ranks = BTreeMap::new();
    while let Some(node_id) = queue.pop_front() {
        let rank = ranks.len();
        ranks.insert(node_id.clone(), rank);
        for edge in outgoing.get(&node_id).into_iter().flatten() {
            let Some(degree) = indegree.get_mut(&edge.target) else {
                continue;
            };
            *degree -= 1;
            if *degree == 0 {
                queue.push_back(edge.target.clone());
            }
        }
    }
    ranks
}

fn descendants_or_self(
    node: &OrchestrationNodeId,
    outgoing: &Edges,
) -> BTreeSet<OrchestrationNodeId> {
    let mut seen = descendants(node, outgoing);
    seen.insert(node.clone());
    seen
}

/// Nodes from which `node` is reachable (its upstream), excluding itself.
pub(super) fn ancestors(
    node: &OrchestrationNodeId,
    outgoing: &Edges,
) -> BTreeSet<OrchestrationNodeId> {
    let mut incoming: BTreeMap<&OrchestrationNodeId, Vec<&OrchestrationNodeId>> = BTreeMap::new();
    for edge in outgoing.values().flatten() {
        incoming.entry(&edge.target).or_default().push(&edge.source);
    }
    let mut seen = BTreeSet::new();
    let mut queue = VecDeque::from([node]);
    while let Some(current) = queue.pop_front() {
        for source in incoming.get(current).into_iter().flatten() {
            if seen.insert((*source).clone()) {
                queue.push_back(source);
            }
        }
    }
    seen
}

/// Nodes reachable from `node` (its downstream), excluding itself unless it is on a cycle.
pub(super) fn descendants(
    node: &OrchestrationNodeId,
    outgoing: &Edges,
) -> BTreeSet<OrchestrationNodeId> {
    let mut seen = BTreeSet::new();
    let mut queue = VecDeque::from([node.clone()]);
    while let Some(current) = queue.pop_front() {
        for edge in outgoing.get(&current).into_iter().flatten() {
            if seen.insert(edge.target.clone()) {
                queue.push_back(edge.target.clone());
            }
        }
    }
    seen
}
