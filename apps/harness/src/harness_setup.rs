use std::{collections::HashMap, path::PathBuf, sync::Arc};

use anyhow::Result;
use harness_engine::{FsWorkspace, Harness, SessionHandle};
use harness_integration_anthropic::{AnthropicConfig, AnthropicFactory};
use harness_protocol::{
    ids::ToolId,
    tools::{AgentToolset, PermissionMode, ToolCapability, ToolDescriptor, ToolPolicy},
};
use harness_session_store::JsonlSessionStore;

pub async fn start_session(workspace_root: PathBuf) -> Result<SessionHandle> {
    let store_root = workspace_root.join(".harness").join("sessions");
    let harness = Harness::builder()
        .register_integration(Arc::new(AnthropicFactory))
        .session_store(Arc::new(JsonlSessionStore::new(store_root)))
        .build()
        .await?;

    let workspace = Arc::new(FsWorkspace::new(workspace_root));
    Ok(harness
        .session()
        .integration("anthropic", AnthropicConfig::default())?
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
