//! Deterministic budget checks for protocol budget values.

use harness_protocol::usage::{AgentBudget, Cost, ModelUsage};

#[derive(Debug, Clone, thiserror::Error)]
pub enum BudgetError {
    #[error("Usage exceeds budget: {0}")]
    Exceeded(String),
    #[error("Cost exceeds budget")]
    CostExceeded,
}

/// Extension methods for checking an [`AgentBudget`] without violating Rust's orphan rules.
pub trait BudgetCheck {
    fn check_usage(&self, usage: &ModelUsage) -> Result<(), BudgetError>;
    fn check_cost(&self, cost: &Cost) -> Result<(), BudgetError>;
}

impl BudgetCheck for AgentBudget {
    fn check_usage(&self, usage: &ModelUsage) -> Result<(), BudgetError> {
        if let (Some(limit), Some(actual)) = (self.max_total_tokens, usage.total_tokens.value()) {
            if actual > limit {
                return Err(BudgetError::Exceeded(format!(
                    "total tokens {actual} exceeds limit {limit}"
                )));
            }
        }
        Ok(())
    }

    fn check_cost(&self, cost: &Cost) -> Result<(), BudgetError> {
        if let (Some(limit), Some(actual)) = (self.max_cost_usd, cost.amount_usd) {
            if actual > limit {
                return Err(BudgetError::CostExceeded);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use harness_protocol::usage::{AgentBudget, Cost, ModelUsage, UsageValue};
    use rust_decimal::Decimal;

    use super::*;

    #[test]
    fn usage_limit_is_enforced() {
        let budget = AgentBudget { max_total_tokens: Some(100), ..Default::default() };
        assert!(budget.check_usage(&ModelUsage {
            total_tokens: UsageValue::new(Some(101)),
            ..Default::default()
        }).is_err());
        assert!(budget.check_usage(&ModelUsage {
            total_tokens: UsageValue::new(None),
            ..Default::default()
        }).is_ok());
    }

    #[test]
    fn cost_limit_is_enforced() {
        let budget = AgentBudget {
            max_cost_usd: Some(Decimal::new(10, 0)),
            ..Default::default()
        };
        assert!(budget.check_cost(&Cost {
            amount_usd: Some(Decimal::new(11, 0)),
            source: None,
        }).is_err());
        assert!(budget.check_cost(&Cost::default()).is_ok());
    }
}
