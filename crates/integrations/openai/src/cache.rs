//! Prompt-cache hints for Chat Completions, per destination. Each provider
//! behind this wire format caches differently, and an unknown field can be
//! rejected, so a hint is only sent where it is understood:
//! - OpenAI caches automatically; `prompt_cache_key` keeps one run's calls on
//!   the same cache.
//! - OpenRouter caches OpenAI, DeepSeek and recent Gemini models by itself,
//!   but Claude (and explicit Gemini caching) only up to content parts
//!   marked with `cache_control`: the system prompt and the last message.
//! - GitHub Copilot's Chat Completions route caches Claude models up to
//!   messages marked with `copilot_cache_control`, as its own clients send it.
//!   (Copilot's Messages route uses the Anthropic client's marks, and its
//!   Responses route sends `prompt_cache_key`.)
//! - Anything else (Groq, Together, a local Ollama, vLLM or llama.cpp server)
//!   gets no hint: local servers reuse their KV cache for a stable prefix on
//!   their own.

use serde_json::{json, Value};

use crate::config::PromptCache;

#[derive(Debug, PartialEq, Eq)]
enum Destination {
    OpenAi,
    OpenRouter,
    Copilot,
    Other,
}

fn destination(style: PromptCache, base_url: &str) -> Destination {
    match style {
        PromptCache::Copilot => return Destination::Copilot,
        PromptCache::Off => return Destination::Other,
        PromptCache::ByHost => {}
    }
    let host = reqwest::Url::parse(base_url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned))
        .unwrap_or_default();
    match host.as_str() {
        "api.openai.com" => Destination::OpenAi,
        "openrouter.ai" => Destination::OpenRouter,
        "api.githubcopilot.com" => Destination::Copilot,
        other if other.starts_with("copilot-api.") => Destination::Copilot,
        _ => Destination::Other,
    }
}

/// Adds the hints the destination understands to a serialized request.
pub fn add_cache_hints(
    body: &mut Value,
    style: PromptCache,
    base_url: &str,
    cache_key: Option<&str>,
) {
    let model = body["model"].as_str().unwrap_or("").to_ascii_lowercase();
    match destination(style, base_url) {
        Destination::OpenAi => {
            if let (Some(key), Some(fields)) = (cache_key, body.as_object_mut()) {
                fields
                    .entry("prompt_cache_key")
                    .or_insert_with(|| key.into());
            }
        }
        Destination::OpenRouter
            if model.starts_with("anthropic/") || model.starts_with("google/") =>
        {
            for message in marked_messages(body) {
                mark_content(message);
            }
        }
        Destination::Copilot if model.contains("claude") => {
            for message in marked_messages(body) {
                if let Some(fields) = message.as_object_mut() {
                    fields
                        .entry("copilot_cache_control")
                        .or_insert_with(|| json!({ "type": "ephemeral" }));
                }
            }
        }
        _ => {}
    }
}

/// The system message and the last message with content.
fn marked_messages(body: &mut Value) -> Vec<&mut Value> {
    let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) else {
        return Vec::new();
    };
    let last = messages
        .iter()
        .rposition(has_content)
        .filter(|&at| messages[at]["role"] != "system");
    messages
        .iter_mut()
        .enumerate()
        .filter(|(at, message)| (message["role"] == "system" && *at == 0) || Some(*at) == last)
        .map(|(_, message)| message)
        .collect()
}

fn has_content(message: &Value) -> bool {
    match &message["content"] {
        Value::String(text) => !text.is_empty(),
        Value::Array(parts) => !parts.is_empty(),
        _ => false,
    }
}

