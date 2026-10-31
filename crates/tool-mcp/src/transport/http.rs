//! Streamable-HTTP transport: POSTs JSON-RPC to a single endpoint.
//!
//! Per MCP's streamable-HTTP transport, one endpoint accepts POSTed
//! JSON-RPC and may answer in either of two ways:
//!
//! - `Content-Type: application/json` — one reply, done.
//! - `Content-Type: text/event-stream` — an SSE stream that may carry
//!   progress notifications before the actual reply.
//!
//! Both are handled here. The SSE branch is consumed **incrementally** and
//! returns the moment the matching reply arrives, rather than buffering the
//! whole body: a server is allowed to keep the stream open after answering,
//! and reading to EOF would stall every call until the request timeout.
//!
//! Correlation needs no pending map — unlike stdio, a POST's reply arrives
//! on that POST's own response. Ids are still allocated and checked so a
//! server that interleaves an unrelated message on the stream can't be
//! mistaken for the answer.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, ACCEPT, CONTENT_TYPE};
use serde_json::Value;
use tokio::time::timeout;
use tracing::{debug, warn};

use crate::error::McpError;
use crate::protocol::{JsonRpcInbound, JsonRpcNotification, JsonRpcRequest, MCP_PROTOCOL_VERSION};
use crate::transport::McpTransport;

/// Header the server uses to hand out a session id at `initialize`, which
/// the client must then echo on every subsequent request.
const SESSION_HEADER: &str = "mcp-session-id";
/// Header carrying the negotiated protocol version on post-initialize
/// requests.
const PROTOCOL_VERSION_HEADER: &str = "mcp-protocol-version";

pub(crate) struct HttpTransport {
    client: reqwest::Client,
    server_name: String,
    url: String,
    headers: HeaderMap,
    /// Captured from the `initialize` response, then echoed on every later
    /// request. `OnceLock` for the same reason `McpClient::server_info`
    /// uses one: written once during the handshake, read concurrently
    /// afterwards.
    session_id: OnceLock<String>,
    next_id: AtomicU64,
    request_timeout: Duration,
}

impl HttpTransport {
    pub(crate) fn connect(
        server_name: &str,
        url: &str,
        extra_headers: &HashMap<String, String>,
        request_timeout: Duration,
    ) -> Result<Self, McpError> {
        if !url.starts_with("http://") && !url.starts_with("https://") {
            return Err(McpError::InvalidUrl {
                url: url.to_owned(),
                reason: "must start with http:// or https://".to_owned(),
            });
        }

        let mut headers = HeaderMap::new();
        for (key, value) in extra_headers {
            let name =
                HeaderName::try_from(key.as_str()).map_err(|error| McpError::InvalidUrl {
                    url: url.to_owned(),
                    reason: format!("invalid header name {key:?}: {error}"),
                })?;
            let value = HeaderValue::from_str(value).map_err(|error| McpError::InvalidUrl {
                url: url.to_owned(),
                reason: format!("invalid value for header {key:?}: {error}"),
            })?;
            headers.insert(name, value);
        }

        let client = reqwest::Client::builder()
            .build()
            .map_err(|source| McpError::Http {
                name: server_name.to_owned(),
                source,
            })?;

        Ok(Self {
            client,
            server_name: server_name.to_owned(),
            url: url.to_owned(),
            headers,
            session_id: OnceLock::new(),
            next_id: AtomicU64::new(1),
            request_timeout,
        })
    }

    /// Builds the header set for one request: the caller's static headers,
    /// plus the session id and protocol version once the handshake has
    /// established them.
    fn request_headers(&self) -> HeaderMap {
        let mut headers = self.headers.clone();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            ACCEPT,
            HeaderValue::from_static("application/json, text/event-stream"),
        );
        if let Some(session_id) = self.session_id.get() {
            if let Ok(value) = HeaderValue::from_str(session_id) {
                headers.insert(HeaderName::from_static(SESSION_HEADER), value);
            }
            // The version header only becomes meaningful once `initialize`
            // has happened, and the session id is the marker for that.
            headers.insert(
                HeaderName::from_static(PROTOCOL_VERSION_HEADER),
                HeaderValue::from_static(MCP_PROTOCOL_VERSION),
            );
        }
        headers
    }

    fn http_error(&self, source: reqwest::Error) -> McpError {
        McpError::Http {
            name: self.server_name.clone(),
            source,
        }
    }

    async fn post(&self, body: String) -> Result<reqwest::Response, McpError> {
        let response = self
            .client
            .post(&self.url)
            .headers(self.request_headers())
            .body(body)
            .send()
            .await
            .map_err(|source| self.http_error(source))?;

        // A server hands out its session id at `initialize`; capturing it
        // unconditionally is harmless and avoids special-casing that one
        // method here.
        if let Some(value) = response.headers().get(SESSION_HEADER) {
            if let Ok(value) = value.to_str() {
                let _ = self.session_id.set(value.to_owned());
            }
        }

        if !response.status().is_success() {
            let status = response.status().as_u16();
            let body = response.text().await.unwrap_or_default();
            return Err(McpError::HttpStatus {
                status,
                body: truncate(&body, 512),
            });
        }

        Ok(response)
    }
}

