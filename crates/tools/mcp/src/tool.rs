use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use harness_tools::{
    CancellationToken, ToolDescriptor, ToolError, ToolExecutor, ToolId, ToolInput, ToolResult,
};

use crate::client::{McpClient, McpError};
use crate::config::McpServerConfig;
use crate::protocol::{CallToolResult, McpToolInfo};

/// Wraps one tool advertised by a connected MCP server as a `ToolExecutor`.
///
/// The tool id is namespaced `mcp.<server>.<tool>` so tools from different
/// servers — or an MCP tool and a built-in one — never collide in a
/// `ToolRegistry`.
pub struct McpToolExecutor {
    client: Arc<McpClient>,
    remote_name: String,
    descriptor: ToolDescriptor,
}

impl McpToolExecutor {
    pub fn new(client: Arc<McpClient>, server_name: &str, info: McpToolInfo) -> Self {
        let descriptor = ToolDescriptor {
            id: ToolId::new(format!("mcp.{server_name}.{}", info.name)),
            name: info.name.clone(),
            description: info
                .description
                .clone()
                .unwrap_or_else(|| format!("MCP tool `{}` from server `{server_name}`", info.name)),
            input_schema: info.input_schema,
        };
        Self {
            client,
            remote_name: info.name,
            descriptor,
        }
    }
}

#[async_trait]
impl ToolExecutor for McpToolExecutor {
    fn descriptor(&self) -> ToolDescriptor {
        self.descriptor.clone()
    }

    async fn execute(
        &self,
        input: ToolInput,
        cancel: CancellationToken,
    ) -> Result<ToolResult, ToolError> {
        let call_id = self.descriptor.id.as_str().to_string();
        if cancel.is_cancelled() {
            return Err(ToolError::Timeout);
        }

        let outcome = tokio::select! {
            _ = cancel.cancelled() => return Err(ToolError::Timeout),
            outcome = self.client.call_tool(&self.remote_name, input.arguments) => outcome,
        };

        // Transport/protocol failures (server crashed, timed out, sent
        // malformed JSON) are reported the same way every other tool in
        // this workspace reports its own domain errors — as a logical
        // `ToolResult { is_error: true }` the model can read and react to,
        // rather than a `ToolError` that aborts the run.
        match outcome {
            Ok(result) => Ok(ToolResult {
                call_id,
                is_error: result.is_error.unwrap_or(false),
                output: call_result_to_output(&result),
            }),
            Err(err) => Ok(ToolResult {
                call_id,
                output: json!({ "error": err.to_string() }),
                is_error: true,
            }),
        }
    }
}

/// Flattens an MCP `tools/call` result into the harness's tool output
/// shape: `content` is the raw block array (future-proof against content
/// types this crate doesn't model), plus a `text` convenience field
/// concatenating every text block, since that covers the common case and
/// is cheaper for the model to read than re-parsing nested blocks.
fn call_result_to_output(result: &CallToolResult) -> Value {
    let text: String = result
        .content
        .iter()
        .filter_map(|block| block.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n");
    json!({ "content": result.content, "text": text })
}

/// Connects to `config` and returns one `ToolExecutor` per tool the server
/// advertises. All returned executors share a single `McpClient` — one
/// process, one stdio connection per server, regardless of how many tools
/// it exposes.
///
/// This is the entry point session-wiring code calls at startup (see
/// `SessionBuilder::mcp_server` in `harness-engine`).
pub async fn connect_and_discover(
    config: &McpServerConfig,
) -> Result<Vec<Arc<dyn ToolExecutor>>, McpError> {
    let client = McpClient::connect(config).await?;
    let tools = client.list_tools().await?;
    Ok(tools
        .into_iter()
        .map(|info| {
            Arc::new(McpToolExecutor::new(client.clone(), &config.name, info)) as Arc<dyn ToolExecutor>
        })
        .collect())
}
