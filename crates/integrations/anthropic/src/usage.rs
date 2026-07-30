//! Anthropic usage mapping and cost calculation.
//!
//! This module provides [`AnthropicUsageMapper`] for converting raw usage data
//! from the Anthropic Messages API into harness-protocol [`ModelUsage`] and
//! computing [`Cost`] from a per-model rate table.
//!
//! # Mapping
//!
//! | Anthropic field                     | `ModelUsage` field        |
//! |-------------------------------------|---------------------------|
//! | `input_tokens`                      | `input_tokens`            |
//! | `output_tokens`                     | `output_tokens`           |
//! | `cache_read_input_tokens`           | `cache_read_tokens`       |
//! | `cache_write_input_tokens`          | `cache_write_tokens`      |
//! | *(not exposed by Anthropic)*        | `reasoning_tokens: None`  |
//! | `input_tokens + output_tokens`      | `total_tokens`            |
//!
//! # Cost calculation
//!
//! Costs are computed via [`calculate_cost`] using a built-in rate table keyed
//! by model name. Cache read tokens are billed at the input rate; cache write
//! tokens are billed at 1.25× the input rate. The returned [`Cost`] always has
//! `source = CostSource::Calculated`.

use rust_decimal::Decimal;

use harness_protocol::usage::{Cost, CostSource, ModelUsage, UsageValue};

// ---------------------------------------------------------------------------
// RawAnthropicUsage
// ---------------------------------------------------------------------------

/// Raw usage data reported by the Anthropic Messages API.
///
/// This struct mirrors the `usage` object in an Anthropic API response
/// (`message_start` or `message_delta` payloads).
#[derive(Debug, Clone, Default)]
pub struct RawAnthropicUsage {
    /// Number of input tokens consumed.
    pub input_tokens: Option<u64>,
    /// Number of output tokens generated.
    pub output_tokens: Option<u64>,
    /// Tokens read from the prompt cache.
    pub cache_read_input_tokens: Option<u64>,
    /// Tokens written to the prompt cache.
    pub cache_write_input_tokens: Option<u64>,
}

// ---------------------------------------------------------------------------
// AnthropicUsageMapper
// ---------------------------------------------------------------------------

/// Maps raw Anthropic usage data into harness-protocol [`ModelUsage`] and
/// computes [`Cost`] from a per-model rate table.
///
/// # Rate table
///
/// The following models are currently recognised (rates are USD per million
/// tokens):
///
/// | Model                              | Input $/M | Output $/M |
/// |------------------------------------|-----------|------------|
/// | `claude-sonnet-4-20250513`         | $3.00     | $15.00     |
/// | `claude-3-5-haiku-20241022`        | $0.80     | $4.00      |
///
/// Unknown models yield zero rates, so the computed cost will be `$0.00`.
pub struct AnthropicUsageMapper;

impl AnthropicUsageMapper {
    /// Map raw Anthropic usage fields into a [`ModelUsage`].
    ///
    /// This performs the field mapping documented on the module and computes
    /// `total_tokens = input_tokens + output_tokens` when both are known.
    /// `reasoning_tokens` is always set to `None` (Anthropic does not expose
    /// this field).
    pub fn map_usage(raw: &RawAnthropicUsage, _model: &str) -> ModelUsage {
        let input = raw.input_tokens;
        let output = raw.output_tokens;
        let total = match (input, output) {
            (Some(i), Some(o)) => Some(i + o),
            _ => None,
        };

        ModelUsage {
            input_tokens: UsageValue::new(input),
            output_tokens: UsageValue::new(output),
            cache_read_tokens: UsageValue::new(raw.cache_read_input_tokens),
            cache_write_tokens: UsageValue::new(raw.cache_write_input_tokens),
            reasoning_tokens: UsageValue::new(None),
            total_tokens: UsageValue::new(total),
        }
    }

    /// Compute the cost of a usage record for the given model.
    ///
    /// Pricing is looked up from the built-in rate table (see [`Self`] for
    /// recognised models). The returned [`Cost`] has:
    ///
    /// * `amount_usd` — the computed total (input + output + cache) in USD, or
    ///   `None` if no token counts are available.
    /// * `source` — `Some(CostSource::Calculated)`.
    ///
    /// Cache read tokens are billed at the input rate. Cache write tokens are
    /// billed at 1.25× the input rate (Anthropic's pricing).
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

        let cache_read_cost = usage
            .cache_read_tokens
            .value()
            .map(|t| Decimal::from(t) * rate.input_rate / PER_MILLION);

