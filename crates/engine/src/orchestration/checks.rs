//! Checks a workflow must pass before it may run: reachability, flows and profiles.

use super::*;
use rusty_workflow_core::{
    CompiledOrchestration, OrchestrationNodeKind, SubflowTarget, MAX_SUBFLOW_DEPTH,
};
use std::sync::Arc;

use crate::HarnessError;

impl RunnerParts {
    /// Every flow `compiled` can reach through subflow nodes must exist and
    /// the chain must not loop or nest deeper than [`MAX_SUBFLOW_DEPTH`].
    /// The profiles of all of them must resolve.
    pub(super) fn check_reachable(
        &self,
        compiled: &Arc<CompiledOrchestration>,
    ) -> Result<(), HarnessError> {
        let mut chain = Vec::new();
        self.check_flow(compiled, 0, &mut chain)
    }

    pub(super) fn check_flow(
        &self,
        compiled: &Arc<CompiledOrchestration>,
        depth: u32,
        chain: &mut Vec<(String, u64)>,
    ) -> Result<(), HarnessError> {
        let key = (compiled.definition_id.to_string(), compiled.revision);
        if chain.contains(&key) {
            return Err(HarnessError::OrchestrationDefinition(format!(
                "flow {}@{} runs itself through its subflows",
                key.0, key.1
            )));
        }
        chain.push(key);
        self.check_profiles(compiled)?;
        for node in compiled.nodes.values() {
            let targets: Vec<&SubflowTarget> = match &node.kind {
                OrchestrationNodeKind::Subflow(config) => vec![&config.target],
                OrchestrationNodeKind::Agent(config) => config
                    .task_queue
                    .iter()
                    .flat_map(|queue| queue.flows.values())
                    .collect(),
                _ => continue,
            };
            if targets.is_empty() {
                continue;
            }
            if depth + 1 > MAX_SUBFLOW_DEPTH {
                return Err(HarnessError::OrchestrationDefinition(format!(
                    "node {}: flows may run flows at most {MAX_SUBFLOW_DEPTH} levels deep",
                    node.id
                )));
            }
            let inputs: Vec<String> = node
                .input_bindings
                .iter()
                .map(|binding| binding.target.clone())
                .collect();
            for target in targets {
                let child = self.target(&node.id, target, &inputs)?;
                self.check_flow(&child, depth + 1, chain)?;
            }
        }
        chain.pop();
        Ok(())
    }

    /// Every profile a step names must resolve before anything runs.
    pub(super) fn check_profiles(
        &self,
        compiled: &CompiledOrchestration,
    ) -> Result<(), HarnessError> {
        for node in compiled.nodes.values() {
            let OrchestrationNodeKind::Agent(config) = &node.kind else {
                continue;
            };
            let queue = config.task_queue.iter().flat_map(|queue| queue.profiles());
            for reference in config.profile.iter().chain(queue) {
                let closure = self
                    .profiles
                    .resolve(reference)
                    .and_then(|profile| self.profiles.resolve_closure(&profile))
                    .map_err(|error| {
                        HarnessError::OrchestrationDefinition(format!("node {}: {error}", node.id))
                    })?;
                crate::profiles::check_command_trust(&self.profiles, &closure, self.command_trust)
                    .map_err(|error| {
                        HarnessError::OrchestrationDefinition(format!("node {}: {error}", node.id))
                    })?;
            }
        }
        Ok(())
    }
}
