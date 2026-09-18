use std::collections::HashMap;
use std::fmt;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use harness_generic_backend::RecoveryPolicy;

/// Configuration for the OpenAI Responses API client.
///
/// `base_url` includes the version path segment (`/v1`), matching
/// `harness-integration-openai`'s own convention -- the client posts to
/// `{base_url}/responses`. This is what lets a gateway-style provider (e.g.
/// OpenCode Zen's GPT/Grok/Muse-Spark family) reuse this client directly by
/// pointing `base_url` at its own `/v1`-suffixed root.
#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct OpenAiResponsesConfig {
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
    /// `Content-Type`.
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

impl Default for OpenAiResponsesConfig {
    fn default() -> Self {
        Self {
            api_key: std::env::var("OPENAI_API_KEY").unwrap_or_default(),
            base_url: "https://api.openai.com/v1".into(),
            default_model: "gpt-5".into(),
            default_max_tokens: 4096,
            request_timeout: Duration::from_secs(120),
            recovery: RecoveryPolicy::default(),
            extra_headers: HashMap::new(),
        }
    }
}

impl fmt::Display for OpenAiResponsesConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let redacted = if self.api_key.len() >= 4 {
            format!("{}***", &self.api_key[..4])
        } else {
            "***".into()
        };
        write!(
            f,
            "OpenAiResponsesConfig {{ api_key: {}, base_url: {}, default_model: {}, default_max_tokens: {}, request_timeout: {:?}, recovery: {:?} }}",
            redacted, self.base_url, self.default_model, self.default_max_tokens, self.request_timeout, self.recovery,
        )
    }
}

impl fmt::Debug for OpenAiResponsesConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl OpenAiResponsesConfig {
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
        let config: OpenAiResponsesConfig = serde_json::from_value(serde_json::json!({
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
    }

    #[test]
    fn formatting_redacts_api_key() {
        let config = OpenAiResponsesConfig::new("sk-my-secret-key");
        assert!(!format!("{config}").contains("my-secret-key"));
        assert!(!format!("{config:?}").contains("my-secret-key"));
    }
}
