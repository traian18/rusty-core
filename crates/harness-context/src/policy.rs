//! Model-aware context budgeting and compaction policy.
//!
//! This module decides *when* context preparation should compact. It does not
//! mutate canonical conversation history or perform summarization.

/// Declares which layer owns the inference provider's active context window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextOwnership {
    /// The harness sends the complete prepared message payload and owns compaction.
    HarnessManaged,
    /// A CLI or remote agent runtime owns its native session context.
    BackendManaged,
}

/// Whether a token estimate came from a provider tokenizer or a conservative fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TokenEstimate {
    pub tokens: u64,
    pub exact: bool,
}

impl TokenEstimate {
    pub const fn exact(tokens: u64) -> Self {
        Self {
            tokens,
            exact: true,
        }
    }

    pub const fn approximate(tokens: u64) -> Self {
        Self {
            tokens,
            exact: false,
        }
    }
}

/// Default thresholds used to avoid repeatedly compacting near the model limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextPolicy {
    /// Portion of the advertised model window considered safe for input plus output.
    pub safe_window_percent: u8,
    /// Schedule background compaction after a stable turn at this pressure.
    pub soft_limit_percent: u8,
    /// Compact synchronously before inference at this pressure.
    pub hard_limit_percent: u8,
    /// Normal compaction should reduce active context to this pressure.
    pub target_percent: u8,
    /// Emergency recovery after a provider context rejection should target this pressure.
    pub emergency_target_percent: u8,
    /// Output capacity held back from the safe model window.
    pub reserved_output_tokens: u64,
}

impl Default for ContextPolicy {
    fn default() -> Self {
        Self {
            safe_window_percent: 90,
            soft_limit_percent: 70,
            hard_limit_percent: 85,
            target_percent: 55,
            emergency_target_percent: 45,
            reserved_output_tokens: 8_192,
        }
    }
}

impl ContextPolicy {
    /// Validates threshold ordering and non-zero percentages.
    pub fn validate(&self) -> Result<(), ContextPolicyError> {
        let ordered = self.emergency_target_percent <= self.target_percent
            && self.target_percent < self.soft_limit_percent
            && self.soft_limit_percent < self.hard_limit_percent
            && self.hard_limit_percent <= 100;
        if self.safe_window_percent == 0 || self.safe_window_percent > 100 || !ordered {
            return Err(ContextPolicyError::InvalidThresholds);
        }
        Ok(())
    }

    /// Evaluates projected input pressure for a known model context window.
    pub fn evaluate(
        &self,
        context_window: Option<u64>,
        projected_input: TokenEstimate,
    ) -> ContextDecision {
        let Some(context_window) = context_window else {
            return ContextDecision::Unavailable {
                projected_input,
                reason: ContextBudgetUnavailable::UnknownContextWindow,
            };
        };

        let safe_window = percentage_of(context_window, self.safe_window_percent);
        let Some(input_budget) = safe_window.checked_sub(self.reserved_output_tokens) else {
            return ContextDecision::Unavailable {
                projected_input,
                reason: ContextBudgetUnavailable::ReservedOutputExceedsSafeWindow,
            };
        };
        if input_budget == 0 {
            return ContextDecision::Unavailable {
                projected_input,
                reason: ContextBudgetUnavailable::ReservedOutputExceedsSafeWindow,
            };
        }

        let pressure_percent = ratio_percent_ceil(projected_input.tokens, input_budget);
        let budget = ContextBudget {
            context_window,
            input_budget,
            projected_input,
            pressure_percent,
        };

        if pressure_percent >= u16::from(self.hard_limit_percent) {
            ContextDecision::CompactBeforeRequest { budget }
        } else if pressure_percent >= u16::from(self.soft_limit_percent) {
            ContextDecision::ScheduleBackgroundCompaction { budget }
        } else {
            ContextDecision::Proceed { budget }
        }
    }

    /// Token target for a normal or emergency compaction pass.
    pub fn target_tokens(&self, input_budget: u64, emergency: bool) -> u64 {
        let percent = if emergency {
            self.emergency_target_percent
        } else {
            self.target_percent
        };
        percentage_of(input_budget, percent)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextPolicyError {
    InvalidThresholds,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextBudgetUnavailable {
    UnknownContextWindow,
    ReservedOutputExceedsSafeWindow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextBudget {
    pub context_window: u64,
    pub input_budget: u64,
    pub projected_input: TokenEstimate,
    /// Ceiling percentage; values can exceed 100 when the payload is over budget.
    pub pressure_percent: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextDecision {
    Proceed {
        budget: ContextBudget,
    },
    ScheduleBackgroundCompaction {
        budget: ContextBudget,
    },
    CompactBeforeRequest {
        budget: ContextBudget,
    },
    Unavailable {
        projected_input: TokenEstimate,
        reason: ContextBudgetUnavailable,
    },
}

fn percentage_of(value: u64, percent: u8) -> u64 {
    ((u128::from(value) * u128::from(percent)) / 100).min(u128::from(u64::MAX)) as u64
}

fn ratio_percent_ceil(value: u64, budget: u64) -> u16 {
    let numerator = u128::from(value).saturating_mul(100);
    let rounded =
        numerator.saturating_add(u128::from(budget).saturating_sub(1)) / u128::from(budget);
    rounded.min(u128::from(u16::MAX)) as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy_without_output_reserve() -> ContextPolicy {
        ContextPolicy {
            reserved_output_tokens: 0,
            ..ContextPolicy::default()
        }
    }

    #[test]
    fn defaults_match_the_locked_compaction_policy() {
        let policy = ContextPolicy::default();
        assert_eq!(policy.soft_limit_percent, 70);
        assert_eq!(policy.hard_limit_percent, 85);
        assert_eq!(policy.target_percent, 55);
        assert_eq!(policy.emergency_target_percent, 45);
        assert!(policy.validate().is_ok());
    }

    #[test]
    fn pressure_selects_proceed_background_and_synchronous_actions() {
        let policy = policy_without_output_reserve();
        let context_window = Some(100_000);

        assert!(matches!(
            policy.evaluate(context_window, TokenEstimate::exact(62_000)),
            ContextDecision::Proceed { .. }
        ));
        assert!(matches!(
            policy.evaluate(context_window, TokenEstimate::exact(63_000)),
            ContextDecision::ScheduleBackgroundCompaction { .. }
        ));
        assert!(matches!(
            policy.evaluate(context_window, TokenEstimate::exact(76_500)),
            ContextDecision::CompactBeforeRequest { .. }
        ));
    }

    #[test]
    fn unknown_windows_never_invent_a_budget() {
        assert_eq!(
            ContextPolicy::default().evaluate(None, TokenEstimate::approximate(10_000)),
            ContextDecision::Unavailable {
                projected_input: TokenEstimate::approximate(10_000),
                reason: ContextBudgetUnavailable::UnknownContextWindow,
            }
        );
    }

    #[test]
    fn target_uses_hysteresis_and_emergency_headroom() {
        let policy = ContextPolicy::default();
        assert_eq!(policy.target_tokens(100_000, false), 55_000);
        assert_eq!(policy.target_tokens(100_000, true), 45_000);
    }

    #[test]
    fn invalid_threshold_order_is_rejected() {
        let policy = ContextPolicy {
            target_percent: 75,
            ..ContextPolicy::default()
        };
        assert_eq!(
            policy.validate(),
            Err(ContextPolicyError::InvalidThresholds)
        );
    }
}
