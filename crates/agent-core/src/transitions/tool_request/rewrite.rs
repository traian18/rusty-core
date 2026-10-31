//! Rewriting a call's arguments when a rule asks for it.

use rusty_protocol::tools::ToolCall;

use crate::{
    agent::Agent,
    behavior::{merge_patch, system_reminder, RuleOutcome},
};

impl Agent {
    /// Apply `rewrite_args` merge patches. The pending call is replaced, so
    /// approval prompts and execution see the rewritten arguments; the model
    /// is told what ran through a note on the result.
    pub(super) fn rewrite_arguments(
        &mut self,
        mut call: ToolCall,
        outcome: &RuleOutcome,
    ) -> ToolCall {
        if outcome.rewrite_args.is_empty() {
            return call;
        }
        for (_, patch) in &outcome.rewrite_args {
            merge_patch(&mut call.arguments, patch);
        }
        if let Some(pending) = self.state.pending_tools.get_mut(&call.id) {
            pending.call.arguments = call.arguments.clone();
        }
        let rules: Vec<&str> = outcome
            .rewrite_args
            .iter()
            .map(|(rule_id, _)| rule_id.as_str())
            .collect();
        let note = system_reminder(
            &format!(
                "profile:{} rule:{}",
                self.state.behavior.reference(),
                rules.join(",")
            ),
            &format!(
                "The harness adjusted this call's arguments before running it: {}",
                call.arguments
            ),
        );
        self.state
            .behavior
            .run
            .pending_results
            .push((call.id, note));
        call
    }
}
