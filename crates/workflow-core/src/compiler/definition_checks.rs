//! Checks on the definition document itself and on each node taken alone.

use std::collections::BTreeMap;

use super::{
    graph::Nodes,
    pointers::{binding_pointer, check_pointer},
    schema_checks::check_schema_reference,
    Issues,
};
use crate::definition::{
    AgentNodeConfig, ContextAccess, OrchestrationDefinition, OrchestrationNode,
    OrchestrationNodeId, OrchestrationNodeKind, StructuredOutputMode, ToolScope, MAX_STEP_ATTEMPTS,
    ORCHESTRATION_SCHEMA_VERSION,
};

/// The document-level checks: version, identity, budgets, and the input and output schemas.
pub(super) fn check_header(definition: &OrchestrationDefinition, issues: &mut Issues) {
    if definition.schema_version != ORCHESTRATION_SCHEMA_VERSION {
        issues.add(
            "schema_version",
            "unsupported_schema_version",
            format!(
                "expected version {ORCHESTRATION_SCHEMA_VERSION}, got {}",
                definition.schema_version
            ),
        );
    }
    if definition.id.as_str().trim().is_empty() {
        issues.add("id", "empty_id", "definition id cannot be empty");
    }
    if definition.name.trim().is_empty() {
        issues.add("name", "empty_name", "definition name cannot be empty");
    }
    check_budgets(definition, issues);
    check_schema_reference(
        &definition.output_contract.schema,
        "output_contract.schema",
        issues,
    );
    if let Some(schema) = &definition.input_schema {
        check_schema_reference(schema, "input_schema", issues);
    }
}

fn check_budgets(definition: &OrchestrationDefinition, issues: &mut Issues) {
    let policies = &definition.policies;
    if let Some(limit) = policies
        .max_steps
        .filter(|&limit| definition.nodes.len() > limit as usize)
    {
        issues.add(
            "nodes",
            "step_budget_exceeded",
            format!(
                "definition has {} nodes but max_steps is {limit}",
                definition.nodes.len()
            ),
        );
    }
    if policies.max_steps == Some(0) || policies.max_total_attempts == Some(0) {
        issues.add(
            "policies",
            "invalid_budget",
            "step and attempt budgets must be greater than zero",
        );
    }
}

/// Checks every node and returns them by id. A repeated id is reported and the later node wins.
pub(super) fn check_nodes(definition: &OrchestrationDefinition, issues: &mut Issues) -> Nodes {
    let mut nodes = BTreeMap::new();
    for (index, node) in definition.nodes.iter().enumerate() {
        let path = format!("nodes[{index}]");
        check_node(node, &path, issues);
        if nodes.insert(node.id.clone(), node.clone()).is_some() {
            issues.add(
                format!("{path}.id"),
                "duplicate_node_id",
                format!("duplicate node id {}", node.id),
            );
        }
    }
    nodes
}

fn check_node(node: &OrchestrationNode, path: &str, issues: &mut Issues) {
    if node.id.as_str().trim().is_empty() {
        issues.add(format!("{path}.id"), "empty_id", "node id cannot be empty");
    }
    if node.name.trim().is_empty() {
        issues.add(
            format!("{path}.name"),
            "empty_name",
            "node name cannot be empty",
        );
    }
    if node.retry.max_attempts == 0 || node.retry.max_attempts > MAX_STEP_ATTEMPTS {
        issues.add(
            format!("{path}.retry.max_attempts"),
            "invalid_retry",
            format!("max_attempts must be between 1 and {MAX_STEP_ATTEMPTS}"),
        );
    }
    if let OrchestrationNodeKind::Agent(config) = &node.kind {
        check_agent_config(config, path, issues);
    }
    for (index, binding) in node.input_bindings.iter().enumerate() {
        check_pointer(
            binding_pointer(&binding.source),
            &format!("{path}.input_bindings[{index}].source.pointer"),
            issues,
        );
    }
    check_output_schema(node, path, issues);
}

fn check_agent_config(config: &AgentNodeConfig, path: &str, issues: &mut Issues) {
    if let Some(queue) = &config.task_queue {
        check_pointer(
            &queue.plan_pointer,
            &format!("{path}.config.task_queue.plan_pointer"),
            issues,
        );
        if queue.max_repairs > 5 || queue.review_instructions.trim().is_empty() {
            issues.add(
                format!("{path}.config.task_queue"),
                "invalid_task_queue",
                "task queues require reviewer instructions and at most five repairs",
            );
        }
    }
    if config.instructions.trim().is_empty() {
        issues.add(
            format!("{path}.config.instructions"),
            "empty_instructions",
            "agent nodes require instructions",
        );
    }
    if let ToolScope::AllowList(tools) = &config.tools {
        if tools.iter().any(|tool| tool.trim().is_empty()) {
            issues.add(
                format!("{path}.config.tools"),
                "invalid_tool_reference",
                "tool ids cannot be empty",
            );
        }
    }
}

/// An agent that answers in structure needs a schema to answer by.
fn check_output_schema(node: &OrchestrationNode, path: &str, issues: &mut Issues) {
    let answers_in_structure = matches!(
        &node.kind,
        OrchestrationNodeKind::Agent(config) if config.structured_output != StructuredOutputMode::Text
    );
    if answers_in_structure && node.output_schema.is_none() {
        issues.add(
            format!("{path}.output_schema"),
            "missing_output_schema",
            "agent nodes require an output schema",
        );
    }
    if let Some(schema) = &node.output_schema {
        check_schema_reference(schema, &format!("{path}.output_schema"), issues);
    }
}

/// An agent may read the work of other steps of the same flow, and only those.
pub(super) fn check_context_access(
    definition: &OrchestrationDefinition,
    nodes: &Nodes,
    issues: &mut Issues,
) {
    for (index, node) in definition.nodes.iter().enumerate() {
        let OrchestrationNodeKind::Agent(AgentNodeConfig {
            context_access: ContextAccess::Steps(steps),
            ..
        }) = &node.kind
        else {
            continue;
        };
        for step in steps {
            if step == &node.id || !nodes.contains_key(step) {
                issues.add(
                    format!("nodes[{index}].config.context_access"),
                    "invalid_context_access",
                    format!("context_access names {step}, which is not another step of this flow"),
                );
            }
        }
    }
}

/// A flow has exactly one input node, which it starts from, and at least one output node. Returns
/// the input node.
pub(super) fn check_entry_and_exit(
    definition: &OrchestrationDefinition,
    issues: &mut Issues,
) -> Option<OrchestrationNodeId> {
    let input_nodes: Vec<_> = definition
        .nodes
        .iter()
        .filter(|node| matches!(&node.kind, OrchestrationNodeKind::Input(_)))
        .collect();
    if input_nodes.len() != 1 {
        issues.add(
            "nodes",
            "input_count",
            format!(
                "expected exactly one input node, found {}",
                input_nodes.len()
            ),
        );
    }
    let has_output = definition
        .nodes
        .iter()
        .any(|node| matches!(&node.kind, OrchestrationNodeKind::Output(_)));
    if !has_output {
        issues.add(
            "nodes",
            "missing_output",
            "at least one output node is required",
        );
    }
    input_nodes.first().map(|node| node.id.clone())
}
