//! Merging provider-specific request overrides into a wire request body.
//!
//! [`ModelRequest::provider_options`](crate::ModelRequest::provider_options)
//! is namespaced by provider id (e.g. `{"anthropic": {"top_k": 40}}`) so a
//! caller can pass advanced/experimental knobs a client doesn't have a
//! typed field for without coupling `ExecutionParams` to any one provider's
//! wire shape. [`merge_provider_options`] is the single place that
//! namespace lookup and merge happens, shared by every client so the
//! behavior (only same-provider keys apply, unknown/malformed shapes are
//! ignored rather than rejected, typed fields the client already set always
//! win) is identical everywhere instead of subtly reimplemented per client.

use serde_json::Value;

/// Merges `provider_options[provider_id]` (if present and an object) into
/// `body` (which must itself already be an object — every wire request type
/// in this workspace serializes to one).
///
/// A typed field the client already populated on `body` always wins over an
/// identically-named key in `provider_options`: this function is meant for
/// knobs the client has *no* typed field for, not a way to override ones it
/// does. Missing namespace, a non-object body, or a non-object namespace
/// value are all silently no-ops — provider options are explicitly
/// best-effort ("providers that don't recognize their namespace's contents
/// ignore it rather than erroring", per `ExecutionParams::provider_options`'s
/// doc comment), and a client should never fail a request just because the
/// caller sent an oddly-shaped override.
pub fn merge_provider_options(
    mut body: Value,
    provider_options: &Value,
    provider_id: &str,
) -> Value {
    let Some(overrides) = provider_options.get(provider_id).and_then(Value::as_object) else {
        return body;
    };
    let Some(existing) = body.as_object_mut() else {
        return body;
    };
    for (key, value) in overrides {
        existing.entry(key.clone()).or_insert_with(|| value.clone());
    }
    body
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn matching_namespace_merges_into_the_body() {
        let body = json!({"model": "claude-opus-4", "max_tokens": 1024});
        let provider_options = json!({"anthropic": {"top_k": 40}});
        let merged = merge_provider_options(body, &provider_options, "anthropic");
        assert_eq!(merged["top_k"], 40);
        assert_eq!(merged["model"], "claude-opus-4");
    }

    #[test]
    fn a_different_providers_namespace_is_ignored() {
        let body = json!({"model": "claude-opus-4"});
        let provider_options = json!({"openai": {"top_k": 40}});
        let merged = merge_provider_options(body.clone(), &provider_options, "anthropic");
        assert_eq!(merged, body);
    }

    #[test]
    fn an_already_typed_field_is_never_overridden() {
        let body = json!({"temperature": 0.7});
        let provider_options = json!({"anthropic": {"temperature": 0.0}});
        let merged = merge_provider_options(body, &provider_options, "anthropic");
        assert_eq!(
            merged["temperature"], 0.7,
            "a typed field the client set must win over provider_options"
        );
    }

    #[test]
    fn null_provider_options_is_a_no_op() {
        let body = json!({"model": "gpt-5"});
        let merged = merge_provider_options(body.clone(), &Value::Null, "openai");
        assert_eq!(merged, body);
    }

    #[test]
    fn a_non_object_namespace_value_is_a_no_op() {
        let body = json!({"model": "gpt-5"});
        let provider_options = json!({"openai": "not an object"});
        let merged = merge_provider_options(body.clone(), &provider_options, "openai");
        assert_eq!(merged, body);
    }
}
