//! What the run has done so far, recorded so rule conditions can see it.

use rusty_protocol::tools::ToolCall;

use crate::behavior::state::BehaviorState;

impl BehaviorState {
    /// Record a tool request for loop detection. Call before evaluating
    /// `PreToolUse`, so `repeated_call` counts the current request.
    pub fn note_tool_request(&mut self, call: &ToolCall) {
        // serde_json maps are ordered, so this is canonical for equal args.
        let signature = format!("{}\u{0}{}", call.name, call.arguments);
        match &mut self.run.streak {
            Some(streak) if streak.signature == signature => streak.count += 1,
            _ => {
                self.run.streak = Some(crate::behavior::state::CallStreak {
                    signature,
                    count: 1,
                })
            }
        }
    }

    /// Record that a tool actually executed (not denied) in this turn, and
    /// that it succeeded. See [`Self::note_executed_with`].
    pub fn note_executed(&mut self, tool: &str) {
        self.note_executed_with(tool, false);
    }

    /// Record that a tool actually executed (not denied) in this turn.
    /// `failed` is whether its result was an error, which is what the
    /// `outcome` filter of `calls`, `turns_since_call` and `since_last_call`
    /// reads.
    pub fn note_executed_with(&mut self, tool: &str, failed: bool) {
        let turn = self.run.turns;
        self.run
            .executed
            .push(crate::behavior::state::ExecutedCall {
                turn,
                tool: tool.to_string(),
                failed,
            });
    }

    /// Record the tools the latest request offered the model. Not called for
    /// a forced final turn, which offers none.
    pub fn note_offered(&mut self, mut tools: Vec<String>) {
        tools.sort();
        tools.dedup();
        self.run.offered_tools = tools;
    }
}
