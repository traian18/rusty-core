//! Gemini usage mapping and cost calculation.

use rust_decimal::Decimal;
use serde::Deserialize;

use harness_protocol::usage::{Cost, CostSource, ModelUsage, UsageValue};

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawGeminiUsage {
    pub prompt_token_count: Option<u64>,
    pub candidates_token_count: Option<u64>,
    pub total_token_count: Option<u64>,
}

pub struct GeminiUsageMapper;

impl GeminiUsageMapper {
    pub fn map_usage(raw: &RawGeminiUsage) -> ModelUsage {
        ModelUsage {
            input_tokens: UsageValue::new(raw.prompt_token_count),
            output_tokens: UsageValue::new(raw.candidates_token_count),
            cache_read_tokens: UsageValue::new(None),
            cache_write_tokens: UsageValue::new(None),
            reasoning_tokens: UsageValue::new(None),
            total_tokens: UsageValue::new(raw.total_token_count),
        }
    }

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

fn lookup_rate(model: &str) -> ModelRate {
    match model {
        m if m.starts_with("gemini-1.5-flash") => ModelRate {
            input_rate: Decimal::from_parts(7, 0, 0, false, 2),  // 0.07 (<=128k context tier)
            output_rate: Decimal::from_parts(30, 0, 0, false, 2), // 0.30
        },
        m if m.starts_with("gemini-1.5-pro") => ModelRate {
            input_rate: Decimal::from_parts(125, 0, 0, false, 2), // 1.25
            output_rate: Decimal::from_parts(500, 0, 0, false, 2), // 5.00
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
        let raw = RawGeminiUsage {
            prompt_token_count: Some(100),
            candidates_token_count: Some(50),
            total_token_count: Some(150),
        };
        let usage = GeminiUsageMapper::map_usage(&raw);
        assert_eq!(usage.input_tokens.value(), Some(100));
        assert_eq!(usage.output_tokens.value(), Some(50));
        assert_eq!(usage.total_tokens.value(), Some(150));
    }

    #[test]
    fn unknown_model_yields_zero_cost() {
        let usage = ModelUsage {
            input_tokens: UsageValue::new(Some(1000)),
            output_tokens: UsageValue::new(Some(1000)),
            ..Default::default()
        };
        let cost = GeminiUsageMapper::calculate_cost(&usage, "some-unknown-model");
        assert_eq!(cost.amount_usd, Some(ZERO));
    }

    #[test]
    fn known_model_computes_nonzero_cost() {
        let usage = ModelUsage {
            input_tokens: UsageValue::new(Some(1_000_000)),
            output_tokens: UsageValue::new(Some(1_000_000)),
            ..Default::default()
        };
        let cost = GeminiUsageMapper::calculate_cost(&usage, "gemini-1.5-pro");
        assert_eq!(cost.amount_usd, Some(Decimal::from_parts(625, 0, 0, false, 2)));
    }
}