#[async_trait]
impl McpTransport for HttpTransport {
    async fn request(&self, method: &str, params: Option<Value>) -> Result<Value, McpError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let body = serde_json::to_string(&JsonRpcRequest {
            jsonrpc: "2.0",
            id,
            method,
            params,
        })?;

        // One deadline covers the POST and the reply, matching the stdio
        // transport's "request_timeout bounds the whole call" semantics.
        match timeout(self.request_timeout, self.read_reply(body, id)).await {
            Ok(outcome) => outcome,
            Err(_) => Err(McpError::Timeout(self.request_timeout)),
        }
    }

    async fn notify(&self, method: &str, params: Option<Value>) -> Result<(), McpError> {
        let body = serde_json::to_string(&JsonRpcNotification {
            jsonrpc: "2.0",
            method,
            params,
        })?;
        // A notification has no reply: servers answer 202 Accepted with an
        // empty body. `post` has already rejected any non-2xx status.
        match timeout(self.request_timeout, self.post(body)).await {
            Ok(outcome) => outcome.map(|_| ()),
            Err(_) => Err(McpError::Timeout(self.request_timeout)),
        }
    }

    async fn shutdown(&self) {
        let Some(session_id) = self.session_id.get() else {
            return;
        };
        let Ok(value) = HeaderValue::from_str(session_id) else {
            return;
        };
        // Best effort: the server is entitled to answer 405 if it doesn't
        // support explicit session termination, and there is nothing useful
        // to do about a failure while shutting down either way.
        let result = self
            .client
            .delete(&self.url)
            .header(HeaderName::from_static(SESSION_HEADER), value)
            .send()
            .await;
        if let Err(error) = result {
            debug!(server = %self.server_name, error = %error, "MCP: session delete failed");
        }
    }
}

impl HttpTransport {
    /// POSTs `body` and resolves the reply carrying `id`, from either
    /// response shape.
    async fn read_reply(&self, body: String, id: u64) -> Result<Value, McpError> {
        let response = self.post(body).await?;

        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_owned();
        // Compare against the media type only — real servers send
        // `application/json; charset=utf-8`.
        let media_type = content_type
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();

        match media_type.as_str() {
            "application/json" => {
                let text = response.text().await.map_err(|s| self.http_error(s))?;
                let inbound: JsonRpcInbound = serde_json::from_str(&text)?;
                resolve(inbound, id).ok_or(McpError::Closed)?
            }
            "text/event-stream" => self.read_sse_reply(response, id).await,
            other => Err(McpError::UnexpectedContentType(other.to_owned())),
        }
    }

    /// Consumes the SSE stream until the reply for `id` shows up.
    async fn read_sse_reply(
        &self,
        response: reqwest::Response,
        id: u64,
    ) -> Result<Value, McpError> {
        let mut stream = response.bytes_stream();
        let mut buffer = String::new();
        let mut event_data = String::new();

        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|source| self.http_error(source))?;
            buffer.push_str(&String::from_utf8_lossy(&chunk));

