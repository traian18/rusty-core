use super::{
    response::{parse_max_tokens_ceiling, retry_after_from_headers},
    AnthropicClient,
};
use crate::config::AnthropicConfig;
use rusty_model_backend::request::ModelRequest;
use rusty_protocol::backend::ReasoningEffort;
use std::time::Duration;

#[test]
fn retry_after_ms_is_normalized_to_duration() {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert("retry-after-ms", "1250".parse().unwrap());
    assert_eq!(
        retry_after_from_headers(&headers),
        Some(Duration::from_millis(1250))
    );
}

#[test]
fn standard_retry_after_uses_seconds() {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(reqwest::header::RETRY_AFTER, "3".parse().unwrap());
    assert_eq!(
        retry_after_from_headers(&headers),
        Some(Duration::from_secs(3))
    );
}

/// The exact body reported live against `claude-sonnet-4-5-20250929`.
#[test]
fn parses_the_ceiling_out_of_anthropics_own_error_format() {
    let body = r#"{"type":"error","error":{"type":"invalid_request_error","message":"max_tokens: 128000 > 64000, which is the maximum allowed number of output tokens for claude-sonnet-4-5-20250929"}}"#;
    assert_eq!(parse_max_tokens_ceiling(body), Some(64_000));
}

/// The same message, relayed inside a gateway's own wrapper -- OpenCode
/// Zen's `anthropic`-family route reports it exactly this way.
#[test]
fn parses_the_ceiling_out_of_a_gateways_relayed_wrapper() {
    let body = r#"{"type":"error","error":{"type":"invalid_request_error","message":"Upstream request failed: [invalid_request_error] max_tokens: 128000 > 64000, which is the maximum allowed number of output tokens for claude-sonnet-4-5-20250929"}}"#;
    assert_eq!(parse_max_tokens_ceiling(body), Some(64_000));
}

#[test]
fn returns_none_for_an_unrelated_error() {
    let body =
        r#"{"type":"error","error":{"type":"authentication_error","message":"invalid x-api-key"}}"#;
    assert_eq!(parse_max_tokens_ceiling(body), None);
}

#[test]
fn returns_none_for_malformed_text_after_the_marker() {
    assert_eq!(
        parse_max_tokens_ceiling("max_tokens: not a number here"),
        None
    );
    assert_eq!(
        parse_max_tokens_ceiling("max_tokens: 128000, no comparison"),
        None
    );
    assert_eq!(parse_max_tokens_ceiling(""), None);
}

fn client() -> AnthropicClient {
    AnthropicClient::new(AnthropicConfig::default())
}

fn request(max_tokens: u64, reasoning_effort: Option<ReasoningEffort>) -> ModelRequest {
    ModelRequest {
        system_prompt: String::new(),
        messages: Vec::new(),
        tools: Vec::new(),
        model: Some("claude-sonnet-4-5-20250929".to_string()),
        max_tokens: Some(max_tokens),
        temperature: None,
        stop_sequences: Vec::new(),
        extended_thinking: false,
        reasoning_effort,
        response_format: None,
        provider_options: serde_json::Value::Null,
        cache_key: None,
    }
}

/// The bug this whole mechanism exists to avoid: rebuilding the request
/// with a corrected `max_tokens` must also correct `thinking.budget_tokens`
/// (`resolve_thinking`'s own budget is a fraction of `max_tokens`), or a
/// self-corrected retry could still be rejected -- this time for a
/// thinking budget that no longer fits under the new, lower `max_tokens`.
#[test]
fn build_body_rederives_the_thinking_budget_for_the_corrected_max_tokens() {
    let client = client();
    let high_budget = client
        .build_body(
            &request(128_000, Some(ReasoningEffort::High)),
            128_000,
            false,
        )
        .expect("build_body succeeds");
    let corrected_budget = client
        .build_body(
            &request(128_000, Some(ReasoningEffort::High)),
            64_000,
            false,
        )
        .expect("build_body succeeds after correction");

    assert_eq!(high_budget["max_tokens"], 128_000);
    assert_eq!(corrected_budget["max_tokens"], 64_000);

    let high_tokens = high_budget["thinking"]["budget_tokens"]
        .as_u64()
        .expect("thinking budget must be present for a reasoning request");
    let corrected_tokens = corrected_budget["thinking"]["budget_tokens"]
        .as_u64()
        .expect("thinking budget must be present for a reasoning request");
    assert!(
        corrected_tokens < high_tokens,
        "budget must shrink with max_tokens, not stay pinned to the rejected value \
         (high={high_tokens}, corrected={corrected_tokens})"
    );
    assert!(
        corrected_tokens < 64_000,
        "budget must fit under the corrected max_tokens: {corrected_tokens}"
    );
}

#[test]
fn build_body_sends_no_thinking_field_when_no_reasoning_was_requested() {
    let client = client();
    let body = client
        .build_body(&request(64_000, None), 64_000, false)
        .expect("build_body succeeds");
    assert!(body.get("thinking").is_none());
    assert!(body.get("output_config").is_none());
}

#[test]
fn build_body_sends_adaptive_thinking_with_an_effort_instead_of_a_budget() {
    let client = client();
    let body = client
        .build_body(&request(64_000, Some(ReasoningEffort::Low)), 64_000, true)
        .expect("build_body succeeds");
    assert_eq!(body["thinking"], serde_json::json!({"type": "adaptive"}));
    assert_eq!(body["output_config"], serde_json::json!({"effort": "low"}));
}

#[test]
fn adaptive_only_models_use_adaptive_thinking_without_a_failed_attempt() {
    let client = client();
    assert!(client.uses_adaptive_thinking("claude-opus-4.7"));
    assert!(!client.uses_adaptive_thinking("claude-sonnet-4.5"));
}
