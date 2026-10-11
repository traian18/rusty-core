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
//! Costs are computed via [`AnthropicUsageMapper::calculate_cost`] using a
//! built-in rate table keyed by model name. Cache read tokens are billed at
//! the input rate; cache write tokens are billed at 1.25× the input rate.
//! The returned [`Cost`] always has `source = CostSource::Calculated`.

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

        let cache_write_cost = usage
            .cache_write_tokens
            .value()
            .map(|t| Decimal::from(t) * rate.input_rate * CACHE_WRITE_MULTIPLIER / PER_MILLION);

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
            input_rate: Decimal::from_parts(300, 0, 0, false, 2), // 3.00
            output_rate: Decimal::from_parts(1500, 0, 0, false, 2), // 15.00
        },
        m if m.starts_with("claude-3-5-haiku-20241022") => ModelRate {
            input_rate: Decimal::from_parts(80, 0, 0, false, 2), // 0.80
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
mod tests;