            while let Some(newline) = buffer.find('\n') {
                let line = buffer[..newline].trim_end_matches('\r').to_owned();
                buffer.drain(..=newline);

                if line.is_empty() {
                    // Blank line dispatches the accumulated event.
                    if let Some(outcome) = self.dispatch_sse_event(&event_data, id) {
                        return outcome;
                    }
                    event_data.clear();
                    continue;
                }

                // Only `data:` carries payload; `event:`, `id:`, `retry:`,
                // and `:` comments are irrelevant to JSON-RPC correlation.
                if let Some(value) = line.strip_prefix("data:") {
                    if !event_data.is_empty() {
                        event_data.push('\n');
                    }
                    event_data.push_str(value.strip_prefix(' ').unwrap_or(value));
                }
            }
        }

        // Stream ended: flush a final event that had no trailing blank line.
        if let Some(outcome) = self.dispatch_sse_event(&event_data, id) {
            return outcome;
        }
        Err(McpError::Closed)
    }

    /// `Some(result)` when this event was the reply we're waiting for;
    /// `None` to keep reading.
    fn dispatch_sse_event(&self, data: &str, id: u64) -> Option<Result<Value, McpError>> {
        if data.trim().is_empty() {
            return None;
        }
        let inbound: JsonRpcInbound = match serde_json::from_str(data) {
            Ok(inbound) => inbound,
            Err(error) => {
                warn!(server = %self.server_name, error = %error, "MCP: unparseable SSE event");
                return None;
            }
        };
        resolve(inbound, id)
    }
}

/// Matches one inbound message against the id we're waiting for.
///
/// `None` means "not our reply, keep reading" — which covers both
/// server-initiated messages (`method` set, same rule the stdio reader
/// applies) and replies to some other request.
fn resolve(inbound: JsonRpcInbound, id: u64) -> Option<Result<Value, McpError>> {
    if inbound.method.is_some() {
        return None;
    }
    if inbound.id.as_ref().and_then(Value::as_u64) != Some(id) {
        return None;
    }
    Some(match inbound.error {
        Some(error) => Err(McpError::Rpc {
            code: error.code,
            message: error.message,
        }),
        None => Ok(inbound.result.unwrap_or(Value::Null)),
    })
}

fn truncate(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_owned();
    }
    text.chars().take(limit).collect::<String>() + "…"
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `HttpTransport` holds a non-`Debug` `reqwest::Client`, so
    /// `Result::expect_err` isn't available here — match instead. Same
    /// reason `client_e2e.rs` matches on `McpClient::connect`'s result.
    #[test]
    fn rejects_a_url_without_an_http_scheme() {
        match HttpTransport::connect(
            "remote",
            "ftp://example.com/mcp",
            &HashMap::new(),
            Duration::from_secs(1),
        ) {
            Err(McpError::InvalidUrl { .. }) => {}
            Err(other) => panic!("expected McpError::InvalidUrl, got {other:?}"),
            Ok(_) => panic!("a non-http scheme must be refused"),
        }
    }

    #[test]
    fn rejects_an_unusable_header_name() {
        let headers = HashMap::from([("bad header".to_owned(), "value".to_owned())]);
        match HttpTransport::connect(
            "remote",
            "https://example.com/mcp",
            &headers,
            Duration::from_secs(1),
        ) {
            Err(McpError::InvalidUrl { .. }) => {}
            Err(other) => panic!("expected McpError::InvalidUrl, got {other:?}"),
            Ok(_) => panic!("an invalid header name must be refused"),
        }
    }

    #[test]
    fn resolve_ignores_server_initiated_messages_and_other_ids() {
        let notification: JsonRpcInbound =
            serde_json::from_str(r#"{"jsonrpc":"2.0","method":"notifications/progress"}"#).unwrap();
        assert!(resolve(notification, 1).is_none());

        let other: JsonRpcInbound =
            serde_json::from_str(r#"{"jsonrpc":"2.0","id":7,"result":{}}"#).unwrap();
        assert!(resolve(other, 1).is_none());

        let ours: JsonRpcInbound =
            serde_json::from_str(r#"{"jsonrpc":"2.0","id":1,"result":{"ok":true}}"#).unwrap();
        assert!(matches!(resolve(ours, 1), Some(Ok(_))));
    }

    #[test]
    fn resolve_surfaces_an_rpc_error_member() {
        let inbound: JsonRpcInbound = serde_json::from_str(
            r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32602,"message":"nope"}}"#,
        )
        .unwrap();
        assert!(matches!(
            resolve(inbound, 1),
            Some(Err(McpError::Rpc { code: -32602, .. }))
        ));
    }

    #[test]
    fn truncate_leaves_short_bodies_alone_and_caps_long_ones() {
        assert_eq!(truncate("short", 512), "short");
        assert_eq!(truncate(&"x".repeat(600), 8), "xxxxxxxx…");
    }
}
