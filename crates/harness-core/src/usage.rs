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
        AgentUsageMetrics {
            total_runs: self.runs,
            total_requests: self.records.len() as u64,
            total_tool_calls: self.tool_calls,
            total_tokens: aggregate_model_usage(
                self.records.iter().map(|record| &record.model_usage),
            )
            .total_tokens,
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
mod tests {
    use harness_protocol::usage::{Cost, UsageRecord, UsageValue};
    use rust_decimal_macros::dec;

    use super::*;

    fn record(input: Option<u64>, output: Option<u64>) -> UsageRecord {
        UsageRecord {
            model_usage: ModelUsage {
                input_tokens: UsageValue::new(input),
                output_tokens: UsageValue::new(output),
                ..Default::default()
            },
            cost: Cost::default(),
            tool_usage: None,
        }
    }

    fn record_with_cost(amount: Decimal) -> UsageRecord {
        UsageRecord {
            model_usage: ModelUsage::default(),
            cost: Cost {
                amount_usd: Some(amount),
                source: Some(harness_protocol::usage::CostSource::ProviderReported),
            },
            tool_usage: None,
        }
    }

    #[test]
    fn ledger_sums_known_values() {
        let mut ledger = UsageLedger::new();
        ledger.add_record(record(Some(10), None));
        ledger.add_record(record(Some(20), None));
        assert_eq!(ledger.self_usage().input_tokens.value(), Some(30));
    }

    #[test]
    fn unknown_contribution_is_not_zero() {
        let mut ledger = UsageLedger::new();
        ledger.add_record(record(Some(0), Some(0)));
        ledger.add_record(record(Some(0), None));
        assert_eq!(ledger.self_usage().input_tokens.value(), Some(0));
        assert!(ledger.self_usage().output_tokens.is_unknown());
    }

    #[test]
    fn self_metrics_counts_requests_and_tool_calls() {
        let mut ledger = UsageLedger::new();
        ledger.add_record(record(Some(1), None));
        ledger.add_record(record(Some(1), None));
        ledger.add_record(record(Some(1), None));
        ledger.tool_calls = 2;
        let metrics = ledger.self_metrics();
        assert_eq!(metrics.total_requests, 3);
        assert_eq!(metrics.total_tool_calls, 2);
    }

    #[test]
    fn self_metrics_sums_known_costs() {
        let mut ledger = UsageLedger::new();
        ledger.add_record(record_with_cost(dec!(0.05)));
        ledger.add_record(record_with_cost(dec!(0.10)));
        let metrics = ledger.self_metrics();
        assert_eq!(metrics.total_cost, Some(dec!(0.15)));
    }

    #[test]
    fn self_metrics_cost_is_unknown_if_any_record_is_unknown() {
        let mut ledger = UsageLedger::new();
        ledger.add_record(record_with_cost(dec!(0.05)));
        ledger.add_record(record(Some(1), None)); // Cost::default() => amount_usd: None
        let metrics = ledger.self_metrics();
        assert_eq!(metrics.total_cost, None);
    }

    #[test]
    fn empty_ledger_reports_zero_requests_and_unknown_cost() {
        let ledger = UsageLedger::new();
        let metrics = ledger.self_metrics();
        assert_eq!(metrics.total_requests, 0);
        assert_eq!(metrics.total_tool_calls, 0);
        assert_eq!(metrics.total_cost, None);
    }

    #[test]
    fn self_metrics_reports_the_ledgers_real_run_count() {
        let mut ledger = UsageLedger::new();
        assert_eq!(
            ledger.self_metrics().total_runs,
            0,
            "a fresh ledger has completed no runs"
        );
        ledger.runs = 3;
        assert_eq!(ledger.self_metrics().total_runs, 3);
    }

    #[test]
    fn compute_agent_usage_summary_sums_real_run_counts_with_no_double_counting() {
        let self_metrics = AgentUsageMetrics {
            total_runs: 2,
            total_requests: 2,
            ..Default::default()
        };
        let child = AgentUsageSummary {
            self_usage: AgentUsageMetrics {
                total_runs: 1,
                ..Default::default()
            },
            descendant_usage: AgentUsageMetrics::default(),
            inclusive_usage: AgentUsageMetrics {
                total_runs: 1,
                ..Default::default()
            },
        };
        let summary = compute_agent_usage_summary(self_metrics, &[child]);
        assert_eq!(
            summary.descendant_usage.total_runs, 1,
            "only the child's inclusive total counts, not its self total again"
        );
        assert_eq!(
            summary.inclusive_usage.total_runs, 3,
            "this agent's 2 runs plus its child's 1"
        );
    }

    #[test]
    fn tree_sum_has_no_double_counting() {
        let self_metrics = AgentUsageMetrics {
            total_requests: 2,
            total_tokens: UsageValue::new(Some(2)),
            ..Default::default()
        };
        let child = AgentUsageSummary {
            self_usage: AgentUsageMetrics {
                total_requests: 1,
                total_tokens: UsageValue::new(Some(1)),
                ..Default::default()
            },
            descendant_usage: AgentUsageMetrics::default(),
            inclusive_usage: AgentUsageMetrics {
                total_requests: 1,
                total_tokens: UsageValue::new(Some(1)),
                ..Default::default()
            },
        };
        let first = compute_agent_usage_summary(self_metrics.clone(), &[child]);
        let second = compute_agent_usage_summary(self_metrics, &[]);

        assert_eq!(first.self_usage.total_requests, 2);
        assert_eq!(first.descendant_usage.total_requests, 1);
        assert_eq!(first.inclusive_usage.total_requests, 3);
        assert_eq!(first.inclusive_usage.total_tokens.value(), Some(3));
        assert_eq!(second.self_usage.total_requests, 2);
        assert_eq!(second.descendant_usage.total_requests, 0);
    }

    #[test]
    fn grandchild_usage_is_not_double_counted_through_a_child() {
        // grandchild: 1 request. child: 1 request of its own + the
        // grandchild's inclusive usage already folded in = 2 inclusive.
        let grandchild_inclusive = AgentUsageMetrics {
            total_requests: 1,
            ..Default::default()
        };
        let child_self = AgentUsageMetrics {
            total_requests: 1,
            ..Default::default()
        };
        let child_summary = compute_agent_usage_summary(
            child_self,
            &[AgentUsageSummary {
                self_usage: grandchild_inclusive.clone(),
                descendant_usage: AgentUsageMetrics::default(),
                inclusive_usage: grandchild_inclusive,
            }],
        );
        assert_eq!(child_summary.inclusive_usage.total_requests, 2);

        // parent has no requests of its own; its only child is the one above.
        let parent_summary =
            compute_agent_usage_summary(AgentUsageMetrics::default(), &[child_summary]);
        // Must be 2 (child's 1 + grandchild's 1), not 3 or more — proves the
        // parent aggregates the child's *inclusive* usage exactly once,
        // rather than separately re-adding the grandchild.
        assert_eq!(parent_summary.inclusive_usage.total_requests, 2);
    }

    /// Regression test: a leaf agent (no children at all) must report its
    /// own known usage as `inclusive_usage` unchanged — an empty child list
    /// must not poison known totals down to "unknown" via
    /// `checked_add`'s unknown-poisons-the-sum semantics. This was a real
    /// regression introduced while reworking this module for M4 and caught
    /// by `harness-integration-anthropic`'s session e2e tests.
    #[test]
    fn leaf_agent_with_no_children_reports_its_own_known_usage_unchanged() {
        let self_metrics = AgentUsageMetrics {
            total_requests: 1,
            total_tokens: UsageValue::new(Some(15)),
            total_cost: Some(dec!(0.000105)),
            ..Default::default()
        };
        let summary = compute_agent_usage_summary(self_metrics.clone(), &[]);
        assert_eq!(summary.inclusive_usage, self_metrics);
        assert_eq!(summary.inclusive_usage.total_tokens.value(), Some(15));
        assert_eq!(summary.inclusive_usage.total_cost, Some(dec!(0.000105)));
    }

    #[test]
    fn default_summary_is_all_zero_or_unknown() {
        let summary = AgentUsageSummary::default();
        assert!(summary.self_usage.total_tokens.is_unknown());
        assert!(summary.inclusive_usage.total_tokens.is_unknown());
        assert_eq!(summary.self_usage.total_requests, 0);
        assert_eq!(summary.self_usage.total_cost, None);
    }
}
