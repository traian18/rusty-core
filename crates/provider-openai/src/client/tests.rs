use super::response::{
    generation_usage, is_openrouter_url, recover_openrouter_usage, retry_after_from_headers,
};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

#[test]
fn standard_retry_after_is_normalized_to_duration() {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(reqwest::header::RETRY_AFTER, "5".parse().unwrap());
    assert_eq!(
        retry_after_from_headers(&headers),
        Some(Duration::from_secs(5))
    );
}

#[test]
fn missing_header_normalizes_to_none() {
    let headers = reqwest::header::HeaderMap::new();
    assert_eq!(retry_after_from_headers(&headers), None);
}

#[test]
fn openrouter_generation_usage_waits_for_real_counters() {
    assert!(is_openrouter_url("https://openrouter.ai/api/v1"));
    assert!(!is_openrouter_url("https://openrouter.ai.evil.test/api/v1"));
    assert!(generation_usage(&serde_json::json!({
        "native_tokens_prompt": 0,
        "native_tokens_completion": 0
    }))
    .is_none());
    let usage = generation_usage(&serde_json::json!({
        "native_tokens_prompt": 0,
        "tokens_prompt": 1_000,
        "native_tokens_completion": 0,
        "tokens_completion": 40,
        "native_tokens_cached": 100,
        "native_tokens_reasoning": 10
    }))
    .unwrap();
    assert_eq!(usage.input_tokens.value(), Some(1_000));
    assert_eq!(usage.output_tokens.value(), Some(40));
    assert_eq!(usage.cache_read_tokens.value(), Some(100));
    assert_eq!(usage.reasoning_tokens.value(), Some(10));
    assert_eq!(usage.total_tokens.value(), Some(1_040));
}

#[tokio::test]
async fn generation_lookup_retries_until_openrouter_populates_usage() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        for attempt in 0..2 {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buffer = [0_u8; 4_096];
            let size = socket.read(&mut buffer).await.unwrap();
            let request = String::from_utf8_lossy(&buffer[..size]);
            assert!(request.starts_with("GET /api/v1/generation?id=gen-gemini HTTP/1.1"));
            let body = if attempt == 0 {
                r#"{"data":{"native_tokens_prompt":0,"native_tokens_completion":0}}"#
            } else {
                r#"{"data":{"native_tokens_prompt":200,"native_tokens_completion":30}}"#
            };
            let response = format!("HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len());
            socket.write_all(response.as_bytes()).await.unwrap();
        }
    });
    let usage = recover_openrouter_usage(
        &reqwest::Client::new(),
        &format!("http://{address}/api/v1"),
        "sk-test",
        "gen-gemini",
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(usage.total_tokens.value(), Some(230));
    server.await.unwrap();
}
