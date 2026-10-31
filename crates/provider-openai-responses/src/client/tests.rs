#[test]
fn a_usage_limit_body_says_when_it_resets() {
    use super::response::usage_limit_reset;
    use std::time::Duration;
    let body = r#"{"error":{"type":"usage_limit_reached","message":"The usage limit has been reached","resets_in_seconds":16743}}"#;
    assert_eq!(usage_limit_reset(body, 0), Some(Duration::from_secs(16743)));
    let body = r#"{"error":{"type":"usage_limit_reached","resets_at":1000}}"#;
    assert_eq!(usage_limit_reset(body, 400), Some(Duration::from_secs(600)));
    assert_eq!(usage_limit_reset("Too Many Requests", 0), None);
}

use super::response::retry_after_from_headers;
use std::time::Duration;

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
