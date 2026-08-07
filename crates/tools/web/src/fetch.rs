use std::net::IpAddr;
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use harness_tools::{
    CancellationToken, ToolDescriptor, ToolError, ToolExecutor, ToolId, ToolInput, ToolResult,
};

use crate::ssrf::is_safe_target;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
/// Enforced by capping bytes actually read off the response stream, not by
/// trusting `Content-Length` — that header can lie.
const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
/// Width used for `html2text`'s HTML→text line wrapping.
const HTML_TEXT_WIDTH: usize = 120;

/// Input for the `web.fetch` tool.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct FetchInput {
    pub url: String,
}

/// Fetches a URL over HTTP(S) and returns its text content (HTML is
/// converted to readable text). Static content only — no JavaScript
/// rendering.
///
/// Rejects redirects rather than following them blindly (a redirect to a
/// disallowed address would otherwise bypass the SSRF guard); the caller
/// sees the `Location` header and can re-fetch it explicitly if appropriate.
pub struct FetchTool {
    client: reqwest::Client,
}

impl FetchTool {
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("reqwest client with rustls-tls should always build");
        Self { client }
    }
}

impl Default for FetchTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ToolExecutor for FetchTool {
    fn descriptor(&self) -> ToolDescriptor {
        let schema = schemars::schema_for!(FetchInput);
        ToolDescriptor {
            id: ToolId::new("web.fetch"),
            name: "Fetch URL".to_string(),
            description: "Fetch a URL over HTTP(S) and return its text content".to_string(),
            input_schema: serde_json::to_value(schema).unwrap_or(json!({})),
        }
    }

    async fn execute(
        &self,
        input: ToolInput,
        cancel: CancellationToken,
    ) -> Result<ToolResult, ToolError> {
        let input: FetchInput = input.parse().map_err(|_| ToolError::ExecutionFailed)?;
        if cancel.is_cancelled() {
            return Err(ToolError::Timeout);
        }

        match fetch(&self.client, &input.url).await {
            Ok(FetchOutcome::Content { content_type, body }) => Ok(ToolResult {
                call_id: "web.fetch".to_string(),
                output: json!({ "content_type": content_type, "content": body }),
                is_error: false,
            }),
            Ok(FetchOutcome::Redirected { location }) => Ok(ToolResult {
                call_id: "web.fetch".to_string(),
                output: json!({
                    "error": format!("redirected to {location}; not followed automatically")
                }),
                is_error: true,
            }),
            Err(message) => Ok(ToolResult {
                call_id: "web.fetch".to_string(),
                output: json!({ "error": message }),
                is_error: true,
            }),
        }
    }
}

enum FetchOutcome {
    Content { content_type: String, body: String },
    Redirected { location: String },
}

async fn fetch(client: &reqwest::Client, url_str: &str) -> Result<FetchOutcome, String> {
    let url = reqwest::Url::parse(url_str).map_err(|e| format!("invalid URL: {e}"))?;
    if url.scheme() != "http" && url.scheme() != "https" {
        return Err(format!("unsupported scheme: {}", url.scheme()));
    }
    let host = url
        .host_str()
        .ok_or_else(|| "URL has no host".to_string())?;
    let port = url.port_or_known_default().unwrap_or(443);

    validate_host_is_safe(host, port).await?;

    let response = client
        .get(url.clone())
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if response.status().is_redirection() {
        let location = response
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("<unknown>")
            .to_string();
        return Ok(FetchOutcome::Redirected { location });
    }
    if !response.status().is_success() {
        return Err(format!("HTTP {}", response.status()));
    }

    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_string();

    let bytes = read_capped(response, MAX_RESPONSE_BYTES).await?;
    let text = String::from_utf8_lossy(&bytes).into_owned();

    let body = if content_type.contains("text/html") {
        html2text::from_read(text.as_bytes(), HTML_TEXT_WIDTH)
    } else {
        text
    };

    Ok(FetchOutcome::Content { content_type, body })
}

