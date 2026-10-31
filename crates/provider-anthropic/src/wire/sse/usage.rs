//! Token usage as the stream reports it.

use rusty_protocol::usage::ModelUsage;
use serde::Deserialize;

use crate::usage::{AnthropicUsageMapper, RawAnthropicUsage};

#[derive(Debug, Clone, Default, Deserialize)]
pub(super) struct RawUsage {
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    cache_read_input_tokens: Option<u64>,
    #[serde(alias = "cache_creation_input_tokens")]
    cache_write_input_tokens: Option<u64>,
}

pub(super) fn map_usage(raw: &RawUsage) -> ModelUsage {
    AnthropicUsageMapper::map_usage(
        &RawAnthropicUsage {
            input_tokens: raw.input_tokens,
            output_tokens: raw.output_tokens,
            cache_read_input_tokens: raw.cache_read_input_tokens,
            cache_write_input_tokens: raw.cache_write_input_tokens,
        },
        "",
    )
}

pub(super) fn merge_usage(previous: &ModelUsage, raw: &RawUsage) -> ModelUsage {
    let mapped = map_usage(raw);
    let input_tokens = if mapped.input_tokens.is_unknown() {
        previous.input_tokens
    } else {
        mapped.input_tokens
    };
    let output_tokens = if mapped.output_tokens.is_unknown() {
        previous.output_tokens
    } else {
        mapped.output_tokens
    };
    let total_tokens = input_tokens.checked_add(output_tokens);

    ModelUsage {
        input_tokens,
        output_tokens,
        cache_read_tokens: if mapped.cache_read_tokens.is_unknown() {
            previous.cache_read_tokens
        } else {
            mapped.cache_read_tokens
        },
        cache_write_tokens: if mapped.cache_write_tokens.is_unknown() {
            previous.cache_write_tokens
        } else {
            mapped.cache_write_tokens
        },
        reasoning_tokens: previous.reasoning_tokens,
        total_tokens,
    }
}
