use std::{collections::HashMap, path::PathBuf, sync::Arc};

use anyhow::Result;
use harness_engine::{BackendSelection, FsWorkspace, Harness, SessionHandle};
use harness_integration_anthropic::AnthropicFactory;
use harness_integration_claude_code::ClaudeCodeFactory;
use harness_integration_codex::CodexFactory;
use harness_integration_github_copilot::GitHubCopilotFactory;
use harness_integration_openai::OpenAiFactory;
use harness_protocol::{
    ids::ToolId,
    tools::{AgentToolset, PermissionMode, ToolCapability, ToolDescriptor, ToolPolicy},
};
use harness_session_store::{JsonlSessionStore, SessionSummary, StoredSession};
use serde_json::{json, Value};

use crate::providers::{option_from_descriptor, ProviderOption, SessionSelection};

/// Options for starting a harness session.
#[derive(Debug, Clone)]
pub struct SessionOptions {
    pub integration: String,
    pub config_json: String,
}

impl Default for SessionOptions {
    fn default() -> Self {
        Self {
            integration: "anthropic".to_string(),
            config_json: "{}".to_string(),
        }
    }
}

impl SessionOptions {
    pub fn selection(&self) -> SessionSelection {
        let config = serde_json::from_str::<Value>(&self.config_json).unwrap_or_else(|_| json!({}));
        crate::providers::selection_for(&self.integration, &config)
    }
}

/// Application-scoped composition root shared by every TUI session.
///
/// Keeping one engine instance means all live sessions share the same
/// scheduler, integration registry, and durable store. This is also the
/// boundary a future IDE host can retain while views are created and closed.
pub struct AppHarness {
    harness: Harness,
    workspace: Arc<FsWorkspace>,
}

impl AppHarness {
    pub async fn new(workspace_root: PathBuf) -> Result<Self> {
        let store_root = workspace_root.join(".harness").join("sessions");
        let harness = Harness::builder()
            .register_integration(Arc::new(AnthropicFactory))
            .register_integration(Arc::new(ClaudeCodeFactory))
            .register_integration(Arc::new(OpenAiFactory))
            .register_integration(Arc::new(CodexFactory))
            .register_integration(Arc::new(GitHubCopilotFactory))
            .session_store(Arc::new(JsonlSessionStore::new(store_root)))
            .build()
            .await?;

        Ok(Self {
            harness,
            workspace: Arc::new(FsWorkspace::new(workspace_root)),
        })
    }

    pub async fn provider_options(&self) -> Result<Vec<ProviderOption>> {
        let mut options = Vec::new();
        for provider in self.harness.list_providers()? {
            let profile = self.harness.list_credential_profiles(&provider.id)?
                .into_iter().next().ok_or_else(|| anyhow::anyhow!("provider {} has no credential profile", provider.name))?;
            let health = self.harness.provider_health(&provider.id)?;
            let models = self.harness.list_models(&provider.id, &profile.id, false).await?;
            options.push(option_from_descriptor(provider, profile, health, models));
        }
        options.sort_by_key(|provider| match provider.integration.as_str() {
            "anthropic" => 0,
            "claude-code" => 1,
            "openai" => 2,
            "codex" => 3,
            "github-copilot" => 4,
            _ => 5,
        });
        Ok(options)
    }

    pub async fn refresh_provider(&self, provider_key: &harness_engine::ProviderKey) -> Result<ProviderOption> {
        let provider = self.harness.list_providers()?.into_iter()
            .find(|candidate| &candidate.id == provider_key)
            .ok_or_else(|| anyhow::anyhow!("unknown provider {provider_key}"))?;
        let profile = self.harness.list_credential_profiles(provider_key)?.into_iter().next()
            .ok_or_else(|| anyhow::anyhow!("provider {} has no credential profile", provider.name))?;
        let health = self.harness.provider_health(provider_key)?;
        let models = self.harness.list_models(provider_key, &profile.id, true).await?;
        Ok(option_from_descriptor(provider, profile, health, models))
    }

    pub fn auth_flow(&self, provider: &harness_engine::ProviderKey) -> Result<harness_engine::AuthFlowHandle> {
        let descriptor = self.harness.list_providers()?.into_iter()
            .find(|candidate| &candidate.id == provider)
            .ok_or_else(|| anyhow::anyhow!("unknown provider {provider}"))?;
        let method = descriptor
            .auth_methods
            .first()
            .copied()
            .ok_or_else(|| anyhow::anyhow!("provider {} has no authentication method", descriptor.name))?;
        Ok(self.harness.begin_auth(provider, method)?)
    }

