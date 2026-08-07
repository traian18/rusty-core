use std::collections::HashMap;
use std::fmt;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use harness_generic_backend::RecoveryPolicy;
use harness_integration_openai::OpenAiConfig;

/// Configuration for an OpenAI-Chat-Completions-compatible endpoint
/// (OpenRouter, Together, Groq, a local Ollama/vLLM/llama.cpp server, ...).
///
/// Unlike [`OpenAiConfig`], nothing here has a sensible cross-provider
/// default — `base_url` and `model` must be supplied by the caller.
///
/// M6 secret-redaction audit: this is the one provider config in the
/// workspace that previously had **no** redacted `Display`/`Debug` at all
/// (every other provider config does) — a real gap, since `api_key` is
/// present here just like the others, and `extra_headers` is a documented,
/// real mechanism for providers that need a custom auth header beyond
/// `Authorization` (e.g. some OpenRouter-compatible endpoints). Both are
/// now redacted the same way the sibling configs already are.
#[derive(Clone, Serialize, Deserialize)]
pub struct OpenAiCompatibleConfig {
    /// e.g. `"https://openrouter.ai/api/v1"`. No default — required.
    pub base_url: String,
    /// Some local servers accept requests without any auth at all.
    #[serde(default)]
    pub api_key: Option<String>,
    /// No sensible default across providers — required.
    pub model: String,
    #[serde(default = "default_max_tokens")]
    pub default_max_tokens: u64,
    #[serde(
        default = "default_timeout",
        rename = "request_timeout_secs",
        serialize_with = "serialize_duration_secs",
        deserialize_with = "deserialize_duration_secs"
    )]
    pub request_timeout: Duration,
    /// Retry, deadline, and circuit-breaker settings for provider calls.
    #[serde(default)]
    pub recovery: RecoveryPolicy,
    /// Some providers require a custom header beyond `Authorization`.
    #[serde(default)]
    pub extra_headers: HashMap<String, String>,
}

fn default_max_tokens() -> u64 {
    4096
}

fn default_timeout() -> Duration {
    Duration::from_secs(120)
}

fn serialize_duration_secs<S>(duration: &Duration, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    serializer.serialize_u64(duration.as_secs())
}

fn deserialize_duration_secs<'de, D>(deserializer: D) -> Result<Duration, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Duration::from_secs(u64::deserialize(deserializer)?))
}

impl fmt::Display for OpenAiCompatibleConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let redacted_key = match &self.api_key {
            Some(key) if key.len() >= 4 => format!("Some({}***)", &key[..4]),
            Some(_) => "Some(***)".to_string(),
            None => "None".to_string(),
        };
        // Header *values* may carry a bearer token or similar (this is
        // exactly why the field exists — a provider-specific auth header
        // beyond `Authorization`); only the names are safe to show.
        let mut header_names: Vec<&str> = self.extra_headers.keys().map(String::as_str).collect();
        header_names.sort_unstable();
        write!(
            f,
            "OpenAiCompatibleConfig {{ base_url: {}, api_key: {}, model: {}, default_max_tokens: {}, request_timeout: {:?}, recovery: {:?}, extra_headers: {{{} header(s): {:?}, values redacted}} }}",
            self.base_url,
            redacted_key,
            self.model,
            self.default_max_tokens,
            self.request_timeout,
            self.recovery,
            header_names.len(),
            header_names,
        )
    }
}

impl fmt::Debug for OpenAiCompatibleConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl OpenAiCompatibleConfig {
    pub fn new(base_url: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            api_key: None,
            model: model.into(),
            default_max_tokens: default_max_tokens(),
            request_timeout: default_timeout(),
            recovery: RecoveryPolicy::default(),
            extra_headers: HashMap::new(),
        }
    }

    /// Maps into the [`OpenAiConfig`] shape `OpenAiClient` actually consumes
    /// — this is the whole reuse mechanism: no forked client, no duplicated
    /// wire/SSE logic, just a different set of field values.
    pub fn into_openai_config(self) -> OpenAiConfig {
        OpenAiConfig {
            api_key: self.api_key.unwrap_or_default(),
            base_url: self.base_url,
            default_model: self.model,
            default_max_tokens: self.default_max_tokens,
            request_timeout: self.request_timeout,
            recovery: self.recovery,
            extra_headers: self.extra_headers,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_fields_into_openai_config() {
        let mut config =
            OpenAiCompatibleConfig::new("https://openrouter.ai/api/v1", "meta-llama/llama-3");
        config.api_key = Some("or-key".to_string());
        config
            .extra_headers
            .insert("HTTP-Referer".to_string(), "my-app".to_string());

        let openai_config = config.into_openai_config();
        assert_eq!(openai_config.base_url, "https://openrouter.ai/api/v1");
        assert_eq!(openai_config.default_model, "meta-llama/llama-3");
        assert_eq!(openai_config.api_key, "or-key");
        assert_eq!(
            openai_config.extra_headers.get("HTTP-Referer"),
            Some(&"my-app".to_string())
        );
    }

    #[test]
    fn formatting_redacts_api_key_and_header_values() {
        let mut config =
            OpenAiCompatibleConfig::new("https://openrouter.ai/api/v1", "meta-llama/llama-3");
        config.api_key = Some("sk-super-secret-value".to_string());
        config.extra_headers.insert(
            "Authorization".to_string(),
            "Bearer sk-also-secret".to_string(),
        );

        let displayed = format!("{config}");
        assert!(
            !displayed.contains("super-secret-value"),
            "api_key must not leak: {displayed}"
        );
        assert!(
            !displayed.contains("also-secret"),
            "header value must not leak: {displayed}"
        );
        assert!(
            displayed.contains("Authorization"),
            "header *name* is fine to show: {displayed}"
        );

        let debugged = format!("{config:?}");
        assert_eq!(
            debugged, displayed,
            "Debug must delegate to the same redacted Display"
        );
    }

    #[test]
    fn formatting_handles_no_api_key() {
        let config = OpenAiCompatibleConfig::new("http://localhost:11434/v1", "llama3");
        let displayed = format!("{config}");
        assert!(
            displayed.contains("None"),
            "an absent key must format as None, not panic: {displayed}"
        );
    }

    #[test]
    fn missing_api_key_maps_to_empty_string_not_a_panic() {
        let config = OpenAiCompatibleConfig::new("http://localhost:11434/v1", "llama3");
        let openai_config = config.into_openai_config();
        assert_eq!(openai_config.api_key, "");
    }

    #[test]
    fn maps_custom_recovery_into_openai_config() {
        let mut config = OpenAiCompatibleConfig::new("http://localhost:11434/v1", "llama3");
        config.recovery.max_attempts = 6;
        let openai_config = config.into_openai_config();
        assert_eq!(openai_config.recovery.max_attempts, 6);
    }

    #[test]
    fn deserializes_from_minimal_json() {
        let config: OpenAiCompatibleConfig = serde_json::from_value(serde_json::json!({
            "base_url": "http://localhost:11434/v1",
            "model": "llama3"
        }))
        .expect("valid minimal config");
        assert_eq!(config.default_max_tokens, 4096);
        assert!(config.api_key.is_none());
        assert_eq!(config.recovery, RecoveryPolicy::default());
    }
}
