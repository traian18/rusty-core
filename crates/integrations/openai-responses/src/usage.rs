//! OpenAI Responses API usage mapping and cost calculation.
//!
//! # Mapping
//!
//! | Responses field                                   | `ModelUsage` field   |
//! |-----------------------------------------------------|----------------------|
//! | `input_tokens`                                       | `input_tokens`       |
//! | `output_tokens`                                      | `output_tokens`      |
//! | `input_tokens_details.cached_tokens`                 | `cache_read_tokens`  |
//! | *(not exposed)*                                      | `cache_write_tokens: None` |
//! | `output_tokens_details.reasoning_tokens`             | `reasoning_tokens`   |
//! | `total_tokens`                                       | `total_tokens`       |

use serde::Deserialize;

use harness_protocol::usage::{Cost, CostSource, ModelUsage, UsageValue};

#[derive(Debug, Clone, Default, Deserialize)]
pub struct RawOpenAiResponsesUsage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    #[serde(default)]
    pub input_tokens_details: Option<InputTokensDetails>,
    #[serde(default)]
    pub output_tokens_details: Option<OutputTokensDetails>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct InputTokensDetails {
    pub cached_tokens: Option<u64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct OutputTokensDetails {
    pub reasoning_tokens: Option<u64>,
}

pub struct OpenAiResponsesUsageMapper;

impl OpenAiResponsesUsageMapper {
    pub fn map_usage(raw: &RawOpenAiResponsesUsage) -> ModelUsage {
        let input = raw.input_tokens;
        let output = raw.output_tokens;
        let total = raw.total_tokens.or(match (input, output) {
            (Some(i), Some(o)) => Some(i + o),
            _ => None,
        });

        ModelUsage {
            input_tokens: UsageValue::new(input),
            output_tokens: UsageValue::new(output),
            cache_read_tokens: UsageValue::new(
                raw.input_tokens_details
                    .as_ref()
                    .and_then(|d| d.cached_tokens),
            ),
            cache_write_tokens: UsageValue::new(None),
            reasoning_tokens: UsageValue::new(
                raw.output_tokens_details
                    .as_ref()
                    .and_then(|d| d.reasoning_tokens),
            ),
            total_tokens: UsageValue::new(total),
        }
    }

    /// Cost is informational; no per-model rate table is maintained here yet
    /// (gateway providers like OpenCode Zen have their own pricing, distinct
    /// from OpenAI's own), so this always reports a zeroed, calculated cost
    /// rather than a wrong nonzero number.
    pub fn calculate_cost(_usage: &ModelUsage, _model: &str) -> Cost {
        Cost {
            amount_usd: Some(rust_decimal::Decimal::ZERO),
            source: Some(CostSource::Calculated),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_all_fields_correctly() {
        let raw = RawOpenAiResponsesUsage {
            input_tokens: Some(100),
            output_tokens: Some(50),
            total_tokens: Some(150),
            input_tokens_details: Some(InputTokensDetails {
                cached_tokens: Some(10),
            }),
            output_tokens_details: Some(OutputTokensDetails {
                reasoning_tokens: Some(5),
            }),
        };
        let usage = OpenAiResponsesUsageMapper::map_usage(&raw);
        assert_eq!(usage.input_tokens.value(), Some(100));
        assert_eq!(usage.output_tokens.value(), Some(50));
        assert_eq!(usage.cache_read_tokens.value(), Some(10));
        assert_eq!(usage.reasoning_tokens.value(), Some(5));
        assert_eq!(usage.total_tokens.value(), Some(150));
        assert_eq!(usage.cache_write_tokens.value(), None);
    }

    #[test]
    fn total_tokens_falls_back_to_sum_when_missing() {
        let raw = RawOpenAiResponsesUsage {
            input_tokens: Some(10),
            output_tokens: Some(20),
            total_tokens: None,
            ..Default::default()
        };
        let usage = OpenAiResponsesUsageMapper::map_usage(&raw);
        assert_eq!(usage.total_tokens.value(), Some(30));
    }
}
