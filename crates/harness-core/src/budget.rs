//! Deterministic budget checks for protocol budget values.

use harness_protocol::usage::{AgentBudget, Cost, ModelUsage};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BudgetError {
    #[error("Usage exceeds budget: {0}")]
    Exceeded(String),
    #[error("Cost exceeds budget")]
    CostExceeded,
    #[error("max_children ({limit}) exceeded: already have {current}")]
    TooManyChildren { limit: u32, current: u32 },
    #[error("max_depth ({limit}) exceeded: current depth is {current}")]
    MaxDepthExceeded { limit: u32, current: u32 },
}

/// Extension methods for checking an [`AgentBudget`] without violating Rust's orphan rules.
pub trait BudgetCheck {
    fn check_usage(&self, usage: &ModelUsage) -> Result<(), BudgetError>;
    fn check_cost(&self, cost: &Cost) -> Result<(), BudgetError>;
    fn check_children(&self, current_children: u32) -> Result<(), BudgetError>;
    fn check_depth(&self, current_depth: u32) -> Result<(), BudgetError>;
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

    fn check_children(&self, current_children: u32) -> Result<(), BudgetError> {
        if let Some(limit) = self.max_children {
            if current_children >= limit {
                return Err(BudgetError::TooManyChildren { limit, current: current_children });
            }
        }
        Ok(())
    }

    fn check_depth(&self, current_depth: u32) -> Result<(), BudgetError> {
        if let Some(limit) = self.max_depth {
            if current_depth >= limit {
                return Err(BudgetError::MaxDepthExceeded { limit, current: current_depth });
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

    #[test]
    fn children_limit_is_enforced() {
        let budget = AgentBudget { max_children: Some(3), ..Default::default() };
        // Below the limit passes.
        assert!(budget.check_children(2).is_ok());
        // At the limit is rejected.
        assert_eq!(
            budget.check_children(3),
            Err(BudgetError::TooManyChildren { limit: 3, current: 3 })
        );
        // Above the limit is rejected.
        assert!(budget.check_children(4).is_err());
    }

    #[test]
    fn depth_limit_is_enforced() {
        let budget = AgentBudget { max_depth: Some(3), ..Default::default() };
        // Below the limit passes.
        assert!(budget.check_depth(2).is_ok());
        // At the limit is rejected.
        assert_eq!(
            budget.check_depth(3),
            Err(BudgetError::MaxDepthExceeded { limit: 3, current: 3 })
        );
        // Above the limit is rejected.
        assert!(budget.check_depth(4).is_err());
    }

    #[test]
    fn unbounded_limits_pass() {
        let budget = AgentBudget::default();
        // Unbounded children and depth always pass.
        assert!(budget.check_children(0).is_ok());
        assert!(budget.check_children(1000).is_ok());
        assert!(budget.check_depth(0).is_ok());
        assert!(budget.check_depth(1000).is_ok());
    }
}
