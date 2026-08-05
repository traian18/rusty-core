use std::{collections::HashMap, path::PathBuf, sync::Arc};

use anyhow::Result;
use harness_engine::{FsWorkspace, Harness, SessionHandle};
use harness_integration_anthropic::{AnthropicConfig, AnthropicFactory};
use harness_integration_claude_code::{ClaudeCodeConfig, ClaudeCodeFactory};
use harness_protocol::{
    ids::ToolId,
    tools::{AgentToolset, PermissionMode, ToolCapability, ToolDescriptor, ToolPolicy},
};
use harness_session_store::JsonlSessionStore;
use serde_json::{json, Value};

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

pub async fn start_session(
    workspace_root: PathBuf,
    options: SessionOptions,
) -> Result<SessionHandle> {
    let store_root = workspace_root.join(".harness").join("sessions");
    let harness = Harness::builder()
        .register_integration(Arc::new(AnthropicFactory))
        .register_integration(Arc::new(ClaudeCodeFactory))
        .session_store(Arc::new(JsonlSessionStore::new(store_root)))
        .build()
        .await?;

    let workspace = Arc::new(FsWorkspace::new(workspace_root));

    // Parse the config JSON
    let config = serde_json::from_str::<Value>(&options.config_json)
        .unwrap_or_else(|_| json!({}));

    Ok(harness
        .session()
        .integration(&options.integration, config)?
        .toolset(default_toolset(), workspace)
        .start()
        .await?)
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
