//! Providers, credentials and models: what the host can offer and how a selection becomes a session.

use super::*;

use crate::providers::{
    self, AuthFlowHandle, AuthFlowState, AuthMethod, BackendSelection, CredentialProfileId,
    CredentialProfileSummary, CredentialState, ModelDescriptor, ProviderDescriptor, ProviderHealth,
    ProviderKey,
};
use crate::session_builder::{HarnessError, SessionBuilder};

impl Harness {
    /// List every registered provider through a storage-neutral engine model.
    pub fn list_providers(&self) -> Result<Vec<ProviderDescriptor>, HarnessError> {
        Ok(self
            .integrations
            .list()?
            .into_iter()
            .map(|(id, descriptor)| providers::descriptor_for(&id, descriptor.capabilities))
            .collect())
    }

    pub fn list_credential_profiles(
        &self,
        provider: &ProviderKey,
    ) -> Result<Vec<CredentialProfileSummary>, HarnessError> {
        let descriptor = self
            .list_providers()?
            .into_iter()
            .find(|item| &item.id == provider)
            .ok_or_else(|| HarnessError::UnknownProvider(provider.to_string()))?;
        let auth_method = descriptor.auth_methods[0];
        let state = match provider.as_str() {
            "anthropic-api" => {
                if std::env::var_os("ANTHROPIC_API_KEY").is_some() {
                    CredentialState::Available
                } else {
                    CredentialState::Missing
                }
            }
            "openai-api" => {
                if std::env::var_os("OPENAI_API_KEY").is_some() {
                    CredentialState::Available
                } else {
                    CredentialState::Missing
                }
            }
            _ => CredentialState::ManagedExternally,
        };
        Ok(vec![CredentialProfileSummary {
            id: CredentialProfileId::new(format!("{}:default", provider.as_str())),
            provider: provider.clone(),
            label: descriptor.credential_hint,
            state,
            auth_method,
        }])
    }

    pub fn begin_auth(
        &self,
        provider: &ProviderKey,
        method: AuthMethod,
    ) -> Result<AuthFlowHandle, HarnessError> {
        let profile = self
            .list_credential_profiles(provider)?
            .into_iter()
            .next()
            .ok_or_else(|| HarnessError::UnknownProvider(provider.to_string()))?;
        let next = match method {
            AuthMethod::Environment if profile.state == CredentialState::Available => {
                AuthFlowState::Connected { profile }
            }
            AuthMethod::Environment => AuthFlowState::Failed {
                safe_message: "Set the provider API-key environment variable and refresh".into(),
            },
            AuthMethod::CliManaged => {
                let (program, args) = match provider.as_str() {
                    "github-copilot" => ("copilot", vec!["login"]),
                    "codex" => ("codex", vec!["login"]),
                    _ => return Err(HarnessError::UnknownProvider(provider.to_string())),
                };
                AuthFlowState::WaitingForExternalCommand {
                    program: program.into(),
                    args: args.into_iter().map(str::to_owned).collect(),
                }
            }
        };
        Ok(AuthFlowHandle {
            provider: provider.clone(),
            states: vec![AuthFlowState::Starting, next],
        })
    }

    pub async fn list_models(
        &self,
        provider: &ProviderKey,
        _credential: &CredentialProfileId,
        refresh: bool,
    ) -> Result<Vec<ModelDescriptor>, HarnessError> {
        if !self
            .list_providers()?
            .iter()
            .any(|item| &item.id == provider)
        {
            return Err(HarnessError::UnknownProvider(provider.to_string()));
        }
        if !refresh {
            if let Some(models) = self
                .model_cache
                .read()
                .map_err(|_| HarnessError::ProviderCatalog("model cache lock poisoned".into()))?
                .get(provider)
                .cloned()
            {
                return Ok(models);
            }
            return Ok(providers::default_models(provider));
        }
        match providers::discover_api_models(provider).await {
            Ok(models) if !models.is_empty() => {
                self.model_cache
                    .write()
                    .map_err(|_| HarnessError::ProviderCatalog("model cache lock poisoned".into()))?
                    .insert(provider.clone(), models.clone());
                Ok(models)
            }
            Ok(_) => Ok(providers::default_models(provider)),
            Err(_) => {
                let mut fallback = self
                    .model_cache
                    .read()
                    .map_err(|_| HarnessError::ProviderCatalog("model cache lock poisoned".into()))?
                    .get(provider)
                    .cloned()
                    .unwrap_or_else(|| providers::default_models(provider));
                for model in &mut fallback {
                    model.stale = true;
                }
                Ok(fallback)
            }
        }
    }

