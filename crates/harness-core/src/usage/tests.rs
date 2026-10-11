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
