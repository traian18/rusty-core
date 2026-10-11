//! Pure usage-ledger aggregation with no descendant double counting.
//!
//! M4 correctness fix: this module previously only aggregated `total_tokens`
//! — request count, tool-call count, and cost were always reported as
//! zero/unknown regardless of actual activity, even though every
//! [`UsageRecord`] pushed onto a [`UsageLedger`] already carries a real,
//! provider-reported [`Cost`](harness_protocol::usage::Cost). This module now
//! reuses [`harness_protocol::usage::AgentUsageMetrics`] (rather than a
//! separate, poorer core-level type) as the aggregation unit, so the same
//! struct used for durable usage snapshots carries real data end to end.

use harness_protocol::usage::{AgentUsageMetrics, ModelUsage, UsageRecord};
use rust_decimal::Decimal;

use crate::agent::UsageLedger;

/// Self/descendant/inclusive usage split, matching
/// [`harness_protocol::usage::AgentUsageSummary`]'s shape exactly (this type
/// exists so `harness-core` doesn't need to depend on RPC/wire concerns of
/// the protocol crate's `AgentUsageSummary` for its own internal ledger
/// bookkeeping) — see the `From` impls below for lossless conversion between
/// the two.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
pub struct AgentUsageSummary {
    pub self_usage: AgentUsageMetrics,
    pub descendant_usage: AgentUsageMetrics,
    pub inclusive_usage: AgentUsageMetrics,
}

impl From<harness_protocol::usage::AgentUsageSummary> for AgentUsageSummary {
    fn from(value: harness_protocol::usage::AgentUsageSummary) -> Self {
        Self {
            self_usage: value.self_usage,
            descendant_usage: value.descendant_usage,
            inclusive_usage: value.inclusive_usage,
        }
    }
}

impl From<AgentUsageSummary> for harness_protocol::usage::AgentUsageSummary {
    fn from(value: AgentUsageSummary) -> Self {
        Self {
            self_usage: value.self_usage,
            descendant_usage: value.descendant_usage,
            inclusive_usage: value.inclusive_usage,
        }
    }
}

/// Sums two `Option<Decimal>` cost contributions the same way
/// [`harness_protocol::usage::UsageValue::checked_add`] treats token counts:
/// an unknown (`None`) contribution poisons the total to `None` rather than
/// being silently treated as zero. A session that mixes a provider reporting
/// exact cost with one that doesn't must show "cost unknown," not an
/// understated total.
fn checked_add_cost(left: Option<Decimal>, right: Option<Decimal>) -> Option<Decimal> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left + right),
        _ => None,
    }
}

fn add_metrics(left: &AgentUsageMetrics, right: &AgentUsageMetrics) -> AgentUsageMetrics {
    AgentUsageMetrics {
        total_runs: left.total_runs.saturating_add(right.total_runs),
        total_requests: left.total_requests.saturating_add(right.total_requests),
        total_tool_calls: left.total_tool_calls.saturating_add(right.total_tool_calls),
        total_tokens: left.total_tokens.checked_add(right.total_tokens),
        input_tokens: left.input_tokens.checked_add(right.input_tokens),
        output_tokens: left.output_tokens.checked_add(right.output_tokens),
        cache_read_tokens: left.cache_read_tokens.checked_add(right.cache_read_tokens),
        cache_write_tokens: left
            .cache_write_tokens
            .checked_add(right.cache_write_tokens),
        reasoning_tokens: left.reasoning_tokens.checked_add(right.reasoning_tokens),
        total_cost: checked_add_cost(left.total_cost, right.total_cost),
    }
}

fn aggregate_metrics<'a>(
    metrics: impl IntoIterator<Item = &'a AgentUsageMetrics>,
) -> AgentUsageMetrics {
    let mut metrics = metrics.into_iter();
    let Some(first) = metrics.next() else {
        return AgentUsageMetrics::default();
    };
    metrics.fold(first.clone(), |total, next| add_metrics(&total, next))
}

