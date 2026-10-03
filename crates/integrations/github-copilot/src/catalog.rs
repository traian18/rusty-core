//! Account-scoped model catalog (`GET {api_root}/models`).
//!
//! Copilot rejects any model the account's plan or organization policy has not
//! enabled with `400 model_not_supported`, so a hard-coded default cannot work
//! for everyone. The catalog tells us what this account may actually call and
//! which inference endpoint each model accepts.
use serde_json::Value;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CatalogModel {
    pub id: String,
    pub is_default: bool,
    pub is_fallback: bool,
    pub picker_enabled: bool,
    pub endpoints: Vec<String>,
    pub max_output_tokens: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Catalog {
    models: Vec<CatalogModel>,
}

impl Catalog {
    /// Parses a `/models` payload, keeping only chat models the account may use.
    /// Returns `None` when nothing usable is listed, so callers fall back to
    /// letting the API decide rather than rejecting every request.
    pub fn parse(payload: &Value) -> Option<Self> {
        let entries = payload
            .get("data")
            .and_then(Value::as_array)
            .or_else(|| payload.as_array())?;
        let models: Vec<_> = entries
            .iter()
            .filter_map(|entry| {
                let id = entry["id"].as_str().filter(|id| !id.is_empty())?;
                let chat = entry
                    .pointer("/capabilities/type")
                    .and_then(Value::as_str)
                    .map_or(true, |kind| kind == "chat");
                let enabled =
                    entry.pointer("/policy/state").and_then(Value::as_str) != Some("disabled");
                (chat && enabled).then(|| CatalogModel {
                    id: id.to_owned(),
                    is_default: entry["is_chat_default"].as_bool().unwrap_or(false),
                    is_fallback: entry["is_chat_fallback"].as_bool().unwrap_or(false),
                    picker_enabled: entry["model_picker_enabled"].as_bool().unwrap_or(true),
                    max_output_tokens: entry
                        .pointer("/capabilities/limits/max_output_tokens")
                        .and_then(Value::as_u64)
                        .filter(|limit| *limit > 0),
                    endpoints: entry["supported_endpoints"]
                        .as_array()
                        .map(|list| {
                            list.iter()
                                .filter_map(Value::as_str)
                                .map(str::to_owned)
                                .collect()
                        })
                        .unwrap_or_default(),
                })
            })
            .collect();
        (!models.is_empty()).then_some(Self { models })
    }

    pub fn get(&self, id: &str) -> Option<&CatalogModel> {
        self.models.iter().find(|model| model.id == id)
    }

    /// Model ids worth suggesting to a user: the ones Copilot shows in its
    /// picker, or every listed model if it flags none of them.
    pub fn selectable_ids(&self) -> Vec<&str> {
        let picked: Vec<_> = self
            .models
            .iter()
            .filter(|model| model.picker_enabled)
            .map(|model| model.id.as_str())
            .collect();
        if picked.is_empty() {
            return self.models.iter().map(|model| model.id.as_str()).collect();
        }
        picked
    }

    /// Resolves "auto": the configured default when the account has it, then
    /// Copilot's own chat default and fallback, then the first selectable model.
    pub fn pick_auto(&self, preferred: &str) -> Option<&str> {
        let selectable = || self.models.iter().filter(|model| model.picker_enabled);
        self.get(preferred)
            .or_else(|| selectable().find(|model| model.is_default))
            .or_else(|| selectable().find(|model| model.is_fallback))
            .or_else(|| selectable().next())
            .or_else(|| self.models.first())
            .map(|model| model.id.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn payload() -> Value {
        json!({"data": [
            {"id": "text-embedding-3-small", "capabilities": {"type": "embeddings"}},
            {"id": "gpt-4.1", "policy": {"state": "disabled"}, "model_picker_enabled": true},
            {"id": "claude-opus-9", "policy": {"state": "unconfigured"}, "model_picker_enabled": true},
            {"id": "gpt-4.1-2025-04-14", "capabilities": {"type": "chat"}, "model_picker_enabled": false},
            {"id": "gpt-5-mini", "capabilities": {"type": "chat"}, "policy": {"state": "enabled"},
             "model_picker_enabled": true, "is_chat_fallback": true, "supported_endpoints": ["/chat/completions", "/responses"]},
            {"id": "claude-sonnet-4.5", "capabilities": {"type": "chat"}, "model_picker_enabled": true, "is_chat_default": true}
        ]})
    }

    #[test]
    fn keeps_only_chat_models_the_account_may_use() {
        let catalog = Catalog::parse(&payload()).unwrap();
        for unusable in ["text-embedding-3-small", "gpt-4.1"] {
            assert!(
                catalog.get(unusable).is_none(),
                "{unusable} must be excluded"
            );
        }
        assert!(
            catalog.get("gpt-4.1-2025-04-14").is_some(),
            "hidden aliases stay callable"
        );
        assert_eq!(
            catalog.selectable_ids(),
            ["claude-opus-9", "gpt-5-mini", "claude-sonnet-4.5"]
        );
        assert_eq!(
            catalog.get("gpt-5-mini").unwrap().endpoints,
            ["/chat/completions", "/responses"]
        );
    }

    #[test]
    fn auto_prefers_configured_then_copilot_default_then_fallback() {
        let catalog = Catalog::parse(&payload()).unwrap();
        assert_eq!(catalog.pick_auto("gpt-5-mini"), Some("gpt-5-mini"));
        // The configured default is not enabled for this account.
        assert_eq!(catalog.pick_auto("gpt-4.1"), Some("claude-sonnet-4.5"));
        let no_default = json!({"data": [
            {"id": "a", "model_picker_enabled": false},
            {"id": "b", "model_picker_enabled": true, "is_chat_fallback": true},
            {"id": "c", "model_picker_enabled": true}
        ]});
        assert_eq!(
            Catalog::parse(&no_default).unwrap().pick_auto("gpt-4.1"),
            Some("b")
        );
        let plain = json!([{"id": "a"}, {"id": "b"}]);
        assert_eq!(
            Catalog::parse(&plain).unwrap().pick_auto("gpt-4.1"),
            Some("a")
        );
    }

    #[test]
    fn unusable_or_malformed_payloads_yield_no_catalog() {
        assert!(Catalog::parse(&json!({"data": []})).is_none());
        assert!(Catalog::parse(&json!({"error": "nope"})).is_none());
        assert!(
            Catalog::parse(&json!({"data": [{"id": "x", "policy": {"state": "disabled"}}]}))
                .is_none()
        );
    }
}
