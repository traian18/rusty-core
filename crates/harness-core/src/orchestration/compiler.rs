use std::collections::{BTreeMap, BTreeSet, VecDeque};

use super::definition::{
    AgentNodeConfig, EdgeCondition, OrchestrationDefinition, OrchestrationDefinitionId,
    OrchestrationEdge, OrchestrationNode, OrchestrationNodeId, OrchestrationNodeKind,
    OutputBinding, RetryReason, SchemaReference, StructuredOutputMode, SubflowTarget, ToolScope,
    VerificationCheck, MAX_REVISIONS, MAX_STEP_ATTEMPTS, ORCHESTRATION_SCHEMA_VERSION,
};
use super::task_plan::{is_task_plan_schema, TASK_PLAN_SCHEMA_ID, TASK_PLAN_SCHEMA_REVISION};

/// JSON Schema keywords the host validator implements. Anything else is
/// rejected at compile time rather than silently ignored at run time.
pub const SUPPORTED_SCHEMA_KEYWORDS: &[&str] = &[
    "type",
    "enum",
    "const",
    "required",
    "properties",
    "additionalProperties",
    "items",
    "minLength",
    "maxLength",
    "minimum",
    "maximum",
    "minItems",
    "maxItems",
    "default",
    "description",
    "title",
];

#[derive(Debug, Clone, PartialEq)]
pub struct CompiledOrchestration {
    pub definition_id: OrchestrationDefinitionId,
    pub revision: u64,
    pub entry_node: OrchestrationNodeId,
    pub nodes: BTreeMap<OrchestrationNodeId, OrchestrationNode>,
    pub outgoing: BTreeMap<OrchestrationNodeId, Vec<OrchestrationEdge>>,
    pub incoming: BTreeMap<OrchestrationNodeId, Vec<OrchestrationEdge>>,
    pub topological_rank: BTreeMap<OrchestrationNodeId, usize>,
    /// For each verify node with a `retry_target` and each approval with a
    /// revise target, the nodes reset to `Pending` when it sends work back
    /// upstream (target and sending node included), in topological order.
    pub retry_spans: BTreeMap<OrchestrationNodeId, Vec<OrchestrationNodeId>>,
    /// Stable FNV-1a hash of the serialized definition. Runs record it so a
    /// restore can prove it is resuming against the exact same document.
    pub content_hash: String,
    pub definition: OrchestrationDefinition,
}

impl CompiledOrchestration {
    pub fn node(&self, id: &OrchestrationNodeId) -> Option<&OrchestrationNode> {
        self.nodes.get(id)
    }

    pub fn outgoing_for(
        &self,
        id: &OrchestrationNodeId,
        condition: EdgeCondition,
    ) -> Option<&OrchestrationEdge> {
        self.outgoing
            .get(id)?
            .iter()
            .find(|edge| edge.condition == condition)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefinitionIssue {
    pub path: String,
    pub code: &'static str,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefinitionValidationError {
    pub issues: Vec<DefinitionIssue>,
}

impl std::fmt::Display for DefinitionValidationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "invalid orchestration definition")?;
        for issue in &self.issues {
            write!(formatter, "; {}: {}", issue.path, issue.message)?;
        }
        Ok(())
    }
}

impl std::error::Error for DefinitionValidationError {}

