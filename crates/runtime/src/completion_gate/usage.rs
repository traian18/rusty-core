//! What a verifier session cost, in the shape the parent's usage is kept in.

use rusty_protocol::usage::{Cost, ModelUsage, UsageRecord, UsageValue};

use crate::session_runtime::AgentLiveState;

/// The verifier's usage as the parent's records: one per request it made, with the aggregate
/// tokens and cost on the first so totals stay exact.
pub(super) fn verifier_usage(live: &AgentLiveState) -> Vec<UsageRecord> {
    let metrics = &live.usage.inclusive_usage;
    (0..live.total_requests)
        .map(|index| {
            let first = index == 0;
            let value = |value: UsageValue| {
                if first {
                    value
                } else {
                    UsageValue::new(Some(0))
                }
            };
            UsageRecord {
                model_usage: ModelUsage {
                    input_tokens: value(metrics.input_tokens),
                    output_tokens: value(metrics.output_tokens),
                    cache_read_tokens: value(metrics.cache_read_tokens),
                    cache_write_tokens: value(metrics.cache_write_tokens),
                    reasoning_tokens: value(metrics.reasoning_tokens),
                    total_tokens: value(metrics.total_tokens),
                },
                cost: Cost {
                    amount_usd: if first { live.total_cost_usd } else { None },
                    source: None,
                },
                tool_usage: None,
            }
        })
        .collect()
}