        let cache_write_cost = usage.cache_write_tokens.value().map(|t| {
            Decimal::from(t) * rate.input_rate * CACHE_WRITE_MULTIPLIER / PER_MILLION
        });

        let total = [input_cost, output_cost, cache_read_cost, cache_write_cost]
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

// ---------------------------------------------------------------------------
// Rate table constants
// ---------------------------------------------------------------------------

/// One million — used to convert per-million-token rates to per-token costs.
const PER_MILLION: Decimal = Decimal::from_parts(1_000_000, 0, 0, false, 0);

/// Multiplier for cache write tokens (1.25× the input rate).
const CACHE_WRITE_MULTIPLIER: Decimal = Decimal::from_parts(125, 0, 0, false, 2); // 1.25

/// A zero-valued [`Decimal`] constant, used as a fallback for unknown models.
const ZERO: Decimal = Decimal::from_parts(0, 0, 0, false, 0);

/// Pricing information for a model.
struct ModelRate {
    /// Cost per million input tokens (USD).
    input_rate: Decimal,
    /// Cost per million output tokens (USD).
    output_rate: Decimal,
}

/// Look up pricing rates for a given model name.
///
/// Uses prefix matching so that `"claude-sonnet-4-20250513"` matches exactly,
/// and future point-releases starting with the same prefix would also match.
///
/// Returns zero rates for unknown models.
fn lookup_rate(model: &str) -> ModelRate {
    match model {
        m if m.starts_with("claude-sonnet-4-20250513") => ModelRate {
            input_rate: Decimal::from_parts(300, 0, 0, false, 2),   // 3.00
            output_rate: Decimal::from_parts(1500, 0, 0, false, 2), // 15.00
        },
        m if m.starts_with("claude-3-5-haiku-20241022") => ModelRate {
            input_rate: Decimal::from_parts(80, 0, 0, false, 2),  // 0.80
            output_rate: Decimal::from_parts(400, 0, 0, false, 2), // 4.00
        },
        _ => ModelRate {
            input_rate: ZERO,
            output_rate: ZERO,
        },
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ------------------------------------------------------------------
    // map_usage tests
    // ------------------------------------------------------------------

    #[test]
    fn maps_all_fields_correctly() {
        let raw = RawAnthropicUsage {
            input_tokens: Some(100),
            output_tokens: Some(50),
            cache_read_input_tokens: Some(10),
            cache_write_input_tokens: Some(5),
        };

        let usage = AnthropicUsageMapper::map_usage(&raw, "claude-sonnet-4-20250513");

        assert_eq!(usage.input_tokens.value(), Some(100));
        assert_eq!(usage.output_tokens.value(), Some(50));
        assert_eq!(usage.cache_read_tokens.value(), Some(10));
        assert_eq!(usage.cache_write_tokens.value(), Some(5));
        assert_eq!(usage.reasoning_tokens.value(), None);
        assert_eq!(usage.total_tokens.value(), Some(150));
    }

    #[test]
    fn total_tokens_is_none_when_input_or_output_unknown() {
        // Both unknown
        let raw = RawAnthropicUsage {
            input_tokens: None,
            output_tokens: None,
            ..Default::default()
        };
        let usage = AnthropicUsageMapper::map_usage(&raw, "claude-sonnet-4-20250513");
        assert_eq!(usage.total_tokens.value(), None);

        // Only input known
        let raw = RawAnthropicUsage {
            input_tokens: Some(10),
            output_tokens: None,
            ..Default::default()
        };
        let usage = AnthropicUsageMapper::map_usage(&raw, "claude-sonnet-4-20250513");
        assert_eq!(usage.total_tokens.value(), None);

        // Only output known
        let raw = RawAnthropicUsage {
            input_tokens: None,
            output_tokens: Some(10),
            ..Default::default()
        };
        let usage = AnthropicUsageMapper::map_usage(&raw, "claude-sonnet-4-20250513");
        assert_eq!(usage.total_tokens.value(), None);
    }

    #[test]
    fn reasoning_tokens_always_none() {
        let raw = RawAnthropicUsage {
            input_tokens: Some(10),
            output_tokens: Some(10),
            ..Default::default()
        };
        let usage = AnthropicUsageMapper::map_usage(&raw, "claude-sonnet-4-20250513");
        assert_eq!(usage.reasoning_tokens.value(), None);
    }

    #[test]
    fn cache_fields_are_none_when_not_present() {
        let raw = RawAnthropicUsage {
            input_tokens: Some(10),
            output_tokens: Some(10),
            cache_read_input_tokens: None,
            cache_write_input_tokens: None,
        };
        let usage = AnthropicUsageMapper::map_usage(&raw, "claude-sonnet-4-20250513");
        assert_eq!(usage.cache_read_tokens.value(), None);
        assert_eq!(usage.cache_write_tokens.value(), None);
    }

    #[test]
    fn model_string_is_not_used_for_mapping() {
        // The model parameter is ignored by map_usage but accepted for
        // API consistency with calculate_cost.
        let raw = RawAnthropicUsage {
            input_tokens: Some(42),
            output_tokens: Some(24),
            ..Default::default()
        };
        let usage = AnthropicUsageMapper::map_usage(&raw, "unknown-model-xyz");
        assert_eq!(usage.input_tokens.value(), Some(42));
        assert_eq!(usage.output_tokens.value(), Some(24));
    }

    // ------------------------------------------------------------------
    // calculate_cost tests
    // ------------------------------------------------------------------

    #[test]
    fn cost_for_claude_sonnet_4() {
        // 1M input at $3.00/M = $3.00
        // 500K output at $15.00/M = $7.50
        // Total = $10.50
        let raw = RawAnthropicUsage {
            input_tokens: Some(1_000_000),
            output_tokens: Some(500_000),
            cache_read_input_tokens: None,
            cache_write_input_tokens: None,
        };
        let usage = AnthropicUsageMapper::map_usage(&raw, "claude-sonnet-4-20250513");
        let cost = AnthropicUsageMapper::calculate_cost(&usage, "claude-sonnet-4-20250513");

        assert_eq!(cost.source, Some(CostSource::Calculated));
        let amount = cost.amount_usd.expect("cost should be Some");
        // $3.00 + $7.50 = $10.50
        assert_eq!(amount.to_string(), "10.50");
    }

    #[test]
    fn cost_for_claude_3_5_haiku() {
        // 2M input at $0.80/M = $1.60
        // 1M output at $4.00/M = $4.00
        // Total = $5.60
        let raw = RawAnthropicUsage {
            input_tokens: Some(2_000_000),
            output_tokens: Some(1_000_000),
            cache_read_input_tokens: None,
            cache_write_input_tokens: None,
        };
        let usage = AnthropicUsageMapper::map_usage(&raw, "claude-3-5-haiku-20241022");
        let cost = AnthropicUsageMapper::calculate_cost(&usage, "claude-3-5-haiku-20241022");

        assert_eq!(cost.source, Some(CostSource::Calculated));
        let amount = cost.amount_usd.expect("cost should be Some");
        // $1.60 + $4.00 = $5.60
        assert_eq!(amount.to_string(), "5.60");
    }

    #[test]
    fn cost_with_cache_read_hit() {
        // 1M input at $3.00/M = $3.00
        // 200K output at $15.00/M = $3.00
        // 100K cache read at $3.00/M (input rate) = $0.30
        // Total = $6.30
        let raw = RawAnthropicUsage {
            input_tokens: Some(1_000_000),
            output_tokens: Some(200_000),
            cache_read_input_tokens: Some(100_000),
            cache_write_input_tokens: None,
        };
        let usage = AnthropicUsageMapper::map_usage(&raw, "claude-sonnet-4-20250513");
        let cost = AnthropicUsageMapper::calculate_cost(&usage, "claude-sonnet-4-20250513");

        assert_eq!(cost.source, Some(CostSource::Calculated));
        let amount = cost.amount_usd.expect("cost should be Some");
        // $3.00 + $3.00 + $0.30 = $6.30
        assert_eq!(amount.to_string(), "6.30");
    }

    #[test]
    fn cost_with_cache_write() {
        // 1M input at $3.00/M = $3.00
        // 50K cache write at $3.00/M × 1.25 = $3.75/M → 50K × $3.75/1M = $0.1875
        // Total ≈ $3.1875
        let raw = RawAnthropicUsage {
            input_tokens: Some(1_000_000),
            output_tokens: None,
            cache_read_input_tokens: None,
            cache_write_input_tokens: Some(50_000),
        };
        let usage = AnthropicUsageMapper::map_usage(&raw, "claude-sonnet-4-20250513");
        let cost = AnthropicUsageMapper::calculate_cost(&usage, "claude-sonnet-4-20250513");

        assert_eq!(cost.source, Some(CostSource::Calculated));
        let amount = cost.amount_usd.expect("cost should be Some");
        // $3.00 + $0.1875 = $3.1875
        assert_eq!(amount.to_string(), "3.1875");
    }

    #[test]
    fn cost_with_cache_read_and_write_full_scenario() {
        // Sonnet-4:
        // 500K input × $3.00/M = $1.50
        // 300K output × $15.00/M = $4.50
        // 200K cache read × $3.00/M = $0.60
        // 10K cache write × $3.00/M × 1.25 = $3.75/M → $0.0375
        // Total = $6.6375
        let raw = RawAnthropicUsage {
            input_tokens: Some(500_000),
            output_tokens: Some(300_000),
            cache_read_input_tokens: Some(200_000),
            cache_write_input_tokens: Some(10_000),
        };
        let usage = AnthropicUsageMapper::map_usage(&raw, "claude-sonnet-4-20250513");
        let cost = AnthropicUsageMapper::calculate_cost(&usage, "claude-sonnet-4-20250513");

        assert_eq!(cost.source, Some(CostSource::Calculated));
        let amount = cost.amount_usd.expect("cost should be Some");
        assert_eq!(amount.to_string(), "6.6375");
    }

    #[test]
    fn cost_zero_for_unknown_model() {
        let raw = RawAnthropicUsage {
            input_tokens: Some(1_000_000),
            output_tokens: Some(1_000_000),
            ..Default::default()
        };
        let usage = AnthropicUsageMapper::map_usage(&raw, "claude-opus-4-unknown");
        let cost = AnthropicUsageMapper::calculate_cost(&usage, "claude-opus-4-unknown");

        assert_eq!(cost.source, Some(CostSource::Calculated));
        let amount = cost.amount_usd.expect("cost should be Some(0)");
        assert_eq!(amount.to_string(), "0");
    }

    #[test]
    fn cost_none_when_no_tokens() {
        let raw = RawAnthropicUsage {
            input_tokens: None,
            output_tokens: None,
            ..Default::default()
        };
        let usage = AnthropicUsageMapper::map_usage(&raw, "claude-sonnet-4-20250513");
        let cost = AnthropicUsageMapper::calculate_cost(&usage, "claude-sonnet-4-20250513");

        assert_eq!(cost.source, Some(CostSource::Calculated));
        // When all token counts are None, there are no costs to sum,
        // so amount_usd should be None.
        assert_eq!(cost.amount_usd, None);
    }

    #[test]
    fn cost_with_haiku_and_cache_hit() {
        // Haiku:
        // 1M input at $0.80/M = $0.80
        // 500K output at $4.00/M = $2.00
        // 50K cache read at $0.80/M = $0.04
        // Total = $2.84
        let raw = RawAnthropicUsage {
            input_tokens: Some(1_000_000),
            output_tokens: Some(500_000),
            cache_read_input_tokens: Some(50_000),
            cache_write_input_tokens: None,
        };
        let usage = AnthropicUsageMapper::map_usage(&raw, "claude-3-5-haiku-20241022");
        let cost = AnthropicUsageMapper::calculate_cost(&usage, "claude-3-5-haiku-20241022");

        assert_eq!(cost.source, Some(CostSource::Calculated));
        let amount = cost.amount_usd.expect("cost should be Some");
        assert_eq!(amount.to_string(), "2.84");
    }

    #[test]
    fn prefix_matching_matches_longer_model_strings() {
        // Model strings may include version suffixes or region identifiers.
        // Prefix matching should still work.
        let raw = RawAnthropicUsage {
            input_tokens: Some(1_000_000),
            output_tokens: None,
            ..Default::default()
        };
        let usage = AnthropicUsageMapper::map_usage(&raw, "claude-sonnet-4-20250513-v1");
        let cost = AnthropicUsageMapper::calculate_cost(&usage, "claude-sonnet-4-20250513-v1");
        assert_eq!(
            cost.amount_usd.unwrap().to_string(),
            "3.00",
            "prefix matching should still match extended model strings"
        );
    }

    // ------------------------------------------------------------------
    // Decimal constant correctness
    // ------------------------------------------------------------------

    #[test]
    fn per_million_constant_is_correct() {
        assert_eq!(PER_MILLION.to_string(), "1000000");
    }

    #[test]
    fn cache_write_multiplier_is_1_dot_25() {
        assert_eq!(CACHE_WRITE_MULTIPLIER.to_string(), "1.25");
    }
}
