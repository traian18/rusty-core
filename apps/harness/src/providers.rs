use harness_engine::{CredentialProfileId, CredentialProfileSummary, ProviderDescriptor, ProviderKey};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderOption {
    pub id: ProviderKey,
    pub integration: String,
    pub name: String,
    pub account_hint: String,
    pub credential_profile: CredentialProfileId,
    pub credential_state: String,
    pub health_message: String,
    pub ready: bool,
    pub default_model: String,
    pub models: Vec<String>,
    pub model_hint: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSelection {
    pub provider_id: ProviderKey,
    pub credential_profile: CredentialProfileId,
    pub integration: String,
    pub provider: String,
    pub model: String,
}

pub fn option_from_descriptor(provider: ProviderDescriptor, profile: CredentialProfileSummary, health: harness_engine::ProviderHealth, models: Vec<harness_engine::ModelDescriptor>) -> ProviderOption {
    let default_model = models.iter().find(|model| model.is_default).or_else(|| models.first())
        .map(|model| model.provider_model_id.clone()).unwrap_or_else(|| "default".into());
    ProviderOption {
        credential_profile: profile.id,
        credential_state: format!("{:?}", profile.state),
        health_message: health.message,
        ready: health.ready,
        id: provider.id,
        integration: provider.integration,
        name: provider.name,
        account_hint: provider.credential_hint,
        default_model,
        models: models.into_iter().map(|model| model.provider_model_id).collect(),
        model_hint: "Select a discovered model or enter a custom provider model ID".into(),
    }
}

pub fn fallback_options() -> Vec<ProviderOption> {
    [
        (
            "anthropic-api",
            "anthropic",
            "Anthropic API",
            "ANTHROPIC_API_KEY",
            &["claude-sonnet-4-20250514", "claude-opus-4-20250514"][..],
        ),
        (
            "claude-code",
            "claude-code",
            "Claude Code",
            "Claude CLI login",
            &["sonnet", "opus", "haiku"][..],
        ),
        (
            "openai-api",
            "openai",
            "OpenAI API",
            "OPENAI_API_KEY",
            &["gpt-4o", "gpt-4.1", "o3"][..],
        ),
        (
            "codex",
            "codex",
            "OpenAI Codex",
            "Codex CLI login",
            &["default"][..],
        ),
        (
            "github-copilot",
            "github-copilot",
            "GitHub Copilot",
            "Copilot CLI login",
            &["auto"][..],
        ),
    ]
    .into_iter()
    .map(|(id, integration, name, hint, models)| ProviderOption {
        id: ProviderKey::new(id),
        integration: integration.into(),
        name: name.into(),
        account_hint: hint.into(),
        credential_profile: CredentialProfileId::new(format!("{id}:default")),
        credential_state: "Unknown".into(),
        health_message: "Health not checked".into(),
        ready: true,
        default_model: models[0].into(),
        models: models.iter().map(|model| (*model).to_owned()).collect(),
        model_hint: "Select a known model or enter a provider model ID".into(),
    })
    .collect()
}

pub fn selection_for(integration: &str, config: &Value) -> SessionSelection {
    let options = fallback_options();
    let provider = options.iter().find(|provider| provider.integration == integration).unwrap_or(&options[0]);
    let configured_model = config.get("_backend_selection").and_then(|selection| selection.get("provider_model_id")).and_then(Value::as_str).map(str::to_owned)
        .or_else(|| config.get("default_model").and_then(Value::as_str).map(str::to_owned))
        .or_else(|| config.get("model").and_then(Value::as_str).map(str::to_owned))
        .or_else(|| config.get("extra_args").and_then(Value::as_array).and_then(|args| {
            args.windows(2).find_map(|pair| {
                let flag = pair.first()?.as_str()?;
                let value = pair.get(1)?.as_str()?;
                (flag == "--model").then(|| value.to_owned())
            })
        }));
    SessionSelection {
        provider_id: provider.id.clone(),
        credential_profile: provider.credential_profile.clone(),
        integration: provider.integration.clone(),
        provider: provider.name.clone(),
        model: configured_model.unwrap_or_else(|| provider.default_model.clone()),
    }
}

pub fn selection_for_backend(backend_name: Option<&str>, config: &Value) -> SessionSelection {
    let integration = match backend_name.unwrap_or_default().to_ascii_lowercase().as_str() {
        name if name.contains("anthropic") => "anthropic",
        name if name.contains("claude") => "claude-code",
        name if name.contains("copilot") => "github-copilot",
        name if name.contains("codex") => "codex",
        name if name.contains("openai") => "openai",
        _ => "anthropic",
    };
    let mut selection = selection_for(integration, config);
    if let Some(model) = backend_name
        .and_then(|name| name.rsplit_once('[').map(|(_, encoded)| encoded.trim_end_matches(']')))
        .and_then(|encoded| encoded.split_once(':').map(|(_, model)| model)) {
        selection.model = model.to_owned();
    }
    selection
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn api_selection_keeps_provider_model_separate() {
        let mut selection = selection_for("openai", &json!({}));
        selection.model = "gpt-test".into();
        assert_eq!(selection.integration, "openai");
        assert_eq!(selection.model, "gpt-test");
    }

    #[test]
    fn restored_backend_name_recovers_provider_and_model() {
        let selection = selection_for_backend(Some("OpenAI"), &json!({ "default_model": "gpt-restored" }));
        assert_eq!(selection.integration, "openai");
        assert_eq!(selection.model, "gpt-restored");
    }

    #[test]
    fn descriptor_fallback_preserves_exact_model_slug() {
        let selection = selection_for_backend(Some("OpenAI [openai-api:gpt-exact-2026-08-05]"), &json!({}));
        assert_eq!(selection.model, "gpt-exact-2026-08-05");
    }

    #[test]
    fn copilot_selection_keeps_the_explicit_model() {
        let selection = selection_for("github-copilot", &json!({"model":"auto"}));
        assert_eq!(selection.integration, "github-copilot");
        assert_eq!(selection.model, "auto");
    }
}