/// Marks the last text part, turning plain text content into one part.
fn mark_content(message: &mut Value) {
    let ephemeral = json!({ "type": "ephemeral" });
    if let Some(text) = message["content"].as_str().map(str::to_owned) {
        message["content"] = json!([{ "type": "text", "text": text, "cache_control": ephemeral }]);
        return;
    }
    if let Some(part) = message["content"]
        .as_array_mut()
        .and_then(|parts| parts.iter_mut().rev().find(|part| part["type"] == "text"))
        .and_then(Value::as_object_mut)
    {
        part.entry("cache_control").or_insert(ephemeral);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(model: &str) -> Value {
        json!({
            "model": model,
            "messages": [
                {"role": "system", "content": "You are Build."},
                {"role": "user", "content": "task"},
                {"role": "assistant", "content": null, "tool_calls": [{"id": "1"}]},
                {"role": "tool", "tool_call_id": "1", "content": "file text"}
            ]
        })
    }

    #[test]
    fn openai_gets_the_runs_cache_key() {
        let mut request = body("gpt-4.1");
        add_cache_hints(
            &mut request,
            PromptCache::ByHost,
            "https://api.openai.com/v1",
            Some("run-1"),
        );
        assert_eq!(request["prompt_cache_key"], "run-1");
        assert!(!request.to_string().contains("cache_control"));
    }

    #[test]
    fn openrouter_marks_claude_and_gemini_content() {
        for model in ["anthropic/claude-sonnet-4.5", "google/gemini-2.5-pro"] {
            let mut request = body(model);
            add_cache_hints(
                &mut request,
                PromptCache::ByHost,
                "https://openrouter.ai/api/v1",
                Some("run-1"),
            );
            assert_eq!(
                request["messages"][0]["content"][0]["cache_control"]["type"], "ephemeral",
                "{model}"
            );
            assert_eq!(
                request["messages"][0]["content"][0]["text"],
                "You are Build."
            );
            assert_eq!(
                request["messages"][3]["content"][0]["cache_control"]["type"],
                "ephemeral"
            );
            assert_eq!(
                request["messages"][1]["content"], "task",
                "only the system prompt and the last message"
            );
            assert!(request.get("prompt_cache_key").is_none());
        }
        let mut gpt = body("openai/gpt-4.1");
        add_cache_hints(
            &mut gpt,
            PromptCache::ByHost,
            "https://openrouter.ai/api/v1",
            Some("run-1"),
        );
        assert_eq!(
            gpt,
            body("openai/gpt-4.1"),
            "OpenRouter caches OpenAI models by itself"
        );
    }

    #[test]
    fn copilot_marks_claude_messages() {
        let mut request = body("claude-sonnet-4.5");
        add_cache_hints(
            &mut request,
            PromptCache::ByHost,
            "https://api.githubcopilot.com",
            Some("run-1"),
        );
        assert_eq!(
            request["messages"][0]["copilot_cache_control"]["type"],
            "ephemeral"
        );
        assert_eq!(
            request["messages"][3]["copilot_cache_control"]["type"],
            "ephemeral"
        );
        assert!(request["messages"][1]
            .get("copilot_cache_control")
            .is_none());
        let mut enterprise = body("claude-sonnet-4.5");
        add_cache_hints(
            &mut enterprise,
            PromptCache::ByHost,
            "https://copilot-api.acme.ghe.com",
            None,
        );
        assert_eq!(
            enterprise["messages"][0]["copilot_cache_control"]["type"],
            "ephemeral"
        );
        let mut gpt = body("gpt-4.1");
        add_cache_hints(
            &mut gpt,
            PromptCache::ByHost,
            "https://api.githubcopilot.com",
            Some("run-1"),
        );
        assert_eq!(gpt, body("gpt-4.1"));
    }

    #[test]
    fn other_servers_get_nothing() {
        for url in [
            "http://localhost:11434/v1",
            "https://api.groq.com/openai/v1",
            "not a url",
        ] {
            let mut request = body("llama3");
            add_cache_hints(&mut request, PromptCache::ByHost, url, Some("run-1"));
            assert_eq!(request, body("llama3"), "{url}");
        }
    }

    #[test]
    fn the_configured_style_wins_over_the_host() {
        let mut copilot = body("claude-sonnet-4.5");
        add_cache_hints(
            &mut copilot,
            PromptCache::Copilot,
            "http://127.0.0.1:9999",
            None,
        );
        assert_eq!(
            copilot["messages"][0]["copilot_cache_control"]["type"],
            "ephemeral"
        );
        let mut off = body("gpt-4.1");
        add_cache_hints(
            &mut off,
            PromptCache::Off,
            "https://api.openai.com/v1",
            Some("run-1"),
        );
        assert_eq!(off, body("gpt-4.1"));
    }
}
