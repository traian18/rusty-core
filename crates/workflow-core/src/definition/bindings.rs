//! Graph edges, data bindings, and output schema contracts.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{default_true, OrchestrationEdgeId, OrchestrationNodeId};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrchestrationEdge {
    pub id: OrchestrationEdgeId,
    pub source: OrchestrationNodeId,
    pub target: OrchestrationNodeId,
    pub condition: EdgeCondition,
    #[serde(default)]
    pub metadata: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeCondition {
    OnSuccess,
    OnFailure,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InputBinding {
    pub target: String,
    pub source: OutputBinding,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OutputBinding {
    RunInput {
        pointer: String,
    },
    NodeOutput {
        node_id: OrchestrationNodeId,
        pointer: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OutputContract {
    pub schema: SchemaReference,
    /// Which node produces the result; omitted, it is the output node's own
    /// source (the usual case).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<OutputBinding>,
    #[serde(default = "default_true")]
    pub strict: bool,
}

impl Default for OutputContract {
    /// A written result, taken from the output node and not schema-checked.
    fn default() -> Self {
        Self {
            schema: SchemaReference::Inline {
                name: "result".into(),
                schema: serde_json::json!({"type": "string"}),
            },
            source: None,
            strict: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SchemaReference {
    Inline { name: String, schema: Value },
    Registry { schema_id: String, revision: u64 },
}
