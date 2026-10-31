//! The tools a step's session may use.

use std::collections::HashMap;

use rusty_agent_core::tool_alias::with_aliases;
use rusty_protocol::tools::{AgentToolset, PermissionMode, ToolCapability, ToolPolicy};
use rusty_runtime::traits::ToolRegistry;
use rusty_workflow_core::MAX_SUBFLOW_DEPTH;

use super::research::{RESEARCH_INSTRUCTIONS, RESEARCH_TOOL, RESEARCH_TOOLS};
use super::IsolatedSessionAgentExecutor;
use crate::orchestration::AgentStepRequest;

/// The tool names a step may call, and whether it may ask for research.
pub(super) struct StepTools {
    pub names: Vec<String>,
    pub research: bool,
    /// What a research step started for this step may use.
    pub research_tools: Vec<String>,
}

impl IsolatedSessionAgentExecutor {
    pub(super) fn step_tools(&self, request: &AgentStepRequest) -> StepTools {
        // A step scoped to `write_file` may also use its aliases (`edit_file`), and one that may
        // read may ask for research.
        let mut names = with_aliases(&request.tools);
        let research_tools: Vec<String> = names
            .iter()
            .filter(|tool| RESEARCH_TOOLS.contains(&tool.as_str()))
            .cloned()
            .collect();
        // A research step never asks for research itself: `read_file` admits `request_research`,
        // and without this each research step could start another, nesting until the first
        // call's timeout cancels them all.
        let research = request.research.max_per_attempt > 0
            && request.instructions != RESEARCH_INSTRUCTIONS
            && self.subflows.is_some()
            && request.subflow_depth <= MAX_SUBFLOW_DEPTH
            && names.iter().any(|tool| tool == RESEARCH_TOOL)
            && research_tools.iter().any(|tool| tool == "read_file");
        if !research {
            names.retain(|tool| tool != RESEARCH_TOOL);
        }
        StepTools {
            names,
            research,
            research_tools,
        }
    }

    /// Descriptors come from the scoped registry, so only allow-listed tools appear. Each keeps
    /// the parent's policy for that tool; a tool the parent does not list defaults to `Ask`,
    /// never to `Allow`.
    pub(super) fn scoped_toolset(&self, registry: &dyn ToolRegistry) -> AgentToolset {
        let parent_policies: HashMap<String, ToolPolicy> = self
            .parent
            .root_toolset()
            .tools
            .into_values()
            .map(|capability| (capability.descriptor.name, capability.policy))
            .collect();
        let tools = registry
            .descriptors()
            .into_iter()
            .filter_map(|descriptor| capability_of(descriptor, &parent_policies))
            .collect();
        AgentToolset { tools }
    }
}

fn capability_of(
    descriptor: rusty_tool_api::ToolDescriptor,
    parent_policies: &HashMap<String, ToolPolicy>,
) -> Option<(rusty_protocol::ids::ToolId, ToolCapability)> {
    let name = descriptor.id.to_string();
    // Research only reads, with tools the step already has.
    let policy = if name == RESEARCH_TOOL {
        ToolPolicy {
            permission: PermissionMode::Allow,
            enabled: true,
        }
    } else {
        parent_policies.get(&name).cloned().unwrap_or(ToolPolicy {
            permission: PermissionMode::Ask,
            enabled: true,
        })
    };
    if !policy.enabled {
        return None;
    }
    let id = rusty_protocol::ids::ToolId::new();
    let descriptor = rusty_protocol::tools::ToolDescriptor {
        id,
        name,
        description: descriptor.description,
        input_schema: descriptor.input_schema,
    };
    Some((
        id,
        ToolCapability {
            descriptor,
            policy,
            delegatable: false,
        },
    ))
}
