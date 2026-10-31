//! The built-in workflow: Input, Agent, Verify, Output.

use serde_json::{json, Value};

use super::definition::{
    AgentContextMode, AgentNodeConfig, DefinitionStatus, EdgeCondition, InputBinding,
    InputNodeConfig, OrchestrationDefinition, OrchestrationDefinitionId, OrchestrationEdge,
    OrchestrationEdgeId, OrchestrationNode, OrchestrationNodeId, OrchestrationNodeKind,
    OrchestrationPolicies, OutputBinding, OutputContract, OutputNodeConfig, RetryPolicy,
    RetryReason, SchemaReference, StructuredOutputMode, ToolScope, VerificationCheck,
    VerifyNodeConfig, ORCHESTRATION_SCHEMA_VERSION,
};

const INPUT: &str = "input";
const EXECUTE: &str = "execute";
const VERIFY: &str = "verify";
const OUTPUT: &str = "output";

/// Input, then one agent that does the work and reports on it, then a check of the report, then
/// the output. A failed check sends the agent round again once.
pub fn default_orchestration_definition() -> OrchestrationDefinition {
    OrchestrationDefinition {
        schema_version: ORCHESTRATION_SCHEMA_VERSION,
        id: OrchestrationDefinitionId::from("rusty.default"),
        revision: 1,
        name: "Default orchestration".into(),
        description: Some("Input → Agent → Verify → Output".into()),
        status: DefinitionStatus::Published,
        input_schema: Some(request_schema()),
        output_contract: OutputContract {
            schema: report_schema_reference(),
            source: Some(execute_output()),
            strict: true,
        },
        nodes: vec![input_node(), execute_node(), verify_node(), output_node()],
        edges: vec![
            success_edge("input-execute", INPUT, EXECUTE),
            success_edge("execute-verify", EXECUTE, VERIFY),
            success_edge("verify-output", VERIFY, OUTPUT),
        ],
        policies: OrchestrationPolicies::default(),
        metadata: Value::Null,
    }
}

fn input_node() -> OrchestrationNode {
    node(
        INPUT,
        "Input",
        OrchestrationNodeKind::Input(InputNodeConfig::default()),
    )
}

fn execute_node() -> OrchestrationNode {
    OrchestrationNode {
        input_bindings: vec![InputBinding {
            target: "request".into(),
            source: OutputBinding::NodeOutput {
                node_id: OrchestrationNodeId::from(INPUT),
                pointer: "/request".into(),
            },
        }],
        output_schema: Some(report_schema_reference()),
        retry: RetryPolicy {
            max_attempts: 2,
            retry_on: vec![
                RetryReason::BackendRateLimited,
                RetryReason::BackendTimeout,
                RetryReason::InvalidStructuredOutput,
                RetryReason::VerificationFailed,
            ],
        },
        ..node(
            EXECUTE,
            "Execute",
            OrchestrationNodeKind::Agent(AgentNodeConfig {
                task_queue: None,
                instructions: "Complete the user's request and return the execution report.".into(),
                tools: ToolScope::Inherit,
                context_mode: AgentContextMode::IsolatedChild,
                model: None,
                structured_output: StructuredOutputMode::Require,
                profile: None,
                context_access: Default::default(),
            }),
        )
    }
}

fn verify_node() -> OrchestrationNode {
    OrchestrationNode {
        input_bindings: vec![InputBinding {
            target: "report".into(),
            source: execute_output(),
        }],
        ..node(
            VERIFY,
            "Verify",
            OrchestrationNodeKind::Verify(VerifyNodeConfig {
                checks: vec![
                    VerificationCheck::Schema,
                    VerificationCheck::RequiredStatus {
                        pointer: "/report/status".into(),
                        equals: "completed".into(),
                    },
                    VerificationCheck::ArtifactsResolvable {
                        pointer: "/report/artifacts".into(),
                    },
                ],
                retry_target: Some(OrchestrationNodeId::from(EXECUTE)),
            }),
        )
    }
}

fn output_node() -> OrchestrationNode {
    node(
        OUTPUT,
        "Output",
        OrchestrationNodeKind::Output(OutputNodeConfig {
            source: execute_output(),
            strict: true,
        }),
    )
}

/// A node with no bindings, schema, custom retry or timeout; callers override what they need.
fn node(id: &str, name: &str, kind: OrchestrationNodeKind) -> OrchestrationNode {
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

/// The agent's whole output.
fn execute_output() -> OutputBinding {
    OutputBinding::NodeOutput {
        node_id: OrchestrationNodeId::from(EXECUTE),
        pointer: String::new(),
    }
}

fn request_schema() -> SchemaReference {
    SchemaReference::Inline {
        name: "orchestration_input".into(),
        schema: json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["request"],
            "properties": {
                "request": { "type": "string", "minLength": 1 },
                "attachments": { "type": "array", "default": [] }
            }
        }),
    }
}

fn report_schema_reference() -> SchemaReference {
    SchemaReference::Inline {
        name: "execution_report".into(),
        schema: execution_report_schema(),
    }
}

fn success_edge(id: &str, source: &str, target: &str) -> OrchestrationEdge {
    OrchestrationEdge {
        id: OrchestrationEdgeId::from(id),
        source: OrchestrationNodeId::from(source),
        target: OrchestrationNodeId::from(target),
        condition: EdgeCondition::OnSuccess,
        metadata: Value::Null,
    }
}

fn execution_report_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["summary", "status", "artifacts", "claimsToVerify"],
        "properties": {
            "summary": { "type": "string" },
            "status": { "enum": ["completed", "blocked", "failed"] },
            "artifacts": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["kind", "reference"],
                    "properties": {
                        "kind": { "type": "string" },
                        "reference": { "type": "string" }
                    }
                }
            },
            "claimsToVerify": {
                "type": "array",
                "items": { "type": "string" }
            }
        }
    })
}
