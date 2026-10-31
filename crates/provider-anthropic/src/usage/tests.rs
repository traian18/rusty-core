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
