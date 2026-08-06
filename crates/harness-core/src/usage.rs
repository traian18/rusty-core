//! Pure usage-ledger aggregation with no descendant double counting.

use harness_protocol::usage::{ModelUsage, UsageRecord};

use crate::agent::UsageLedger;

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct AgentUsageSummary {
    pub self_usage: ModelUsage,
    pub descendant_usage: ModelUsage,
    pub inclusive_usage: ModelUsage,
}

fn add(left: &ModelUsage, right: &ModelUsage) -> ModelUsage {
    ModelUsage {
        input_tokens: left.input_tokens.checked_add(right.input_tokens),
        output_tokens: left.output_tokens.checked_add(right.output_tokens),
        cache_read_tokens: left.cache_read_tokens.checked_add(right.cache_read_tokens),
        cache_write_tokens: left
            .cache_write_tokens
            .checked_add(right.cache_write_tokens),
        reasoning_tokens: left.reasoning_tokens.checked_add(right.reasoning_tokens),
        total_tokens: left.total_tokens.checked_add(right.total_tokens),
    }
}

fn aggregate<'a>(usages: impl IntoIterator<Item = &'a ModelUsage>) -> ModelUsage {
    let mut usages = usages.into_iter();
    let Some(first) = usages.next() else {
        return ModelUsage::default();
    };
    usages.fold(first.clone(), |total, usage| add(&total, usage))
}

impl UsageLedger {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_record(&mut self, record: UsageRecord) {
        self.records.push(record);
    }

    pub fn self_usage(&self) -> ModelUsage {
        aggregate(self.records.iter().map(|record| &record.model_usage))
    }
}

pub fn compute_agent_usage_summary(
    self_records: &[UsageRecord],
    child_summaries: &[AgentUsageSummary],
) -> AgentUsageSummary {
    let self_usage = aggregate(self_records.iter().map(|record| &record.model_usage));
    let descendant_usage = aggregate(
        child_summaries
            .iter()
            .map(|summary| &summary.inclusive_usage),
    );
    let inclusive_usage = if child_summaries.is_empty() {
        self_usage.clone()
    } else if self_records.is_empty() {
        descendant_usage.clone()
    } else {
        add(&self_usage, &descendant_usage)
    };

    AgentUsageSummary {
        self_usage,
        descendant_usage,
        inclusive_usage,
    }
}

#[cfg(test)]
mod tests {
    use harness_protocol::usage::{Cost, UsageRecord, UsageValue};

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
    fn tree_sum_has_no_double_counting() {
        let parent = vec![record(Some(1), None), record(Some(1), None)];
        let child_usage = aggregate([&record(Some(1), None).model_usage]);
        let child = AgentUsageSummary {
            self_usage: child_usage.clone(),
            descendant_usage: ModelUsage::default(),
            inclusive_usage: child_usage,
        };
        let first = compute_agent_usage_summary(&parent, &[child]);
        let second = compute_agent_usage_summary(&parent, &[]);
        assert_eq!(first.self_usage.input_tokens.value(), Some(2));
        assert_eq!(first.descendant_usage.input_tokens.value(), Some(1));
        assert_eq!(first.inclusive_usage.input_tokens.value(), Some(3));
        assert_eq!(second.self_usage.input_tokens.value(), Some(2));
    }

    #[test]
    fn default_summary_is_all_unknown() {
        let summary = AgentUsageSummary::default();
        assert!(summary.self_usage.input_tokens.is_unknown());
        assert!(summary.inclusive_usage.total_tokens.is_unknown());
    }
}
