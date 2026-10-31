//! A child may never be given more than its parent has.

use rusty_protocol::usage::AgentBudget;

use super::SupervisorError;

/// Refuses a child budget that is looser than the parent's for any limit the parent sets.
pub(super) fn validate_child_budget(
    parent: &AgentBudget,
    child: &AgentBudget,
) -> Result<(), SupervisorError> {
    macro_rules! no_looser {
        ($field:ident) => {
            match (parent.$field, child.$field) {
                (Some(parent_limit), Some(child_limit)) if child_limit <= parent_limit => {}
                (Some(_), _) => return Err(SupervisorError::BudgetEscalation(stringify!($field))),
                (None, _) => {}
            }
        };
    }
    no_looser!(max_input_tokens);
    no_looser!(max_output_tokens);
    no_looser!(max_total_tokens);
    no_looser!(max_cost_usd);
    no_looser!(max_requests);
    no_looser!(max_tool_calls);
    no_looser!(max_children);
    no_looser!(max_depth);
    Ok(())
}
