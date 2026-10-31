//! What each node may read: only outputs of nodes that must already have run (its ancestors), and
//! well-formed pointers into them. Also the checks on nodes whose configuration refers to other
//! nodes or to bindings.

use std::collections::BTreeSet;

use super::{
    graph::{ancestors, Edges, Nodes},
    pointers::{binding_pointer, check_pointer},
    Issues,
};
use crate::{
    definition::{
        AgentNodeConfig, ApprovalNodeConfig, OrchestrationDefinition, OrchestrationNode,
        OrchestrationNodeId, OrchestrationNodeKind, OutputBinding, StructuredOutputMode,
        SubflowTarget, TaskQueueConfig, VerificationCheck, VerifyNodeConfig, MAX_REVISIONS,
    },
    task_plan::{is_task_plan_schema, TASK_PLAN_SCHEMA_ID, TASK_PLAN_SCHEMA_REVISION},
};

type Upstream = BTreeSet<OrchestrationNodeId>;

/// Runs the data-flow checks for every node, then for the final output contract (which may read
/// any node upstream of every output node).
pub(super) fn check_data_flow(
    definition: &OrchestrationDefinition,
    nodes: &Nodes,
    outgoing: &Edges,
    issues: &mut Issues,
) {
    for node in nodes.values() {
        let upstream = ancestors(&node.id, outgoing);
        for (index, binding) in node.input_bindings.iter().enumerate() {
            let path = format!("nodes.{}.input_bindings[{index}].source", node.id);
            check_binding_source(&binding.source, &upstream, nodes, path, issues);
        }
        match &node.kind {
            OrchestrationNodeKind::Output(config) => {
                let path = format!("nodes.{}.config.source", node.id);
                check_pointer(
                    binding_pointer(&config.source),
                    &format!("{path}.pointer"),
                    issues,
                );
                check_binding_source(&config.source, &upstream, nodes, path, issues);
            }
            OrchestrationNodeKind::Verify(config) => check_verify_node(node, config, nodes, issues),
            OrchestrationNodeKind::Agent(AgentNodeConfig {
                task_queue: Some(queue),
                ..
            }) => check_task_queue(node, queue, nodes, issues),
            OrchestrationNodeKind::Subflow(config) => {
                let path = format!("nodes.{}.config.target", node.id);
                check_subflow_target(&config.target, path, issues);
            }
            OrchestrationNodeKind::Approval(config) => {
                check_approval_node(node, config, &upstream, nodes, issues);
            }
            _ => {}
        }
    }
    check_output_contract(definition, nodes, outgoing, issues);
}

fn check_binding_source(
    source: &OutputBinding,
    upstream: &Upstream,
    nodes: &Nodes,
    path: String,
    issues: &mut Issues,
) {
    let OutputBinding::NodeOutput { node_id, .. } = source else {
        return;
    };
    if !nodes.contains_key(node_id) {
        issues.add(
            path,
            "unknown_binding_source",
            format!("binding references unknown node {node_id}"),
        );
    } else if !upstream.contains(node_id) {
        issues.add(
            path,
            "unavailable_binding_source",
            format!("node {node_id} is not guaranteed to have run before this binding"),
        );
    }
}

fn check_verify_node(
    node: &OrchestrationNode,
    config: &VerifyNodeConfig,
    nodes: &Nodes,
    issues: &mut Issues,
) {
    let checks_path = format!("nodes.{}.config.checks", node.id);
    if config.checks.is_empty() {
        issues.add(
            &checks_path,
            "empty_verification",
            "verify nodes require at least one check",
        );
    }
    for check in &config.checks {
        check_verification(node, check, nodes, &checks_path, issues);
    }
}

fn check_verification(
    node: &OrchestrationNode,
    check: &VerificationCheck,
    nodes: &Nodes,
    checks_path: &str,
    issues: &mut Issues,
) {
    let check_path = format!("{checks_path}.{}", check.id());
    let pointer = match check {
        VerificationCheck::RequirementsSatisfied {
            plan_pointer,
            results_pointer,
        } => {
            check_pointer(plan_pointer, checks_path, issues);
            results_pointer
        }
        VerificationCheck::Schema => {
            check_schema_check_has_schema(node, nodes, checks_path, issues);
            return;
        }
        VerificationCheck::Criteria {
            pointer,
            plan_pointer,
            block_on,
            defer,
            ..
        } => {
            if let Some(plan_pointer) = plan_pointer {
                check_pointer(plan_pointer, &check_path, issues);
            }
            check_criteria_statuses(block_on, defer, &check_path, issues);
            pointer
        }
        VerificationCheck::RequiredStatus { pointer, .. }
        | VerificationCheck::ArtifactExists { pointer }
        | VerificationCheck::ArtifactsResolvable { pointer } => pointer,
    };
    check_pointer(pointer, &check_path, issues);
}

/// A schema check compares an output with its schema, so some bound node must have one.
fn check_schema_check_has_schema(
    node: &OrchestrationNode,
    nodes: &Nodes,
    checks_path: &str,
    issues: &mut Issues,
) {
    let has_schema = node.input_bindings.iter().any(|binding| {
        matches!(&binding.source, OutputBinding::NodeOutput { node_id, .. }
            if nodes.get(node_id).is_some_and(|source| source.output_schema.is_some()))
    });
    if !has_schema {
        issues.add(
            checks_path,
            "schema_check_without_schema",
            "schema check requires a binding to a node with an output schema",
        );
    }
}

