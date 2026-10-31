//! The usage of the agents of a step, summed for the workflow.

use std::collections::HashMap;

use rust_decimal::prelude::ToPrimitive;
use rusty_protocol::{ids::AgentId, usage::AgentUsageMetrics};
use rusty_workflow_core::UsageSummary;

pub(super) fn usage_summary(metrics: &HashMap<AgentId, AgentUsageMetrics>) -> UsageSummary {
    let mut summary = UsageSummary::default();
    for metrics in metrics.values() {
        summary.model_requests += metrics.total_requests;
        summary.tool_calls += metrics.total_tool_calls;
        match metrics.total_tokens.value() {
            Some(tokens) => summary.tokens += tokens,
            None => summary.tokens_unknown = true,
        }
        match metrics.total_cost.and_then(|cost| cost.to_f64()) {
            Some(cost) => summary.cost_usd += cost,
            None => summary.cost_unknown = true,
        }
    }
    summary
}
