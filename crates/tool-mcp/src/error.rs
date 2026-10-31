use std::time::Duration;

/// Errors talking to an MCP server. All of these are handled as a logical
/// tool failure (`ToolResult { is_error: true }`) by
/// [`crate::tool::McpToolExecutor`] rather than aborting the run — the
/// model can see "the MCP server timed out" and retry or route around it.
#[derive(Debug, thiserror::Error)]
pub enum McpError {
    #[error("failed to spawn MCP server '{name}': {source}")]
    Spawn {
        name: String,
        #[source]
        source: std::io::Error,
    },
    #[error("MCP server closed its connection")]
    Closed,
    #[error("I/O error talking to MCP server: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid JSON from MCP server: {0}")]
    InvalidJson(#[from] serde_json::Error),
    #[error("MCP request timed out after {0:?}")]
    Timeout(Duration),
    #[error("MCP server returned an error (code {code}): {message}")]
    Rpc { code: i64, message: String },

    // --- HTTP transport only ---
    #[error("HTTP error talking to MCP server '{name}': {source}")]
    Http {
        name: String,
        #[source]
        source: reqwest::Error,
    },
    #[error("MCP server returned HTTP {status}: {body}")]
    HttpStatus { status: u16, body: String },
    /// The streamable-HTTP transport answers with either `application/json`
    /// or `text/event-stream`; anything else means we're talking to
    /// something that isn't an MCP endpoint (a proxy error page, an HTML
    /// login redirect), and saying so beats a JSON parse error.
    #[error("MCP server replied with unexpected content type {0:?}")]
    UnexpectedContentType(String),
    #[error("invalid MCP server URL {url:?}: {reason}")]
    InvalidUrl { url: String, reason: String },
}