    /// Validates a model override against this provider's known catalog
    /// before it's applied via `SessionBuilder::execution_params` /
    /// `SessionHandle::set_execution_params`.
    ///
    /// Best-effort, not exhaustive: it checks the cached catalog from the
    /// last `list_models(refresh: true)` call, falling back to the static
    /// `default_models()` list when nothing has been cached yet. A model
    /// that's real but simply hasn't been discovered/cached yet can produce
    /// a false rejection here — callers that need certainty should
    /// `list_models(provider, credential, true)` first. This exists to catch
    /// the common case (a typo'd or stale model id) before it reaches a
    /// provider and fails deep inside a run.
    pub fn validate_model_override(
        &self,
        provider: &ProviderKey,
        provider_model_id: &str,
    ) -> Result<(), HarnessError> {
        let cached = self
            .model_cache
            .read()
            .map_err(|_| HarnessError::ProviderCatalog("model cache lock poisoned".into()))?
            .get(provider)
            .cloned();
        let catalog = cached.unwrap_or_else(|| providers::default_models(provider));
        if catalog
            .iter()
            .any(|model| model.provider_model_id == provider_model_id)
        {
            return Ok(());
        }
        Err(HarnessError::UnknownModel {
            provider: provider.to_string(),
            model_id: provider_model_id.to_string(),
        })
    }

    pub fn provider_health(&self, provider: &ProviderKey) -> Result<ProviderHealth, HarnessError> {
        let profile = self.list_credential_profiles(provider)?.remove(0);
        let program = match provider.as_str() {
            "codex" => Some("codex"),
            "github-copilot" => Some("copilot"),
            _ => None,
        };
        let executable = program.and_then(providers::find_executable);
        let ready = profile.state != CredentialState::Missing
            && (program.is_none() || executable.is_some());
        let message = if profile.state == CredentialState::Missing {
            "Credential unavailable".into()
        } else if program.is_some() && executable.is_none() {
            format!(
                "{} executable was not found on PATH",
                program.unwrap_or("provider")
            )
        } else {
            "Ready".into()
        };
        Ok(ProviderHealth {
            provider: provider.clone(),
            credential: profile.state,
            executable,
            ready,
            message,
        })
    }

    pub fn session_from_selection(
        &self,
        selection: &BackendSelection,
    ) -> Result<SessionBuilder, HarnessError> {
        let descriptor = self
            .list_providers()?
            .into_iter()
            .find(|item| item.id == selection.provider)
            .ok_or_else(|| HarnessError::UnknownProvider(selection.provider.to_string()))?;
        let mut config = match descriptor.integration.as_str() {
            "anthropic" | "openai" => {
                serde_json::json!({"default_model": selection.provider_model_id.clone()})
            }
            "codex" | "github-copilot" if selection.provider_model_id == "default" => {
                serde_json::json!({})
            }
            "codex" => {
                serde_json::json!({"extra_args": ["--model", selection.provider_model_id.clone()]})
            }
            "github-copilot" => serde_json::json!({"model": selection.provider_model_id.clone()}),
            _ => serde_json::json!({}),
        };
        if let Some(object) = config.as_object_mut() {
            object.insert(
                "_backend_selection".into(),
                serde_json::to_value(rusty_protocol::backend::PersistedBackendSelection::v1(
                    selection.provider.to_string(),
                    selection.credential_profile.0.clone(),
                    selection.provider_model_id.clone(),
                ))?,
            );
        }
        self.session().integration(descriptor.integration, config)
    }
}