fn check_criteria_statuses(block_on: &[String], defer: &[String], path: &str, issues: &mut Issues) {
    let names_blank_or_pass = block_on
        .iter()
        .chain(defer)
        .any(|status| status.trim().is_empty() || status == "pass");
    let overlap = block_on.iter().any(|status| defer.contains(status));
    if block_on.is_empty() || names_blank_or_pass || overlap {
        issues.add(
            path,
            "invalid_criteria_check",
            "criteria checks need at least one block_on status; block_on and defer must not overlap or name pass",
        );
    }
}

/// A task queue needs named flows to send tasks to, and a plan that one of its bindings supplies.
fn check_task_queue(
    node: &OrchestrationNode,
    queue: &TaskQueueConfig,
    nodes: &Nodes,
    issues: &mut Issues,
) {
    for (name, target) in &queue.flows {
        let path = format!("nodes.{}.config.task_queue.flows.{name}", node.id);
        if name.trim().is_empty() {
            issues.add(
                path,
                "invalid_subflow",
                "a flow needs a name for tasks to refer to",
            );
        } else {
            check_subflow_target(target, path, issues);
        }
    }
    check_plan_source(node, queue, nodes, issues);
}

fn check_plan_source(
    node: &OrchestrationNode,
    queue: &TaskQueueConfig,
    nodes: &Nodes,
    issues: &mut Issues,
) {
    let path = format!("nodes.{}.config.task_queue.plan_pointer", node.id);
    let binding = queue.plan_binding().and_then(|target| {
        node.input_bindings
            .iter()
            .find(|binding| binding.target == target)
    });
    let Some(binding) = binding else {
        issues.add(
            path,
            "task_plan_source",
            format!(
                "plan_pointer {} must start with the name of one of this step's input bindings",
                queue.plan_pointer
            ),
        );
        return;
    };
    // A plan bound whole from a step must be that step's typed task plan; anything else only
    // fails once the run reaches this step.
    let OutputBinding::NodeOutput { node_id, pointer } = &binding.source else {
        return;
    };
    if pointer.is_empty() && !produces_structured_plan(nodes, node_id) {
        issues.add(
            path,
            "task_plan_source",
            format!(
                "{node_id} must produce a task plan: give it a structured response format (e.g. host_validated) and the output schema {{\"type\": \"registry\", \"schema_id\": \"{TASK_PLAN_SCHEMA_ID}\", \"revision\": {TASK_PLAN_SCHEMA_REVISION}}}"
            ),
        );
    }
}

fn produces_structured_plan(nodes: &Nodes, producer_id: &OrchestrationNodeId) -> bool {
    nodes.get(producer_id).is_some_and(|producer| {
        matches!(&producer.kind, OrchestrationNodeKind::Agent(config) if config.structured_output != StructuredOutputMode::Text)
            && producer.output_schema.as_ref().is_some_and(is_task_plan_schema)
    })
}

/// A flow target names a saved flow; a step target carries its own instructions.
fn check_subflow_target(target: &SubflowTarget, path: String, issues: &mut Issues) {
    match target {
        SubflowTarget::Flow { id, .. } if id.as_str().trim().is_empty() => issues.add(
            path,
            "invalid_subflow",
            "a flow target needs the id of a saved flow",
        ),
        SubflowTarget::Step { instructions, .. } if instructions.trim().is_empty() => {
            issues.add(path, "invalid_subflow", "a step target needs instructions");
        }
        _ => {}
    }
}

fn check_approval_node(
    node: &OrchestrationNode,
    config: &ApprovalNodeConfig,
    upstream: &Upstream,
    nodes: &Nodes,
    issues: &mut Issues,
) {
    let subject_path = format!("nodes.{}.config.subject", node.id);
    check_pointer(
        binding_pointer(&config.subject),
        &format!("{subject_path}.pointer"),
        issues,
    );
    check_binding_source(&config.subject, upstream, nodes, subject_path, issues);
    if let Some(pointer) = &config.skip_if_empty {
        check_pointer(
            pointer,
            &format!("nodes.{}.config.skip_if_empty", node.id),
            issues,
        );
    }
    if config.max_revisions > MAX_REVISIONS {
        issues.add(
            format!("nodes.{}.config.max_revisions", node.id),
            "invalid_max_revisions",
            format!("max_revisions must be at most {MAX_REVISIONS}"),
        );
    }
}

/// The final output may read a node only if that node runs before every output node.
fn check_output_contract(
    definition: &OrchestrationDefinition,
    nodes: &Nodes,
    outgoing: &Edges,
    issues: &mut Issues,
) {
    let Some(source) = &definition.output_contract.source else {
        return;
    };
    check_pointer(
        binding_pointer(source),
        "output_contract.source.pointer",
        issues,
    );
    let OutputBinding::NodeOutput { node_id, .. } = source else {
        return;
    };
    let upstream_of_every_output = nodes
        .values()
        .filter(|node| matches!(node.kind, OrchestrationNodeKind::Output(_)))
        .all(|output| ancestors(&output.id, outgoing).contains(node_id));
    if !nodes.contains_key(node_id) || !upstream_of_every_output {
        issues.add(
            "output_contract.source",
            "unavailable_binding_source",
            format!("output contract source {node_id} must run before every output node"),
        );
    }
}
