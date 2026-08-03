use std::fmt;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Configuration for the Gemini `generateContent`/`streamGenerateContent` API.
///
/// Formatting is explicitly redacted so the API key cannot leak through
/// logs. Unlike Anthropic/OpenAI, Gemini authenticates via an API key **query
/// parameter**, not an `Authorization` header — see `client.rs`.
#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct GeminiConfig {
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

impl Default for GeminiConfig {
    fn default() -> Self {
        Self {
            api_key: std::env::var("GEMINI_API_KEY").unwrap_or_default(),
            base_url: "https://generativelanguage.googleapis.com/v1beta".into(),
            default_model: "gemini-1.5-pro".into(),
            default_max_tokens: 8192,
            request_timeout: Duration::from_secs(120),
        }
    }
}

impl fmt::Display for GeminiConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let redacted = if self.api_key.len() >= 4 {
            format!("{}***", &self.api_key[..4])
        } else {
            "***".into()
        };
        write!(
            f,
            "GeminiConfig {{ api_key: {}, base_url: {}, default_model: {}, default_max_tokens: {}, request_timeout: {:?} }}",
            redacted, self.base_url, self.default_model, self.default_max_tokens, self.request_timeout,
        )
    }
}

impl fmt::Debug for GeminiConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl GeminiConfig {
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
        let config: GeminiConfig = serde_json::from_value(serde_json::json!({
            "api_key": "test-key",
            "request_timeout_secs": 30
        }))
        .expect("valid config");
        assert_eq!(config.request_timeout, Duration::from_secs(30));
        assert_eq!(config.default_max_tokens, 8192);

        let value = serde_json::to_value(config).expect("serializable config");
        assert_eq!(value["request_timeout_secs"], 30);
    }

    #[test]
    fn formatting_redacts_api_key() {
        let config = GeminiConfig::new("my-secret-key");
        assert!(!format!("{config}").contains("my-secret-key"));
        assert!(!format!("{config:?}").contains("my-secret-key"));
    }
}
