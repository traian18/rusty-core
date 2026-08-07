//! Provider-agnostic tool descriptors, policy, calls, and results.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::ids::{ToolCallId, ToolId};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDescriptor {
    pub id: ToolId,
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PermissionMode {
    Allow,
    Ask,
    Deny,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolPolicy {
    pub permission: PermissionMode,
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCapability {
    pub descriptor: ToolDescriptor,
    pub policy: ToolPolicy,
    pub delegatable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentToolset {
    pub tools: HashMap<ToolId, ToolCapability>,
}

impl AgentToolset {
    /// Returns enabled descriptors in stable `ToolId` order.
    pub fn enabled_descriptors(&self) -> Vec<&ToolDescriptor> {
        let mut enabled: Vec<_> = self
            .tools
            .iter()
            .filter(|(_, capability)| capability.policy.enabled)
            .collect();
        enabled.sort_by_key(|(id, _)| **id);
        enabled
            .into_iter()
            .map(|(_, capability)| &capability.descriptor)
            .collect()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: ToolCallId,
    pub name: String,
    pub arguments: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub call_id: ToolCallId,
    pub output: serde_json::Value,
    pub is_error: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResultSummary {
    pub has_error: bool,
    pub output_preview: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ToolError {
    ExecutionFailed,
    PermissionDenied,
    Timeout,
    Internal,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolProgress {
    pub status: String,
    pub fraction: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn capability(id: ToolId, enabled: bool) -> ToolCapability {
        ToolCapability {
            descriptor: ToolDescriptor {
                id,
                name: id.to_string(),
                description: "test".into(),
                input_schema: serde_json::json!({}),
            },
            policy: ToolPolicy {
                permission: PermissionMode::Allow,
                enabled,
            },
            delegatable: false,
        }
    }

    #[test]
    fn enabled_descriptors_exclude_disabled_and_are_stable() {
        let first = ToolId::from_uuid(uuid::Uuid::from_u128(1));
        let second = ToolId::from_uuid(uuid::Uuid::from_u128(2));
        let disabled = ToolId::from_uuid(uuid::Uuid::from_u128(3));
        let toolset = AgentToolset {
            tools: HashMap::from([
                (second, capability(second, true)),
                (disabled, capability(disabled, false)),
                (first, capability(first, true)),
            ]),
        };
        let enabled = toolset.enabled_descriptors();
        assert_eq!(
            enabled.iter().map(|tool| tool.id).collect::<Vec<_>>(),
            vec![first, second]
        );
    }
}
