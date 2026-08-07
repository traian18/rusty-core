//! Usage, cost, budget, and snapshot protocol types.

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// A reported counter. `None` means unknown and is distinct from `Some(0)`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageValue(Option<u64>);

impl UsageValue {
    pub const fn new(value: Option<u64>) -> Self {
        Self(value)
    }

    pub const fn is_unknown(&self) -> bool {
        self.0.is_none()
    }

    pub const fn is_zero(&self) -> bool {
        matches!(self.0, Some(0))
    }

    pub const fn value(&self) -> Option<u64> {
        self.0
    }

    /// Adds reported contributions without treating an unknown value as zero.
    pub fn checked_add(self, other: Self) -> Self {
        Self(match (self.0, other.0) {
            (Some(left), Some(right)) => left.checked_add(right),
            _ => None,
        })
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelUsage {
    pub input_tokens: UsageValue,
    pub output_tokens: UsageValue,
    pub cache_read_tokens: UsageValue,
    pub cache_write_tokens: UsageValue,
    pub reasoning_tokens: UsageValue,
    pub total_tokens: UsageValue,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CostSource {
    ProviderReported,
    Calculated,
    Estimated,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Cost {
    pub amount_usd: Option<Decimal>,
    pub source: Option<CostSource>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageRecord {
    pub model_usage: ModelUsage,
    pub cost: Cost,
    pub tool_usage: Option<ModelUsage>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentBudget {
    pub max_input_tokens: Option<u64>,
    pub max_output_tokens: Option<u64>,
    pub max_total_tokens: Option<u64>,
    pub max_cost_usd: Option<Decimal>,
    pub max_requests: Option<u64>,
    pub max_tool_calls: Option<u64>,
    pub max_children: Option<u32>,
    pub max_depth: Option<u32>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CumulativeUsage {
    pub total_tokens: UsageValue,
    pub total_cost: Option<Decimal>,
    pub total_requests: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ContextUsage {
    pub tokens_loaded: UsageValue,
    pub tokens_evicted: UsageValue,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RunUsage {
    pub model_usage: Option<ModelUsage>,
    pub cost: Option<Cost>,
    pub request_count: u64,
    pub tool_call_count: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AgentUsageMetrics {
    pub total_runs: u64,
    pub total_requests: u64,
    pub total_tool_calls: u64,
    pub total_tokens: UsageValue,
    pub total_cost: Option<Decimal>,
}

/// Separates direct, descendant, and inclusive usage without mutating parent totals.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentUsageSummary {
    pub self_usage: AgentUsageMetrics,
    pub descendant_usage: AgentUsageMetrics,
    pub inclusive_usage: AgentUsageMetrics,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentUsageSnapshot {
    pub agent_id: String,
    pub metrics: AgentUsageMetrics,
    pub timestamp: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionUsageSnapshot {
    pub session_id: String,
    pub cumulative: CumulativeUsage,
    pub timestamp: String,
}

#[cfg(test)]
mod tests {
    use super::UsageValue;

    #[test]
    fn unknown_is_distinct_from_known_zero() {
        assert!(UsageValue::new(None).is_unknown());
        assert!(!UsageValue::new(None).is_zero());
        assert!(UsageValue::new(Some(0)).is_zero());
    }

    #[test]
    fn aggregation_preserves_unknown() {
        assert!(UsageValue::new(None)
            .checked_add(UsageValue::new(Some(0)))
            .is_unknown());
        assert_eq!(
            UsageValue::new(Some(0))
                .checked_add(UsageValue::new(Some(0)))
                .value(),
            Some(0)
        );
    }
}
