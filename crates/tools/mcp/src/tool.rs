use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use harness_tools::{
    CancellationToken, ToolDescriptor, ToolError, ToolExecutor, ToolId, ToolInput, ToolResult,
};

use crate::client::McpClient;
use crate::config::McpServerConfig;
use crate::error::McpError;
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

/// The registry ID of a server's tool, which is also the name the model sees
/// and calls it by. Model APIs only accept `^[a-zA-Z0-9_-]+$` (Anthropic caps
/// names at 128 characters, OpenAI at 64), so a dotted `mcp.server.tool`
/// would be rejected before the first turn.
pub fn mcp_tool_id(server: &str, tool: &str) -> String {
    const MAX_LEN: usize = 64;
    let raw = format!("mcp__{server}__{tool}");
    let safe: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if safe == raw && safe.len() <= MAX_LEN {
        return safe;
    }
    // Replacing or truncating characters could make two tools collide, so
    // an altered name carries a hash of the original.
    let suffix = format!("_{:08x}", fnv1a(raw.as_bytes()));
    let keep = (MAX_LEN - suffix.len()).min(safe.len());
    format!("{}{suffix}", &safe[..keep])
}

fn fnv1a(bytes: &[u8]) -> u32 {
    bytes.iter().fold(0x811c_9dc5, |hash, byte| {
        (hash ^ u32::from(*byte)).wrapping_mul(0x0100_0193)
    })
}

impl McpToolExecutor {
    pub fn new(client: Arc<McpClient>, server_name: &str, info: McpToolInfo) -> Self {
        let descriptor = ToolDescriptor {
            id: ToolId::new(mcp_tool_id(server_name, &info.name)),
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
    discover(config, |_| true).await
}

/// Like [`connect_and_discover`], but keeps only tools the server marks
/// read-only (see [`McpToolInfo::is_read_only`]) -- for sessions that may
/// read through an MCP server but must not change anything with it.
pub async fn connect_and_discover_read_only(
    config: &McpServerConfig,
) -> Result<Vec<Arc<dyn ToolExecutor>>, McpError> {
    discover(config, McpToolInfo::is_read_only).await
}

async fn discover(
    config: &McpServerConfig,
    keep: impl Fn(&McpToolInfo) -> bool,
) -> Result<Vec<Arc<dyn ToolExecutor>>, McpError> {
    let client = McpClient::connect(config).await?;
    let tools = client.list_tools().await?;
    Ok(tools
        .into_iter()
        .filter(|info| keep(info))
        .map(|info| {
            Arc::new(McpToolExecutor::new(client.clone(), &config.name, info))
                as Arc<dyn ToolExecutor>
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn is_model_safe(name: &str) -> bool {
        !name.is_empty()
            && name.len() <= 64
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    }

    #[test]
    fn tool_ids_are_readable_and_accepted_by_model_apis() {
        assert_eq!(
            mcp_tool_id("atlassian", "getJiraIssue"),
            "mcp__atlassian__getJiraIssue"
        );
        assert_eq!(
            mcp_tool_id("weather", "get-forecast"),
            "mcp__weather__get-forecast"
        );
    }

    #[test]
    fn unsafe_or_overlong_names_are_rewritten_without_colliding() {
        let dotted = mcp_tool_id("docs", "pages.get");
        let underscored = mcp_tool_id("docs", "pages_get");
        assert!(is_model_safe(&dotted), "{dotted}");
        assert_ne!(dotted, underscored);

        let long_a = mcp_tool_id("atlassian", &"a".repeat(80));
        let long_b = mcp_tool_id("atlassian", &format!("{}b", "a".repeat(79)));
        assert!(
            is_model_safe(&long_a) && is_model_safe(&long_b),
            "{long_a} {long_b}"
        );
        assert_ne!(long_a, long_b);
        assert_eq!(
            mcp_tool_id("atlassian", &"a".repeat(80)),
            long_a,
            "stable across calls"
        );
    }

    #[test]
    fn flattens_multiple_text_blocks_joined_by_newline() {
        let result = CallToolResult {
            content: vec![
                json!({ "type": "text", "text": "first" }),
                json!({ "type": "text", "text": "second" }),
            ],
            is_error: Some(false),
        };
        let output = call_result_to_output(&result);
        assert_eq!(output["text"], "first\nsecond");
        assert_eq!(output["content"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn non_text_blocks_are_kept_in_content_but_excluded_from_the_text_convenience_field() {
        let result = CallToolResult {
            content: vec![
                json!({ "type": "text", "text": "caption" }),
                json!({ "type": "image", "data": "base64...", "mimeType": "image/png" }),
            ],
            is_error: Some(false),
        };
        let output = call_result_to_output(&result);
        assert_eq!(output["text"], "caption");
        assert_eq!(output["content"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn empty_content_produces_empty_text_not_an_error() {
        let output = call_result_to_output(&CallToolResult::default());
        assert_eq!(output["text"], "");
        assert!(output["content"].as_array().unwrap().is_empty());
    }
}