pub fn compile(
    definition: OrchestrationDefinition,
) -> Result<CompiledOrchestration, DefinitionValidationError> {
    let mut issues = Vec::new();

    if definition.schema_version != ORCHESTRATION_SCHEMA_VERSION {
        issue(
            &mut issues,
            "schema_version",
            "unsupported_schema_version",
            format!(
                "expected version {ORCHESTRATION_SCHEMA_VERSION}, got {}",
                definition.schema_version
            ),
        );
    }
    if definition.id.as_str().trim().is_empty() {
        issue(
            &mut issues,
            "id",
            "empty_id",
            "definition id cannot be empty",
        );
    }
    if definition.name.trim().is_empty() {
        issue(
            &mut issues,
            "name",
            "empty_name",
            "definition name cannot be empty",
        );
    }
    if definition
        .policies
        .max_steps
        .is_some_and(|limit| definition.nodes.len() > limit as usize)
    {
        issue(
            &mut issues,
            "nodes",
            "step_budget_exceeded",
            format!(
                "definition has {} nodes but max_steps is {}",
                definition.nodes.len(),
                definition.policies.max_steps.unwrap()
            ),
        );
    }
    if definition.policies.max_steps == Some(0) || definition.policies.max_total_attempts == Some(0)
    {
        issue(
            &mut issues,
            "policies",
            "invalid_budget",
            "step and attempt budgets must be greater than zero",
        );
    }
    validate_schema_reference(
        &definition.output_contract.schema,
        "output_contract.schema",
        &mut issues,
    );
    if let Some(schema) = &definition.input_schema {
        validate_schema_reference(schema, "input_schema", &mut issues);
    }

    let mut nodes = BTreeMap::new();
    for (index, node) in definition.nodes.iter().enumerate() {
        let path = format!("nodes[{index}]");
        if node.id.as_str().trim().is_empty() {
            issue(
                &mut issues,
                format!("{path}.id"),
                "empty_id",
                "node id cannot be empty",
            );
        }
        if node.name.trim().is_empty() {
            issue(
                &mut issues,
                format!("{path}.name"),
                "empty_name",
                "node name cannot be empty",
            );
        }
        if node.retry.max_attempts == 0 || node.retry.max_attempts > MAX_STEP_ATTEMPTS {
            issue(
                &mut issues,
                format!("{path}.retry.max_attempts"),
                "invalid_retry",
                format!("max_attempts must be between 1 and {MAX_STEP_ATTEMPTS}"),
            );
        }
        if let OrchestrationNodeKind::Agent(config) = &node.kind {
            if let Some(queue) = &config.task_queue {
                validate_pointer(
                    &queue.plan_pointer,
                    &format!("{path}.config.task_queue.plan_pointer"),
                    &mut issues,
                );
                if queue.max_repairs > 5 || queue.review_instructions.trim().is_empty() {
                    issue(
                        &mut issues,
                        format!("{path}.config.task_queue"),
                        "invalid_task_queue",
                        "task queues require reviewer instructions and at most five repairs",
                    );
                }
            }
            if config.instructions.trim().is_empty() {
                issue(
                    &mut issues,
                    format!("{path}.config.instructions"),
                    "empty_instructions",
                    "agent nodes require instructions",
                );
            }
            if let ToolScope::AllowList(tools) = &config.tools {
                if tools.iter().any(|tool| tool.trim().is_empty()) {
                    issue(
                        &mut issues,
                        format!("{path}.config.tools"),
                        "invalid_tool_reference",
                        "tool ids cannot be empty",
                    );
                }
            }
        }
        for (binding_index, binding) in node.input_bindings.iter().enumerate() {
            validate_pointer(
                binding_pointer(&binding.source),
                &format!("{path}.input_bindings[{binding_index}].source.pointer"),
                &mut issues,
            );
        }
        if matches!(&node.kind, OrchestrationNodeKind::Agent(config) if config.structured_output != StructuredOutputMode::Text)
            && node.output_schema.is_none()
        {
            issue(
                &mut issues,
                format!("{path}.output_schema"),
                "missing_output_schema",
                "agent nodes require an output schema",
            );
        }
        if let Some(schema) = &node.output_schema {
            validate_schema_reference(schema, &format!("{path}.output_schema"), &mut issues);
        }
        if nodes.insert(node.id.clone(), node.clone()).is_some() {
            issue(
                &mut issues,
                format!("{path}.id"),
                "duplicate_node_id",
                format!("duplicate node id {}", node.id),
            );
        }
    }

    let input_nodes: Vec<_> = definition
        .nodes
        .iter()
        .filter(|node| matches!(&node.kind, OrchestrationNodeKind::Input(_)))
        .collect();
    if input_nodes.len() != 1 {
        issue(
            &mut issues,
            "nodes",
            "input_count",
            format!(
                "expected exactly one input node, found {}",
                input_nodes.len()
            ),
        );
    }
    if !definition
        .nodes
        .iter()
        .any(|node| matches!(&node.kind, OrchestrationNodeKind::Output(_)))
    {
        issue(
            &mut issues,
            "nodes",
            "missing_output",
            "at least one output node is required",
        );
    }

    let mut outgoing: BTreeMap<_, Vec<_>> =
        nodes.keys().cloned().map(|id| (id, Vec::new())).collect();
    let mut incoming = outgoing.clone();
    let mut edge_ids = BTreeSet::new();
    let mut transition_keys = BTreeSet::new();
    for (index, edge) in definition.edges.iter().enumerate() {
        let path = format!("edges[{index}]");
        if !edge_ids.insert(edge.id.clone()) {
            issue(
                &mut issues,
                format!("{path}.id"),
                "duplicate_edge_id",
                format!("duplicate edge id {}", edge.id),
            );
        }
        if !nodes.contains_key(&edge.source) {
            issue(
                &mut issues,
                format!("{path}.source"),
                "dangling_edge",
                format!("source node {} does not exist", edge.source),
            );
            continue;
        }
        if !nodes.contains_key(&edge.target) {
            issue(
                &mut issues,
                format!("{path}.target"),
                "dangling_edge",
                format!("target node {} does not exist", edge.target),
            );
            continue;
        }
        if !transition_keys.insert((edge.source.clone(), edge.condition)) {
            issue(
                &mut issues,
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

    for node in nodes.values() {
        let routes = outgoing
            .get(&node.id)
            .map(Vec::as_slice)
            .unwrap_or_default();
        match &node.kind {
            OrchestrationNodeKind::Output(_) => {
                if !routes.is_empty() {
                    issue(
                        &mut issues,
                        format!("nodes.{}.outgoing", node.id),
                        "terminal_has_route",
                        "output nodes must be terminal",
                    );
                }
            }
            _ if !routes
                .iter()
                .any(|edge| edge.condition == EdgeCondition::OnSuccess) =>
            {
                issue(
                    &mut issues,
                    format!("nodes.{}.outgoing", node.id),
                    "missing_success_route",
                    "non-terminal nodes require an on_success route",
                )
            }
            _ => {}
        }
    }

    let entry_node = input_nodes.first().map(|node| node.id.clone());
    let topological_rank = entry_node
        .as_ref()
        .map(|entry| topological_sort(entry, &nodes, &outgoing, &mut issues))
        .unwrap_or_default();

    // Data-flow checks only make sense on an acyclic graph.
    let mut retry_spans = BTreeMap::new();
    if topological_rank.len() == nodes.len() {
        validate_data_flow(&definition, &nodes, &outgoing, &mut issues);
        retry_spans = compile_retry_spans(&nodes, &outgoing, &topological_rank, &mut issues);
    }

    if issues.is_empty() {
        let content_hash = crate::content_hash::content_hash(&definition);
        Ok(CompiledOrchestration {
            definition_id: definition.id.clone(),
            revision: definition.revision,
            entry_node: entry_node.expect("validated exactly one input node"),
            nodes,
            outgoing,
            incoming,
            topological_rank,
            retry_spans,
            content_hash,
            definition,
        })
    } else {
        Err(DefinitionValidationError { issues })
    }
}

fn topological_sort(
    entry: &OrchestrationNodeId,
    nodes: &BTreeMap<OrchestrationNodeId, OrchestrationNode>,
    outgoing: &BTreeMap<OrchestrationNodeId, Vec<OrchestrationEdge>>,
    issues: &mut Vec<DefinitionIssue>,
) -> BTreeMap<OrchestrationNodeId, usize> {
    let mut indegree: BTreeMap<_, usize> = nodes.keys().cloned().map(|id| (id, 0)).collect();
    for edges in outgoing.values() {
        for edge in edges {
            *indegree.entry(edge.target.clone()).or_default() += 1;
        }
    }
    let mut queue: VecDeque<_> = indegree
        .iter()
        .filter_map(|(id, degree)| (*degree == 0).then_some(id.clone()))
        .collect();
    let mut ranks = BTreeMap::new();
    while let Some(node_id) = queue.pop_front() {
        let rank = ranks.len();
        ranks.insert(node_id.clone(), rank);
        if let Some(edges) = outgoing.get(&node_id) {
            for edge in edges {
                let degree = indegree
                    .get_mut(&edge.target)
                    .expect("validated target must exist");
                *degree -= 1;
                if *degree == 0 {
                    queue.push_back(edge.target.clone());
                }
            }
        }
    }
    if ranks.len() != nodes.len() {
        issue(
            issues,
            "edges",
            "cycle",
            "cycles are not supported in schema version 1",
        );
    }

    let mut reachable = BTreeSet::from([entry.clone()]);
    let mut queue = VecDeque::from([entry.clone()]);
    while let Some(node_id) = queue.pop_front() {
        if let Some(edges) = outgoing.get(&node_id) {
            for edge in edges {
                if reachable.insert(edge.target.clone()) {
                    queue.push_back(edge.target.clone());
                }
            }
        }
    }
    for node_id in nodes.keys() {
        if !reachable.contains(node_id) {
            issue(
                issues,
                format!("nodes.{node_id}"),
                "unreachable_node",
                "node is not reachable from the input node",
            );
        }
    }
    ranks
}

/// Nodes from which `node` is reachable (its upstream), excluding itself.
fn ancestors(
    node: &OrchestrationNodeId,
    outgoing: &BTreeMap<OrchestrationNodeId, Vec<OrchestrationEdge>>,
) -> BTreeSet<OrchestrationNodeId> {
    let mut incoming: BTreeMap<&OrchestrationNodeId, Vec<&OrchestrationNodeId>> = BTreeMap::new();
    for edges in outgoing.values() {
        for edge in edges {
            incoming.entry(&edge.target).or_default().push(&edge.source);
        }
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

/// Nodes reachable from `node` (its downstream), excluding itself.
fn descendants(
    node: &OrchestrationNodeId,
    outgoing: &BTreeMap<OrchestrationNodeId, Vec<OrchestrationEdge>>,
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

fn binding_pointer(binding: &OutputBinding) -> &str {
    match binding {
        OutputBinding::RunInput { pointer } | OutputBinding::NodeOutput { pointer, .. } => pointer,
    }
}

fn validate_pointer(pointer: &str, path: &str, issues: &mut Vec<DefinitionIssue>) {
    if !pointer.is_empty() && !pointer.starts_with('/') {
        issue(
            issues,
            path,
            "invalid_pointer",
            "bindings use JSON Pointer syntax: empty or starting with '/'",
        );
    }
}

/// A node may only read outputs of nodes that must already have run, i.e.
/// its ancestors. The final output contract may read any non-output node
/// upstream of an output node.
fn validate_data_flow(
    definition: &OrchestrationDefinition,
    nodes: &BTreeMap<OrchestrationNodeId, OrchestrationNode>,
    outgoing: &BTreeMap<OrchestrationNodeId, Vec<OrchestrationEdge>>,
    issues: &mut Vec<DefinitionIssue>,
) {
    let check = |source: &OutputBinding,
                 upstream: &BTreeSet<OrchestrationNodeId>,
                 path: String,
                 issues: &mut Vec<DefinitionIssue>| {
        if let OutputBinding::NodeOutput { node_id, .. } = source {
            if !nodes.contains_key(node_id) {
                issue(
                    issues,
                    path,
                    "unknown_binding_source",
                    format!("binding references unknown node {node_id}"),
                );
            } else if !upstream.contains(node_id) {
                issue(
                    issues,
                    path,
                    "unavailable_binding_source",
                    format!("node {node_id} is not guaranteed to have run before this binding"),
                );
            }
        }
    };

    for node in nodes.values() {
        let upstream = ancestors(&node.id, outgoing);
        for (index, binding) in node.input_bindings.iter().enumerate() {
            check(
                &binding.source,
                &upstream,
                format!("nodes.{}.input_bindings[{index}].source", node.id),
                issues,
            );
        }
        match &node.kind {
            OrchestrationNodeKind::Output(config) => {
                validate_pointer(
                    binding_pointer(&config.source),
                    &format!("nodes.{}.config.source.pointer", node.id),
                    issues,
                );
                check(
                    &config.source,
                    &upstream,
                    format!("nodes.{}.config.source", node.id),
                    issues,
                );
            }
            OrchestrationNodeKind::Verify(config) => {
                if config.checks.is_empty() {
                    issue(
                        issues,
                        format!("nodes.{}.config.checks", node.id),
                        "empty_verification",
                        "verify nodes require at least one check",
                    );
                }
                for check in &config.checks {
                    let pointer = match check {
                        VerificationCheck::RequirementsSatisfied {
                            plan_pointer,
                            results_pointer,
                        } => {
                            validate_pointer(
                                plan_pointer,
                                &format!("nodes.{}.config.checks", node.id),
                                issues,
                            );
                            results_pointer
                        }
                        VerificationCheck::Schema => {
                            let has_schema = node.input_bindings.iter().any(|binding| {
                                matches!(&binding.source, OutputBinding::NodeOutput { node_id, .. }
                                    if nodes.get(node_id).is_some_and(|source| source.output_schema.is_some()))
                            });
                            if !has_schema {
                                issue(
                                    issues,
                                    format!("nodes.{}.config.checks", node.id),
                                    "schema_check_without_schema",
                                    "schema check requires a binding to a node with an output schema",
                                );
                            }
                            continue;
                        }
                        VerificationCheck::Criteria {
                            pointer,
                            plan_pointer,
                            block_on,
                            defer,
                            ..
                        } => {
                            if let Some(plan_pointer) = plan_pointer {
                                validate_pointer(
                                    plan_pointer,
                                    &format!("nodes.{}.config.checks.{}", node.id, check.id()),
                                    issues,
                                );
                            }
                            if block_on.is_empty()
                                || block_on
                                    .iter()
                                    .chain(defer)
                                    .any(|status| status.trim().is_empty() || status == "pass")
                                || block_on.iter().any(|status| defer.contains(status))
                            {
                                issue(
                                    issues,
                                    format!("nodes.{}.config.checks.{}", node.id, check.id()),
                                    "invalid_criteria_check",
                                    "criteria checks need at least one block_on status; block_on and defer must not overlap or name pass",
                                );
                            }
                            pointer
                        }
                        VerificationCheck::RequiredStatus { pointer, .. }
                        | VerificationCheck::ArtifactExists { pointer }
                        | VerificationCheck::ArtifactsResolvable { pointer } => pointer,
                    };
                    validate_pointer(
                        pointer,
                        &format!("nodes.{}.config.checks.{}", node.id, check.id()),
                        issues,
                    );
                }
            }
            OrchestrationNodeKind::Agent(AgentNodeConfig {
                task_queue: Some(queue),
                ..
            }) => {
                for (name, target) in &queue.flows {
                    let path = format!("nodes.{}.config.task_queue.flows.{name}", node.id);
                    match target {
                        _ if name.trim().is_empty() => issue(
                            issues,
                            path,
                            "invalid_subflow",
                            "a flow needs a name for tasks to refer to",
                        ),
                        SubflowTarget::Flow { id, .. } if id.as_str().trim().is_empty() => issue(
                            issues,
                            path,
                            "invalid_subflow",
                            "a flow target needs the id of a saved flow",
                        ),
                        SubflowTarget::Step { instructions, .. }
                            if instructions.trim().is_empty() =>
                        {
                            issue(
                                issues,
                                path,
                                "invalid_subflow",
                                "a step target needs instructions",
                            )
                        }
                        _ => {}
                    }
                }
                let path = format!("nodes.{}.config.task_queue.plan_pointer", node.id);
                let binding = queue.plan_binding().and_then(|target| {
                    node.input_bindings
                        .iter()
                        .find(|binding| binding.target == target)
                });
                let Some(binding) = binding else {
                    issue(
                        issues,
                        path,
                        "task_plan_source",
                        format!(
                            "plan_pointer {} must start with the name of one of this step's input bindings",
                            queue.plan_pointer
                        ),
                    );
                    continue;
                };
                // A plan bound whole from a step must be that step's typed
                // task plan; anything else only fails once the run reaches
                // this step.
                if let OutputBinding::NodeOutput { node_id, pointer } = &binding.source {
                    let structured_plan = nodes.get(node_id).is_some_and(|producer| {
                        matches!(&producer.kind, OrchestrationNodeKind::Agent(config) if config.structured_output != StructuredOutputMode::Text)
                            && producer.output_schema.as_ref().is_some_and(is_task_plan_schema)
                    });
                    if pointer.is_empty() && !structured_plan {
                        issue(
                            issues,
                            path,
                            "task_plan_source",
                            format!(
                                "{node_id} must produce a task plan: give it a structured response format (e.g. host_validated) and the output schema {{\"type\": \"registry\", \"schema_id\": \"{TASK_PLAN_SCHEMA_ID}\", \"revision\": {TASK_PLAN_SCHEMA_REVISION}}}"
                            ),
                        );
                    }
                }
            }
            OrchestrationNodeKind::Subflow(config) => {
                let path = format!("nodes.{}.config.target", node.id);
                match &config.target {
                    SubflowTarget::Flow { id, .. } if id.as_str().trim().is_empty() => issue(
                        issues,
                        path,
                        "invalid_subflow",
                        "a flow target needs the id of a saved flow",
                    ),
                    SubflowTarget::Step { instructions, .. } if instructions.trim().is_empty() => {
                        issue(
                            issues,
                            path,
                            "invalid_subflow",
                            "a step target needs instructions",
                        )
                    }
                    _ => {}
                }
            }
            OrchestrationNodeKind::Approval(config) => {
                validate_pointer(
                    binding_pointer(&config.subject),
                    &format!("nodes.{}.config.subject.pointer", node.id),
                    issues,
                );
                check(
                    &config.subject,
                    &upstream,
                    format!("nodes.{}.config.subject", node.id),
                    issues,
                );
                if let Some(pointer) = &config.skip_if_empty {
                    validate_pointer(
                        pointer,
                        &format!("nodes.{}.config.skip_if_empty", node.id),
                        issues,
                    );
                }
                if config.max_revisions > MAX_REVISIONS {
                    issue(
                        issues,
                        format!("nodes.{}.config.max_revisions", node.id),
                        "invalid_max_revisions",
                        format!("max_revisions must be at most {MAX_REVISIONS}"),
                    );
                }
            }
            _ => {}
        }
    }

    let Some(source) = &definition.output_contract.source else {
        return;
    };
    validate_pointer(
        binding_pointer(source),
        "output_contract.source.pointer",
        issues,
    );
    if let OutputBinding::NodeOutput { node_id, .. } = source {
        let upstream_of_every_output = nodes
            .values()
            .filter(|node| matches!(node.kind, OrchestrationNodeKind::Output(_)))
            .all(|output| ancestors(&output.id, outgoing).contains(node_id));
        if !nodes.contains_key(node_id) || !upstream_of_every_output {
            issue(
                issues,
                "output_contract.source",
                "unavailable_binding_source",
                format!("output contract source {node_id} must run before every output node"),
            );
        }
    }
}

fn compile_retry_spans(
    nodes: &BTreeMap<OrchestrationNodeId, OrchestrationNode>,
    outgoing: &BTreeMap<OrchestrationNodeId, Vec<OrchestrationEdge>>,
    ranks: &BTreeMap<OrchestrationNodeId, usize>,
    issues: &mut Vec<DefinitionIssue>,
) -> BTreeMap<OrchestrationNodeId, Vec<OrchestrationNodeId>> {
    let mut spans = BTreeMap::new();
    for node in nodes.values() {
        // A verifier sends failed work back to its retry target; an approval
        // sends the user's requested changes back to its revise target.
        let (target_id, path, needs_policy) = match &node.kind {
            OrchestrationNodeKind::Verify(config) => match &config.retry_target {
                Some(target) => (target, "retry_target", true),
                None => continue,
            },
            OrchestrationNodeKind::Approval(config) => match config.revise_target() {
                Some(target) => (target, "revise_target", false),
                None => continue,
            },
            // A task queue can send the user's request to revise the plan
            // back to the step that wrote it -- when that is an agent step.
            OrchestrationNodeKind::Agent(_) => match node.task_plan_source() {
                Some(target)
                    if nodes.get(target).is_some_and(|plan| {
                        matches!(plan.kind, OrchestrationNodeKind::Agent(_))
                    }) =>
                {
                    (target, "task_queue", false)
                }
                _ => continue,
            },
            _ => continue,
        };
        let path = format!("nodes.{}.config.{path}", node.id);
        let Some(target) = nodes.get(target_id) else {
            issue(
                issues,
                path,
                "unknown_retry_target",
                format!("retry target {target_id} does not exist"),
            );
            continue;
        };
        if !matches!(target.kind, OrchestrationNodeKind::Agent(_)) {
            issue(
                issues,
                path,
                "invalid_retry_target",
                "retry targets must be agent nodes",
            );
            continue;
        }
        let upstream = ancestors(&node.id, outgoing);
        if !upstream.contains(target_id) {
            issue(
                issues,
                path,
                "invalid_retry_target",
                format!("retry target {target_id} must be upstream of {}", node.id),
            );
            continue;
        }
        if needs_policy
            && !target
                .retry
                .retry_on
                .contains(&RetryReason::VerificationFailed)
        {
            issue(
                issues,
                path,
                "retry_target_policy",
                format!("retry target {target_id} must list verification_failed in retry_on"),
            );
            continue;
        }
        let downstream = descendants(target_id, outgoing);
        let mut span: Vec<_> = downstream
            .intersection(&upstream)
            .cloned()
            .chain([target_id.clone(), node.id.clone()])
            .collect();
        span.sort_by_key(|id| ranks.get(id).copied().unwrap_or(usize::MAX));
        spans.insert(node.id.clone(), span);
    }
    spans
}

fn validate_schema_reference(
    reference: &SchemaReference,
    path: &str,
    issues: &mut Vec<DefinitionIssue>,
) {
    match reference {
        SchemaReference::Inline { name, schema } => {
            if name.trim().is_empty() {
                issue(
                    issues,
                    path,
                    "empty_schema_name",
                    "schema name cannot be empty",
                );
            }
            validate_schema_value(schema, path, issues);
        }
        SchemaReference::Registry {
            schema_id,
            revision,
        } => {
            if schema_id.trim().is_empty() || *revision == 0 {
                issue(
                    issues,
                    path,
                    "invalid_schema_reference",
                    "registry schema requires a non-empty id and positive revision",
                );
            }
        }
    }
}

fn validate_schema_value(
    schema: &serde_json::Value,
    path: &str,
    issues: &mut Vec<DefinitionIssue>,
) {
    let Some(object) = schema.as_object() else {
        issue(
            issues,
            path,
            "invalid_schema",
            "inline schema must be a JSON object",
        );
        return;
    };
    for keyword in object.keys() {
        if !SUPPORTED_SCHEMA_KEYWORDS.contains(&keyword.as_str()) {
            issue(
                issues,
                format!("{path}.{keyword}"),
                "unsupported_schema_keyword",
                format!("schema keyword {keyword} is not supported by the host validator"),
            );
        }
    }
    if let Some(schema_type) = object.get("type") {
        let valid = schema_type.as_str().is_some_and(|value| {
            matches!(
                value,
                "null" | "boolean" | "object" | "array" | "number" | "integer" | "string"
            )
        });
        if !valid {
            issue(
                issues,
                format!("{path}.type"),
                "invalid_schema",
                "type must be a supported JSON Schema primitive",
            );
        }
    }
    if let Some(required) = object.get("required") {
        let valid = required
            .as_array()
            .is_some_and(|values| values.iter().all(|value| value.is_string()));
        if !valid {
            issue(
                issues,
                format!("{path}.required"),
                "invalid_schema",
                "required must be an array of strings",
            );
        }
    }
    if let Some(properties) = object.get("properties") {
        if let Some(properties) = properties.as_object() {
            for (name, child) in properties {
                validate_schema_value(child, &format!("{path}.properties.{name}"), issues);
            }
        } else {
            issue(
                issues,
                format!("{path}.properties"),
                "invalid_schema",
                "properties must be an object",
            );
        }
    }
    if let Some(items) = object.get("items") {
        validate_schema_value(items, &format!("{path}.items"), issues);
    }
    if let Some(additional) = object.get("additionalProperties") {
        if !additional.is_boolean() && !additional.is_object() {
            issue(
                issues,
                format!("{path}.additionalProperties"),
                "invalid_schema",
                "additionalProperties must be a boolean or schema object",
            );
        }
    }
    if let Some(values) = object.get("enum") {
        if !values.as_array().is_some_and(|values| !values.is_empty()) {
            issue(
                issues,
                format!("{path}.enum"),
                "invalid_schema",
                "enum must be a non-empty array",
            );
        }
    }
}

fn issue(
    issues: &mut Vec<DefinitionIssue>,
    path: impl Into<String>,
    code: &'static str,
    message: impl Into<String>,
) {
    issues.push(DefinitionIssue {
        path: path.into(),
        code,
        message: message.into(),
    });
}