fn aggregate_model_usage<'a>(usages: impl IntoIterator<Item = &'a ModelUsage>) -> ModelUsage {
    let mut usages = usages.into_iter();
    let Some(first) = usages.next() else {
        return ModelUsage::default();
    };
    usages.fold(first.clone(), |total, usage| ModelUsage {
        input_tokens: total.input_tokens.checked_add(usage.input_tokens),
        output_tokens: total.output_tokens.checked_add(usage.output_tokens),
        cache_read_tokens: total.cache_read_tokens.checked_add(usage.cache_read_tokens),
        cache_write_tokens: total
            .cache_write_tokens
            .checked_add(usage.cache_write_tokens),
        reasoning_tokens: total.reasoning_tokens.checked_add(usage.reasoning_tokens),
        total_tokens: total.total_tokens.checked_add(usage.total_tokens),
    })
}

impl UsageLedger {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_record(&mut self, record: UsageRecord) {
        self.records.push(record);
    }

    /// Raw token usage across every recorded backend request/turn, ignoring
    /// cost/request-count/tool-call bookkeeping. Kept for callers that only
    /// care about tokens (e.g. context-budget checks).
    pub fn self_usage(&self) -> ModelUsage {
        aggregate_model_usage(self.records.iter().map(|record| &record.model_usage))
    }

    /// This agent's own aggregated usage metrics — completed-run count,
    /// request count, tool-call count, total tokens, and total cost —
    /// excluding any descendants.
    pub fn self_metrics(&self) -> AgentUsageMetrics {
        let aggregated =
            aggregate_model_usage(self.records.iter().map(|record| &record.model_usage));
        AgentUsageMetrics {
            total_runs: self.runs,
            total_requests: self.records.len() as u64,
            total_tool_calls: self.tool_calls,
            total_tokens: aggregated.total_tokens,
            input_tokens: aggregated.input_tokens,
            output_tokens: aggregated.output_tokens,
            cache_read_tokens: aggregated.cache_read_tokens,
            cache_write_tokens: aggregated.cache_write_tokens,
            reasoning_tokens: aggregated.reasoning_tokens,
            total_cost: self
                .records
                .iter()
                .map(|record| record.cost.amount_usd)
                .reduce(checked_add_cost)
                .unwrap_or(None),
        }
    }
}

/// Combines this agent's own usage with its (already-aggregated) children's
/// inclusive usage into a self/descendant/inclusive split with no double
/// counting: each child's `inclusive_usage` already covers that child's own
/// descendants, so summing children's `inclusive_usage` values (not their
/// `self_usage`) is what avoids re-counting grandchildren twice.
///
/// `total_runs` on the returned summary is real: `self_metrics.total_runs`
/// (from `UsageLedger::runs`, incremented at every `AgentEffect::FinishRun`
/// emission site — see its doc comment for the exact counted/not-counted
/// cases) is combined with children's already-correct `inclusive_usage`
/// totals by the same `add_metrics` this function uses for every other
/// field, so no special-casing is needed here.
pub fn compute_agent_usage_summary(
    self_metrics: AgentUsageMetrics,
    child_summaries: &[AgentUsageSummary],
) -> AgentUsageSummary {
    let descendant_usage = aggregate_metrics(
        child_summaries
            .iter()
            .map(|summary| &summary.inclusive_usage),
    );
    // `add_metrics`/`checked_add` treats an unknown contribution as
    // poisoning the whole sum to unknown — correct when genuinely combining
    // two partially-known sources, but wrong here for the overwhelmingly
    // common leaf-agent case: no children at all means `descendant_usage` is
    // `AgentUsageMetrics::default()` (unknown token/cost totals, zero
    // counts), and blindly adding that in would poison this agent's own
    // perfectly-known `self_metrics` down to unknown. Mirror the prior
    // (pre-M4) special-casing: an empty side of the sum means "just use the
    // other side," not "combine with an empty unknown."
    let inclusive_usage = if child_summaries.is_empty() {
        self_metrics.clone()
    } else if self_metrics.total_requests == 0 {
        descendant_usage.clone()
    } else {
        add_metrics(&self_metrics, &descendant_usage)
    };

    AgentUsageSummary {
        self_usage: self_metrics,
        descendant_usage,
        inclusive_usage,
    }
}

#[cfg(test)]
mod tests;