/// Validates every address a hostname resolves to, not just the first —
/// some DNS answers legitimately return several. A literal IP in the URL is
/// checked directly without a DNS round-trip.
///
/// Note: this checks the resolved address *before* `reqwest` connects: a
/// TOCTOU gap exists between this check and the actual connection (DNS could
/// theoretically change in between). Accepted as a reasonable v1 mitigation;
/// closing it fully would require overriding `reqwest`'s connector/resolver.
async fn validate_host_is_safe(host: &str, port: u16) -> Result<(), String> {
    if let Ok(ip) = host.parse::<IpAddr>() {
        return if is_safe_target(ip) {
            Ok(())
        } else {
            Err(format!("refusing to fetch from disallowed address: {ip}"))
        };
    }

    let addrs = tokio::net::lookup_host((host, port))
        .await
        .map_err(|e| format!("DNS resolution failed for {host}: {e}"))?;

    let mut resolved_any = false;
    for addr in addrs {
        resolved_any = true;
        if !is_safe_target(addr.ip()) {
            return Err(format!(
                "refusing to fetch {host}: resolves to disallowed address {}",
                addr.ip()
            ));
        }
    }
    if !resolved_any {
        return Err(format!("DNS resolution for {host} returned no addresses"));
    }
    Ok(())
}

/// Reads `response`'s body, capped at `max_bytes`.
///
/// M3: this bounds the *decoded* stream, not the wire transfer. `reqwest`'s
/// `gzip`/`deflate`/`brotli`/`zstd` features (enabled on this crate's
/// `reqwest` dependency) make it transparently decompress
/// `Content-Encoding`-compressed responses; that decompression happens
/// lazily, one chunk at a time, as the caller pulls from `bytes_stream()` —
/// not all at once into a single buffer — so stopping the pull loop the
/// instant the cap is exceeded (below) already stops asking the decoder for
/// more output, which is exactly what defends against a decompression bomb
/// (a small compressed payload that expands to a huge decoded size): the
/// cap is enforced against decoded bytes actually produced, not against
/// `Content-Length` (which describes the *compressed* wire size and would
/// hugely understate the true decoded size for a bomb) and not by fully
/// decompressing before measuring.
async fn read_capped(response: reqwest::Response, max_bytes: usize) -> Result<Vec<u8>, String> {
    read_capped_stream(
        response
            .bytes_stream()
            .map(|chunk| chunk.map_err(|e| e.to_string())),
        max_bytes,
    )
    .await
}

