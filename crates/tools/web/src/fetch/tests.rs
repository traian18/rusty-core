use super::*;

/// Regression test for a real live 400: `SessionBuilder`'s own
/// `discovered_capability` (harness-engine/src/session_builder.rs)
/// projects a registered tool's model-facing `name` from its `id`, not
/// its `.name` field (an established, workspace-wide convention) -- so
/// `id` is what every provider's tool-name pattern actually constrains.
/// `id` must equal `.name` here (both `"web_fetch"`) so the two
/// conventions ("id is the real wire identifier" and "descriptor.name
/// documents what it's called") never diverge again the way they did
/// when `id` was `"web.fetch"` (a `.`, invalid for Anthropic/OpenAI) and
/// `.name` alone was fixed to `"web_fetch"` without the `id` also
/// changing.
#[test]
fn descriptor_id_and_name_are_both_a_valid_provider_tool_name() {
    let descriptor = FetchTool::new().descriptor();
    assert_eq!(descriptor.id.as_str(), "web_fetch");
    assert_eq!(descriptor.name, "web_fetch");
    assert_eq!(
        descriptor.id.as_str(),
        descriptor.name,
        "id and name must match -- discovered_capability sends id as the model-facing name"
    );
}

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
