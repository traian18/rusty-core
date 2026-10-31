//! Turning a workflow definition into a [`CompiledOrchestration`], or reporting everything wrong
//! with it.
//!
//! [`compile`] runs the checks in order and collects every issue instead of stopping at the first,
//! so an editor can show them all. The checks live in `definition_checks` (the document and its
//! nodes), `edge_checks`, `graph` (ordering and reachability), `data_flow` (what each node may
//! read), `retry_spans` and `schema_checks`.

mod data_flow;
mod definition_checks;
mod edge_checks;
mod graph;
mod pointers;
mod retry_spans;
mod schema_checks;

use std::collections::BTreeMap;

use super::definition::{
    EdgeCondition, OrchestrationDefinition, OrchestrationDefinitionId, OrchestrationEdge,
    OrchestrationNode, OrchestrationNodeId,
};

/// JSON Schema keywords the host validator implements. Anything else is rejected at compile time
/// rather than silently ignored at run time.
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

/// A validated definition with the lookups the reducer needs. Immutable once built.
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

/// One problem in a definition, located by a path such as `nodes[2].config`.
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

/// The issues found so far.
#[derive(Default)]
pub(super) struct Issues(Vec<DefinitionIssue>);

impl Issues {
    fn add(&mut self, path: impl Into<String>, code: &'static str, message: impl Into<String>) {
        self.0.push(DefinitionIssue {
            path: path.into(),
            code,
            message: message.into(),
        });
    }
}

/// Validates a definition and builds the lookups the reducer uses, reporting every issue found.
pub fn compile(
    definition: OrchestrationDefinition,
) -> Result<CompiledOrchestration, DefinitionValidationError> {
    let mut issues = Issues::default();
    definition_checks::check_header(&definition, &mut issues);
    let nodes = definition_checks::check_nodes(&definition, &mut issues);
    definition_checks::check_context_access(&definition, &nodes, &mut issues);
    let entry_node = definition_checks::check_entry_and_exit(&definition, &mut issues);
    let (outgoing, incoming) = edge_checks::build_edges(&definition, &nodes, &mut issues);
    edge_checks::check_routes(&nodes, &outgoing, &mut issues);
    let topological_rank = match &entry_node {
        Some(entry) => graph::rank_and_check_reachability(entry, &nodes, &outgoing, &mut issues),
        None => BTreeMap::new(),
    };

    // Data-flow checks only make sense on an acyclic graph.
    let acyclic = topological_rank.len() == nodes.len();
    let retry_spans = if acyclic {
        data_flow::check_data_flow(&definition, &nodes, &outgoing, &mut issues);
        retry_spans::compile_retry_spans(&nodes, &outgoing, &topological_rank, &mut issues)
    } else {
        BTreeMap::new()
    };

    match entry_node.filter(|_| issues.0.is_empty()) {
        Some(entry_node) => Ok(CompiledOrchestration {
            definition_id: definition.id.clone(),
            revision: definition.revision,
            entry_node,
            nodes,
            outgoing,
            incoming,
            topological_rank,
            retry_spans,
            content_hash: rusty_agent_core::content_hash::content_hash(&definition),
            definition,
        }),
        None => Err(DefinitionValidationError { issues: issues.0 }),
    }
}
