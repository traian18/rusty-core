//! OpenAI usage mapping and cost calculation.
//!
//! # Mapping
//!
//! | OpenAI field                                    | `ModelUsage` field   |
//! |--------------------------------------------------|----------------------|
//! | `prompt_tokens`                                   | `input_tokens`       |
//! | `completion_tokens`                               | `output_tokens`      |
//! | `prompt_tokens_details.cached_tokens`              | `cache_read_tokens`  |
//! | *(not exposed by OpenAI)*                          | `cache_write_tokens: None` |
//! | `completion_tokens_details.reasoning_tokens`       | `reasoning_tokens`   |
//! | `total_tokens`                                     | `total_tokens`       |

use rust_decimal::Decimal;
use serde::Deserialize;

use harness_protocol::usage::{Cost, CostSource, ModelUsage, UsageValue};

#[derive(Debug, Clone, Default, Deserialize)]
pub struct RawOpenAiUsage {
    pub prompt_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    #[serde(default)]
    pub prompt_tokens_details: Option<PromptTokensDetails>,
    #[serde(default)]
    pub completion_tokens_details: Option<CompletionTokensDetails>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct PromptTokensDetails {
    pub cached_tokens: Option<u64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct CompletionTokensDetails {
    pub reasoning_tokens: Option<u64>,
}

pub struct OpenAiUsageMapper;

impl OpenAiUsageMapper {
    pub fn map_usage(raw: &RawOpenAiUsage) -> ModelUsage {
        let input = raw.prompt_tokens;
        let output = raw.completion_tokens;
        let total = raw.total_tokens.or(match (input, output) {
            (Some(i), Some(o)) => Some(i + o),
            _ => None,
        });

        ModelUsage {
            input_tokens: UsageValue::new(input),
            output_tokens: UsageValue::new(output),
            cache_read_tokens: UsageValue::new(
                raw.prompt_tokens_details.as_ref().and_then(|d| d.cached_tokens),
            ),
            cache_write_tokens: UsageValue::new(None),
            reasoning_tokens: UsageValue::new(
                raw.completion_tokens_details.as_ref().and_then(|d| d.reasoning_tokens),
            ),
            total_tokens: UsageValue::new(total),
        }
    }

    /// Compute the cost of a usage record for the given model, using a
    /// built-in per-model rate table. Unknown models yield zero rates, so
    /// the computed cost will be `$0.00`.
    pub fn calculate_cost(usage: &ModelUsage, model: &str) -> Cost {
        let rate = lookup_rate(model);

        let input_cost = usage
            .input_tokens
            .value()
            .map(|t| Decimal::from(t) * rate.input_rate / PER_MILLION);
        let output_cost = usage
            .output_tokens
            .value()
            .map(|t| Decimal::from(t) * rate.output_rate / PER_MILLION);

        let total = [input_cost, output_cost]
            .iter()
            .fold(None, |acc: Option<Decimal>, cost| match (acc, cost) {
                (Some(a), Some(c)) => Some(a + c),
                (None, Some(c)) => Some(*c),
                (a, None) => a,
            });

        Cost {
            amount_usd: total,
            source: Some(CostSource::Calculated),
        }
    }
}

const PER_MILLION: Decimal = Decimal::from_parts(1_000_000, 0, 0, false, 0);
const ZERO: Decimal = Decimal::from_parts(0, 0, 0, false, 0);

struct ModelRate {
    input_rate: Decimal,
    output_rate: Decimal,
}

/// Looks up pricing rates for a given model name via prefix matching.
/// Returns zero rates for unrecognized models.
fn lookup_rate(model: &str) -> ModelRate {
    match model {
        m if m.starts_with("gpt-4o-mini") => ModelRate {
            input_rate: Decimal::from_parts(15, 0, 0, false, 2),  // 0.15
            output_rate: Decimal::from_parts(60, 0, 0, false, 2), // 0.60
        },
        m if m.starts_with("gpt-4o") => ModelRate {
            input_rate: Decimal::from_parts(250, 0, 0, false, 2),  // 2.50
            output_rate: Decimal::from_parts(1000, 0, 0, false, 2), // 10.00
        },
        m if m.starts_with("gpt-4-turbo") => ModelRate {
            input_rate: Decimal::from_parts(1000, 0, 0, false, 2), // 10.00
            output_rate: Decimal::from_parts(3000, 0, 0, false, 2), // 30.00
        },
        _ => ModelRate {
            input_rate: ZERO,
            output_rate: ZERO,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_all_fields_correctly() {
        let raw = RawOpenAiUsage {
            prompt_tokens: Some(100),
            completion_tokens: Some(50),
            total_tokens: Some(150),
            prompt_tokens_details: Some(PromptTokensDetails { cached_tokens: Some(10) }),
            completion_tokens_details: Some(CompletionTokensDetails { reasoning_tokens: Some(5) }),
        };
        let usage = OpenAiUsageMapper::map_usage(&raw);
        assert_eq!(usage.input_tokens.value(), Some(100));
        assert_eq!(usage.output_tokens.value(), Some(50));
        assert_eq!(usage.cache_read_tokens.value(), Some(10));
        assert_eq!(usage.reasoning_tokens.value(), Some(5));
        assert_eq!(usage.total_tokens.value(), Some(150));
        assert_eq!(usage.cache_write_tokens.value(), None);
    }

    #[test]
    fn total_tokens_falls_back_to_sum_when_missing() {
        let raw = RawOpenAiUsage {
            prompt_tokens: Some(10),
            completion_tokens: Some(20),
            total_tokens: None,
            ..Default::default()
        };
        let usage = OpenAiUsageMapper::map_usage(&raw);
        assert_eq!(usage.total_tokens.value(), Some(30));
    }

    #[test]
    fn unknown_model_yields_zero_cost() {
        let usage = ModelUsage {
            input_tokens: UsageValue::new(Some(1000)),
            output_tokens: UsageValue::new(Some(1000)),
            ..Default::default()
        };
        let cost = OpenAiUsageMapper::calculate_cost(&usage, "some-unknown-model");
        assert_eq!(cost.amount_usd, Some(ZERO));
    }

    #[test]
    fn known_model_computes_nonzero_cost() {
        let usage = ModelUsage {
            input_tokens: UsageValue::new(Some(1_000_000)),
            output_tokens: UsageValue::new(Some(1_000_000)),
            ..Default::default()
        };
        let cost = OpenAiUsageMapper::calculate_cost(&usage, "gpt-4o");
        assert_eq!(cost.amount_usd, Some(Decimal::from_parts(1250, 0, 0, false, 2)));
    }
}
