use serde_json::{json, Value};

use super::definition::*;

pub fn default_orchestration_definition() -> OrchestrationDefinition {
    let input = OrchestrationNodeId::from("input");
    let execute = OrchestrationNodeId::from("execute");
    let verify = OrchestrationNodeId::from("verify");
    let output = OrchestrationNodeId::from("output");
    let report_schema = execution_report_schema();

    OrchestrationDefinition {
        schema_version: ORCHESTRATION_SCHEMA_VERSION,
        id: OrchestrationDefinitionId::from("rusty.default"),
        revision: 1,
        name: "Default orchestration".into(),
        description: Some("Input → Agent → Verify → Output".into()),
        status: DefinitionStatus::Published,
        input_schema: Some(SchemaReference::Inline {
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
        }),
        output_contract: OutputContract {
            schema: SchemaReference::Inline {
                name: "execution_report".into(),
                schema: report_schema.clone(),
            },
            source: Some(OutputBinding::NodeOutput {
                node_id: execute.clone(),
                pointer: String::new(),
            }),
            strict: true,
        },
        nodes: vec![
            OrchestrationNode {
                id: input.clone(),
                name: "Input".into(),
                kind: OrchestrationNodeKind::Input(InputNodeConfig::default()),
                input_bindings: Vec::new(),
                output_schema: None,
                retry: RetryPolicy::default(),
                timeout_ms: None,
                metadata: Value::Null,
            },
            OrchestrationNode {
                id: execute.clone(),
                name: "Execute".into(),
                kind: OrchestrationNodeKind::Agent(AgentNodeConfig {
                    task_queue: None,
                    instructions: "Complete the user's request and return the execution report."
                        .into(),
                    tools: ToolScope::Inherit,
                    context_mode: AgentContextMode::IsolatedChild,
                    model: None,
                    structured_output: StructuredOutputMode::Require,
                    profile: None,
                }),
                input_bindings: vec![InputBinding {
                    target: "request".into(),
                    source: OutputBinding::NodeOutput {
                        node_id: input.clone(),
                        pointer: "/request".into(),
                    },
                }],
                output_schema: Some(SchemaReference::Inline {
                    name: "execution_report".into(),
                    schema: report_schema,
                }),
                retry: RetryPolicy {
                    max_attempts: 2,
                    retry_on: vec![
                        RetryReason::BackendRateLimited,
                        RetryReason::BackendTimeout,
                        RetryReason::InvalidStructuredOutput,
                        RetryReason::VerificationFailed,
                    ],
                },
                timeout_ms: None,
                metadata: Value::Null,
            },
            OrchestrationNode {
                id: verify.clone(),
                name: "Verify".into(),
                kind: OrchestrationNodeKind::Verify(VerifyNodeConfig {
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
                    retry_target: Some(execute.clone()),
                }),
                input_bindings: vec![InputBinding {
                    target: "report".into(),
                    source: OutputBinding::NodeOutput {
                        node_id: execute.clone(),
                        pointer: String::new(),
                    },
                }],
                output_schema: None,
                retry: RetryPolicy::default(),
                timeout_ms: None,
                metadata: Value::Null,
            },
            OrchestrationNode {
                id: output.clone(),
                name: "Output".into(),
                kind: OrchestrationNodeKind::Output(OutputNodeConfig {
                    source: OutputBinding::NodeOutput {
                        node_id: execute.clone(),
                        pointer: String::new(),
                    },
                    strict: true,
                }),
                input_bindings: Vec::new(),
                output_schema: None,
                retry: RetryPolicy::default(),
                timeout_ms: None,
                metadata: Value::Null,
            },
        ],
        edges: vec![
            success_edge("input-execute", input, execute.clone()),
            success_edge("execute-verify", execute, verify.clone()),
            success_edge("verify-output", verify, output),
        ],
        policies: OrchestrationPolicies::default(),
        metadata: Value::Null,
    }
}

fn success_edge(
    id: &str,
    source: OrchestrationNodeId,
    target: OrchestrationNodeId,
) -> OrchestrationEdge {
    OrchestrationEdge {
        id: OrchestrationEdgeId::from(id),
        source,
        target,
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
