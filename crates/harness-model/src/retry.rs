//! Provider-neutral normalization of HTTP retry-hint headers.
//!
//! Providers advertise a suggested retry delay in different ways: a plain
//! `Retry-After` header expressed in whole seconds (per RFC 9110), or a
//! provider-specific millisecond-precision header (e.g. Anthropic's
//! `retry-after-ms`). This module normalizes both shapes into a single
//! [`Duration`] so that [`crate::events::ModelError::RateLimited`] never
//! leaks a provider-specific time unit.

use std::time::Duration;

/// Normalize a provider's retry-hint headers into a [`Duration`].
///
/// `lookup` resolves a lowercase header name to its raw string value. This
/// keeps the parser independent of any particular HTTP client's header map
/// type, so callers can adapt it to `reqwest::HeaderMap` or any other
/// representation without this crate depending on an HTTP client.
///
/// Millisecond-precision hints (checked first via `retry-after-ms`) take
/// precedence over the standard, second-precision `retry-after` header when
/// both are present. Malformed or absent values normalize to `None` rather
/// than an error, since a missing retry hint is not itself a failure.
pub fn parse_retry_after<'a>(lookup: impl Fn(&str) -> Option<&'a str>) -> Option<Duration> {
    if let Some(ms) = lookup("retry-after-ms").and_then(|value| value.trim().parse::<u64>().ok()) {
        return Some(Duration::from_millis(ms));
    }
    lookup("retry-after")
        .and_then(|value| value.trim().parse::<u64>().ok())
        .map(Duration::from_secs)
}

#[cfg(test)]
mod tests {
    use super::parse_retry_after;
    use std::time::Duration;

    fn lookup_from<'a>(headers: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<&'a str> {
        move |name: &str| headers.iter().find(|(k, _)| *k == name).map(|(_, v)| *v)
    }

    #[test]
    fn absent_headers_normalize_to_none() {
        assert_eq!(parse_retry_after(|_| None), None);
    }

    #[test]
    fn malformed_values_normalize_to_none() {
        let headers = [("retry-after", "soon")];
        assert_eq!(parse_retry_after(lookup_from(&headers)), None);
    }

    #[test]
    fn standard_seconds_header_is_normalized() {
        let headers = [("retry-after", "3")];
        assert_eq!(parse_retry_after(lookup_from(&headers)), Some(Duration::from_secs(3)));
    }

    #[test]
    fn millisecond_header_is_normalized() {
        let headers = [("retry-after-ms", "1250")];
        assert_eq!(parse_retry_after(lookup_from(&headers)), Some(Duration::from_millis(1250)));
    }

    #[test]
    fn millisecond_header_takes_precedence_over_seconds() {
        let headers = [("retry-after-ms", "500"), ("retry-after", "10")];
        assert_eq!(parse_retry_after(lookup_from(&headers)), Some(Duration::from_millis(500)));
    }

    #[test]
    fn whitespace_around_values_is_tolerated() {
        let headers = [("retry-after", " 7 ")];
        assert_eq!(parse_retry_after(lookup_from(&headers)), Some(Duration::from_secs(7)));
    }
}