/// The actual capping loop, generic over any fallible byte-chunk stream —
/// factored out from [`read_capped`] so the cap/truncate behavior itself is
/// directly unit-testable without a live HTTP connection (this tool's SSRF
/// guard rejects loopback outright, so a local fixture server can't stand
/// in for one here the way other tools' tests do).
async fn read_capped_stream<S>(stream: S, max_bytes: usize) -> Result<Vec<u8>, String>
where
    S: futures::Stream<Item = Result<bytes::Bytes, String>>,
{
    let mut stream = Box::pin(stream);
    let mut buffer = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        buffer.extend_from_slice(&chunk);
        if buffer.len() > max_bytes {
            buffer.truncate(max_bytes);
            break;
        }
    }
    Ok(buffer)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn rejects_a_loopback_url_before_connecting() {
        let tool = FetchTool::new();
        let result = tool
            .execute(
                ToolInput {
                    arguments: json!({ "url": "http://127.0.0.1:1/" }),
                },
                CancellationToken::new(),
            )
            .await
            .expect("execute should not hard-fail");
        assert!(result.is_error);
        let error = result.output["error"].as_str().unwrap_or("");
        assert!(error.contains("disallowed address"), "got: {error}");
    }

    #[tokio::test]
    async fn rejects_the_cloud_metadata_address() {
        let tool = FetchTool::new();
        let result = tool
            .execute(
                ToolInput {
                    arguments: json!({ "url": "http://169.254.169.254/latest/meta-data/" }),
                },
                CancellationToken::new(),
            )
            .await
            .expect("execute should not hard-fail");
        assert!(result.is_error);
    }

    #[tokio::test]
    async fn rejects_an_unsupported_scheme() {
        let tool = FetchTool::new();
        let result = tool
            .execute(
                ToolInput {
                    arguments: json!({ "url": "file:///etc/passwd" }),
                },
                CancellationToken::new(),
            )
            .await
            .expect("execute should not hard-fail");
        assert!(result.is_error);
        let error = result.output["error"].as_str().unwrap_or("");
        assert!(error.contains("unsupported scheme"), "got: {error}");
    }

    #[tokio::test]
    async fn rejects_an_invalid_url() {
        let tool = FetchTool::new();
        let result = tool
            .execute(
                ToolInput {
                    arguments: json!({ "url": "not a url" }),
                },
                CancellationToken::new(),
            )
            .await
            .expect("execute should not hard-fail");
        assert!(result.is_error);
    }

    // -----------------------------------------------------------------
    // M3: decompression-bomb cap (`read_capped_stream`)
    // -----------------------------------------------------------------
    //
    // `web.fetch`'s SSRF guard rejects loopback outright (see
    // `rejects_a_loopback_url_before_connecting` above), so unlike the
    // fixture-server pattern other tools' tests use, there is no way to
    // stand up a local HTTP server and exercise `fetch()`/`read_capped` for
    // this crate. `read_capped_stream` is factored out specifically so the
    // cap-and-truncate behavior — which is what actually matters here, not
    // whether `reqwest`'s gzip decoder itself works (a well-tested upstream
    // concern, not this crate's) — can be proven directly against a
    // synthetic stream shaped the way a decompression bomb would be: many
    // chunks, each far larger than what the "compressed" transfer size
    // would suggest.

    fn ok_chunk(bytes: &[u8]) -> Result<bytes::Bytes, String> {
        Ok(bytes::Bytes::copy_from_slice(bytes))
    }

    #[tokio::test]
    async fn read_capped_stream_stops_pulling_once_the_cap_is_exceeded() {
        // Each chunk is 1MB; the cap is 2.5MB, so the loop must stop after
        // the 3rd chunk (having read 3MB) rather than draining the whole
        // 10-chunk (10MB) stream — the defining property of a *bomb* defense:
        // the loop does not keep asking the (here: fake) decoder for more
        // decoded output once the cap is already exceeded.
        let chunk = vec![b'x'; 1024 * 1024];
        let pulled = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let pulled_for_stream = pulled.clone();
        let stream = futures::stream::iter(0..10).then(move |_| {
            let chunk = chunk.clone();
            let pulled = pulled_for_stream.clone();
            async move {
                pulled.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                ok_chunk(&chunk)
            }
        });

        let result = read_capped_stream(stream, 2_500_000)
            .await
            .expect("capped read should succeed");

        assert_eq!(
            result.len(),
            2_500_000,
            "output must be truncated to exactly the cap"
        );
        assert!(
            pulled.load(std::sync::atomic::Ordering::SeqCst) <= 3,
            "must stop pulling chunks once the cap is exceeded, pulled {} of 10",
            pulled.load(std::sync::atomic::Ordering::SeqCst)
        );
    }

    #[tokio::test]
    async fn read_capped_stream_passes_through_content_under_the_cap_unchanged() {
        let stream = futures::stream::iter(vec![ok_chunk(b"hello, "), ok_chunk(b"world")]);
        let result = read_capped_stream(stream, MAX_RESPONSE_BYTES)
            .await
            .expect("capped read should succeed");
        assert_eq!(result, b"hello, world");
    }

    #[tokio::test]
    async fn read_capped_stream_propagates_a_mid_stream_error() {
        let stream = futures::stream::iter(vec![
            ok_chunk(b"partial"),
            Err("connection reset".to_string()),
        ]);
        let result = read_capped_stream(stream, MAX_RESPONSE_BYTES).await;
        assert_eq!(result, Err("connection reset".to_string()));
    }
}
