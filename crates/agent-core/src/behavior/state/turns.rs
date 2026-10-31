//! Starting a turn and counting tool calls against the profile's limits.

use super::{BehaviorState, TurnPlan, FINAL_TURN_GRACE};

impl BehaviorState {
    /// Account for a new model request (not a re-issue after resume) and
    /// decide its shape against the profile's limits.
    pub fn begin_turn(&mut self) -> TurnPlan {
        if self.run.final_turn {
            if self.run.final_grace_used >= FINAL_TURN_GRACE {
                return TurnPlan::Exceeded;
            }
            // The model asked for tools again; retry the tool-less final turn.
            self.run.final_grace_used += 1;
            return TurnPlan::Final {
                prompt: self.profile.profile.limits.final_turn_prompt.clone(),
            };
        }
        self.run.turns += 1;
        let limits = &self.profile.profile.limits;
        let turns_exhausted = limits.max_turns.is_some_and(|max| self.run.turns >= max);
        let tools_exhausted = limits
            .max_tool_calls
            .is_some_and(|max| self.run.tool_calls >= max);
        if turns_exhausted || tools_exhausted {
            self.run.final_turn = true;
            TurnPlan::Final {
                prompt: limits.final_turn_prompt.clone(),
            }
        } else {
            TurnPlan::Normal
        }
    }

    /// Count a tool call, or refuse it when the run is out of tool calls or
    /// on its final turn.
    pub fn admit_tool_call(&mut self) -> bool {
        let over_budget = self
            .profile
            .profile
            .limits
            .max_tool_calls
            .is_some_and(|max| self.run.tool_calls >= max);
        if self.run.final_turn || over_budget {
            return false;
        }
        self.run.tool_calls += 1;
        true
    }
}
