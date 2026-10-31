//! Which steps are reset when work is sent back: from the retry target down to the node that sent
//! it back.

use std::collections::BTreeMap;

use super::{
    graph::{ancestors, descendants, Edges, Nodes},
    Issues,
};
use crate::definition::{
    OrchestrationNode, OrchestrationNodeId, OrchestrationNodeKind, RetryReason,
};

/// The graph facts a span is computed from.
struct Graph<'a> {
    nodes: &'a Nodes,
    outgoing: &'a Edges,
    ranks: &'a BTreeMap<OrchestrationNodeId, usize>,
}

/// Where a node can send work back to, and the rules for that.
struct RetryLink<'a> {
    target_id: &'a OrchestrationNodeId,
    /// The config field that names the target, for issue paths.
    field: &'static str,
    /// Whether the target must list `verification_failed` in its `retry_on`.
    needs_policy: bool,
}

/// For every node that can send work back, the steps it resets, in topological order. Problems
/// with a retry target are reported and that node gets no span.
pub(super) fn compile_retry_spans(
    nodes: &Nodes,
    outgoing: &Edges,
    ranks: &BTreeMap<OrchestrationNodeId, usize>,
    issues: &mut Issues,
) -> BTreeMap<OrchestrationNodeId, Vec<OrchestrationNodeId>> {
    let graph = Graph {
        nodes,
        outgoing,
        ranks,
    };
    let mut spans = BTreeMap::new();
    for node in nodes.values() {
        let Some(link) = retry_link(node, nodes) else {
            continue;
        };
        let path = format!("nodes.{}.config.{}", node.id, link.field);
        if let Some(span) = span_for(node, &link, &path, &graph, issues) {
            spans.insert(node.id.clone(), span);
        }
    }
    spans
}

/// A verifier sends failed work back to its retry target; an approval sends the user's requested
/// changes back to its revise target; a task queue sends a plan revision back to the agent step
/// that wrote the plan.
fn retry_link<'a>(node: &'a OrchestrationNode, nodes: &Nodes) -> Option<RetryLink<'a>> {
    match &node.kind {
        OrchestrationNodeKind::Verify(config) => {
            config.retry_target.as_ref().map(|target_id| RetryLink {
                target_id,
                field: "retry_target",
                needs_policy: true,
            })
        }
        OrchestrationNodeKind::Approval(config) => {
            config.revise_target().map(|target_id| RetryLink {
                target_id,
                field: "revise_target",
                needs_policy: false,
            })
        }
        OrchestrationNodeKind::Agent(_) => {
            let target_id = node.task_plan_source()?;
            let writes_plan = nodes
                .get(target_id)
                .is_some_and(|plan| matches!(plan.kind, OrchestrationNodeKind::Agent(_)));
            writes_plan.then_some(RetryLink {
                target_id,
                field: "task_queue",
                needs_policy: false,
            })
        }
        _ => None,
    }
}

fn span_for(
    node: &OrchestrationNode,
    link: &RetryLink<'_>,
    path: &str,
    graph: &Graph<'_>,
    issues: &mut Issues,
) -> Option<Vec<OrchestrationNodeId>> {
    let target_id = link.target_id;
    let Some(target) = graph.nodes.get(target_id) else {
        issues.add(
            path,
            "unknown_retry_target",
            format!("retry target {target_id} does not exist"),
        );
        return None;
    };
    if !matches!(target.kind, OrchestrationNodeKind::Agent(_)) {
        issues.add(
            path,
            "invalid_retry_target",
            "retry targets must be agent nodes",
        );
        return None;
    }
    let upstream = ancestors(&node.id, graph.outgoing);
    if !upstream.contains(target_id) {
        issues.add(
            path,
            "invalid_retry_target",
            format!("retry target {target_id} must be upstream of {}", node.id),
        );
        return None;
    }
    if link.needs_policy
        && !target
            .retry
            .retry_on
            .contains(&RetryReason::VerificationFailed)
    {
        issues.add(
            path,
            "retry_target_policy",
            format!("retry target {target_id} must list verification_failed in retry_on"),
        );
        return None;
    }
    let mut span: Vec<_> = descendants(target_id, graph.outgoing)
        .intersection(&upstream)
        .cloned()
        .chain([target_id.clone(), node.id.clone()])
        .collect();
    span.sort_by_key(|id| graph.ranks.get(id).copied().unwrap_or(usize::MAX));
    Some(span)
}