    pub async fn list_sessions(&self) -> Result<Vec<SessionSummary>> {
        Ok(self.harness.list_sessions().await?)
    }

    pub async fn load_session(&self, id: harness_protocol::ids::SessionId) -> Result<StoredSession> {
        Ok(self.harness.session_store().load_session(id).await?)
    }

    pub async fn restore(
        &self,
        id: harness_protocol::ids::SessionId,
    ) -> Result<SessionHandle> {
        Ok(self
            .harness
            .restore_session_with_toolset(id, default_toolset(), self.workspace.clone())
            .await?)
    }

    pub async fn start(&self, options: SessionOptions) -> Result<SessionHandle> {
        let mut config =
            serde_json::from_str::<Value>(&options.config_json).unwrap_or_else(|_| json!({}));
        let selection = options.selection();
        if let Some(object) = config.as_object_mut() {
            object.insert("_backend_selection".into(), serde_json::to_value(
                harness_protocol::backend::PersistedBackendSelection::v1(
                    selection.provider_id.to_string(),
                    selection.credential_profile.0,
                    selection.model,
                )
            )?);
        }

        Ok(self
            .harness
            .session()
            .integration(&options.integration, config)?
            .toolset(default_toolset(), self.workspace.clone())
            .start()
            .await?)
    }

    pub async fn start_selected(&self, selection: &SessionSelection) -> Result<SessionHandle> {
        let health = self.harness.provider_health(&selection.provider_id)?;
        if !health.ready {
            anyhow::bail!("{}", health.message);
        }
        let profiles = self.harness.list_credential_profiles(&selection.provider_id)?;
        if !profiles.iter().any(|profile| profile.id == selection.credential_profile) {
            anyhow::bail!(
                "credential profile {} is not available for {}",
                selection.credential_profile.0,
                selection.provider
            );
        }
        let selection = BackendSelection {
            provider: selection.provider_id.clone(),
            credential_profile: selection.credential_profile.clone(),
            provider_model_id: selection.model.clone(),
        };
        Ok(self.harness.session_from_selection(&selection)?
            .toolset(default_toolset(), self.workspace.clone())
            .start().await?)
    }
}

fn default_toolset() -> AgentToolset {
    let mut tools = HashMap::new();
    for (name, description, permission) in [
        (
            "fs.read",
            "Read a file from the workspace.",
            PermissionMode::Allow,
        ),
        ("fs.edit", "Replace a workspace file.", PermissionMode::Ask),
        (
            "workspace.search",
            "Search workspace files.",
            PermissionMode::Allow,
        ),
        ("shell.exec", "Run a shell command.", PermissionMode::Ask),
    ] {
        let id = ToolId::new();
        tools.insert(
            id,
            ToolCapability {
                descriptor: ToolDescriptor {
                    id,
                    name: name.to_owned(),
                    description: description.to_owned(),
                    input_schema: serde_json::json!({ "type": "object" }),
                },
                policy: ToolPolicy {
                    permission,
                    enabled: true,
                },
                delegatable: false,
            },
        );
    }
    AgentToolset { tools }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_preserve_provider_selection() {
        let options = SessionOptions {
            integration: "openai".to_owned(),
            config_json: r#"{"default_model":"gpt-test"}"#.to_owned(),
        };

        let selection = options.selection();
        assert_eq!(selection.integration, "openai");
        assert_eq!(selection.model, "gpt-test");
    }

    #[test]
    fn default_toolset_keeps_mutating_tools_permissioned() {
        let toolset = default_toolset();
        let policies = toolset
            .tools
            .values()
            .map(|tool| (tool.descriptor.name.as_str(), &tool.policy.permission))
            .collect::<HashMap<_, _>>();

        assert!(matches!(policies.get("fs.read"), Some(PermissionMode::Allow)));
        assert!(matches!(
            policies.get("workspace.search"),
            Some(PermissionMode::Allow)
        ));
        assert!(matches!(policies.get("fs.edit"), Some(PermissionMode::Ask)));
        assert!(matches!(
            policies.get("shell.exec"),
            Some(PermissionMode::Ask)
        ));
    }
}
