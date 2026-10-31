//! The one-step flow that a `Step` subflow target runs.

use serde_json::{json, Value};

use super::{
    AgentContextMode, AgentNodeConfig, ContextAccess, DefinitionStatus, EdgeCondition,
    InputBinding, InputNodeConfig, OrchestrationDefinition, OrchestrationDefinitionId,
    OrchestrationEdge, OrchestrationEdgeId, OrchestrationNode, OrchestrationNodeId,
    OrchestrationNodeKind, OrchestrationPolicies, OutputBinding, OutputContract, OutputNodeConfig,
    RetryPolicy, SchemaReference, StructuredOutputMode, SubflowTarget,
    ORCHESTRATION_SCHEMA_VERSION,
};

const AGENT: &str = "step";
const INPUT: &str = "input";
const OUTPUT: &str = "output";

impl SubflowTarget {
    /// The one-step flow a `Step` target runs, with `inputs` as the names of the values bound to
    /// the calling node. `None` for a `Flow` target.
    pub fn step_definition(
        &self,
        node: &OrchestrationNodeId,
        inputs: &[String],
    ) -> Option<OrchestrationDefinition> {
        let Self::Step { .. } = self else {
            return None;
        };
        Some(OrchestrationDefinition {
            schema_version: ORCHESTRATION_SCHEMA_VERSION,
            id: OrchestrationDefinitionId::new(format!("subflow.step.{node}")),
            revision: 1,
            name: format!("Step {node}"),
            description: None,
            status: DefinitionStatus::Published,
            input_schema: None,
            output_contract: OutputContract::default(),
            nodes: vec![input_node(), self.agent_node(inputs)?, output_node()],
            edges: vec![success_edge(INPUT, AGENT), success_edge(AGENT, OUTPUT)],
            policies: OrchestrationPolicies::default(),
            metadata: Value::Null,
        })
    }

    /// The agent that does the step, fed by the run's input values and answering in plain text.
    fn agent_node(&self, inputs: &[String]) -> Option<OrchestrationNode> {
        let Self::Step {
            instructions,
            profile,
            tools,
            model,
        } = self
        else {
            return None;
        };
        let config = AgentNodeConfig {
            instructions: instructions.clone(),
            task_queue: None,
            tools: tools.clone(),
            context_mode: AgentContextMode::IsolatedChild,
            model: model.clone(),
            structured_output: StructuredOutputMode::Text,
            profile: profile.clone(),
            context_access: ContextAccess::Own,
        };
        Some(OrchestrationNode {
            input_bindings: inputs.iter().map(|name| run_input_binding(name)).collect(),
            output_schema: Some(SchemaReference::Inline {
                name: "step".into(),
                schema: json!({"type": "string"}),
            }),
            ..plain_node(AGENT, "Step", OrchestrationNodeKind::Agent(config))
        })
    }
}

fn input_node() -> OrchestrationNode {
    plain_node(
        INPUT,
        "Input",
        OrchestrationNodeKind::Input(InputNodeConfig::default()),
    )
}

fn output_node() -> OrchestrationNode {
    plain_node(
        OUTPUT,
        "Output",
        OrchestrationNodeKind::Output(OutputNodeConfig {
            source: OutputBinding::NodeOutput {
                node_id: OrchestrationNodeId::from(AGENT),
                pointer: String::new(),
            },
            strict: false,
        }),
    )
}

fn plain_node(id: &str, name: &str, kind: OrchestrationNodeKind) -> OrchestrationNode {
    OrchestrationNode {
        id: OrchestrationNodeId::from(id),
        name: name.into(),
        kind,
        input_bindings: Vec::new(),
        output_schema: None,
        retry: RetryPolicy::default(),
        timeout_ms: None,
        metadata: Value::Null,
    }
}

/// Binds the input named `name` to the run input's field of the same name.
fn run_input_binding(name: &str) -> InputBinding {
    InputBinding {
        target: name.to_owned(),
        source: OutputBinding::RunInput {
            pointer: format!("/{name}"),
        },
    }
}

fn success_edge(source: &str, target: &str) -> OrchestrationEdge {
    OrchestrationEdge {
        id: OrchestrationEdgeId::new(format!("{source}-{target}")),
        source: OrchestrationNodeId::from(source),
        target: OrchestrationNodeId::from(target),
        condition: EdgeCondition::OnSuccess,
        metadata: Value::Null,
    }
}
