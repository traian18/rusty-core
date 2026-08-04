
use std::collections::HashMap;
use std::fmt;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use harness_generic_backend::RecoveryPolicy;

/// Configuration for the OpenAI Chat Completions API client.
///
/// Formatting is explicitly redacted so the API key cannot leak through
/// logs. `base_url` includes the version path segment (`/v1`), matching the
/// convention most OpenAI-compatible providers (OpenRouter, Together, Groq)
/// document their own base URLs with — this is what lets
/// `harness-integration-openai-compatible` reuse this client directly by
/// swapping only `base_url`/`api_key`/`default_model` (see that crate's
/// `PLAN.md`).
#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct OpenAiConfig {
    pub api_key: String,
    pub base_url: String,
    pub default_model: String,
    pub default_max_tokens: u64,
    #[serde(
        rename = "request_timeout_secs",
        serialize_with = "serialize_duration_secs",
        deserialize_with = "deserialize_duration_secs"
    )]
    pub request_timeout: Duration,
    /// Retry, deadline, and circuit-breaker settings for provider calls.
    pub recovery: RecoveryPolicy,
    /// Extra headers sent with every request, beyond `Authorization` and
    /// `Content-Type`. Empty for plain OpenAI; some OpenAI-compatible
    /// providers require a provider-specific header (see
    /// `harness-integration-openai-compatible`).
    pub extra_headers: HashMap<String, String>,
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

impl Default for OpenAiConfig {
    fn default() -> Self {
        Self {
            api_key: std::env::var("OPENAI_API_KEY").unwrap_or_default(),
            base_url: "https://api.openai.com/v1".into(),
            default_model: "gpt-4o".into(),
            default_max_tokens: 4096,
            request_timeout: Duration::from_secs(120),
            recovery: RecoveryPolicy::default(),
            extra_headers: HashMap::new(),
        }
    }
}

impl fmt::Display for OpenAiConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let redacted = if self.api_key.len() >= 4 {
            format!("{}***", &self.api_key[..4])
        } else {
            "***".into()
        };
        write!(
            f,
            "OpenAiConfig {{ api_key: {}, base_url: {}, default_model: {}, default_max_tokens: {}, request_timeout: {:?}, recovery: {:?} }}",
            redacted, self.base_url, self.default_model, self.default_max_tokens, self.request_timeout, self.recovery,
        )
    }
}

impl fmt::Debug for OpenAiConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl OpenAiConfig {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            ..Self::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serde_uses_seconds_and_defaults() {
        let config: OpenAiConfig = serde_json::from_value(serde_json::json!({
            "api_key": "test-key",
            "request_timeout_secs": 30
        }))
        .expect("valid config");
        assert_eq!(config.request_timeout, Duration::from_secs(30));
        assert_eq!(config.default_max_tokens, 4096);
        assert_eq!(config.base_url, "https://api.openai.com/v1");
        assert_eq!(config.recovery, RecoveryPolicy::default());

        let value = serde_json::to_value(config).expect("serializable config");
        assert_eq!(value["request_timeout_secs"], 30);
        assert_eq!(value["recovery"]["max_attempts"], 2);
    }

    #[test]
    fn custom_recovery_policy_deserializes() {
        let config: OpenAiConfig = serde_json::from_value(serde_json::json!({
            "api_key": "test-key",
            "recovery": { "max_attempts": 4, "total_deadline_secs": 45 }
        }))
        .expect("valid config");
        assert_eq!(config.recovery.max_attempts, 4);
        assert_eq!(config.recovery.total_deadline, Duration::from_secs(45));
        assert_eq!(config.recovery.circuit_failure_threshold, 3);
    }

    #[test]
    fn formatting_redacts_api_key() {
        let config = OpenAiConfig::new("sk-my-secret-key");
        assert!(!format!("{config}").contains("my-secret-key"));
        assert!(!format!("{config:?}").contains("my-secret-key"));
    }
}
