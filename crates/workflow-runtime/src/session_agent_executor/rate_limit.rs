//! A rate-limited step. A short limit is waited out before the step is
//! retried, so its retries are not spent within a second. A usage limit that
//! lasts longer (a subscription's 5-hour window) fails the step at once with
//! the time it resets: retrying cannot help, and the user can continue the run
//! then.

use std::time::Duration;

use tokio_util::sync::CancellationToken;

/// The longest wait before a rate-limited step is retried. A provider that
/// asks for longer has hit a usage limit.
pub(crate) const MAX_STEP_WAIT: Duration = Duration::from_secs(120);

/// The wait when the provider did not say how long.
const DEFAULT_WAIT: Duration = Duration::from_secs(30);

/// The error code of a step that failed on a usage limit.
pub(crate) const USAGE_LIMIT: &str = "usage_limit";

/// The provider's advertised wait in a `RateLimited { retry_after: Some(…) }`
/// message: a `Duration` in its debug form (`16743s`, `1.5s`, `250ms`), or the
/// whole seconds `ExecutionError::RateLimited` carries (`8200`).
pub(crate) fn advertised_wait(message: &str) -> Option<Duration> {
    let value = message.split("Some(").nth(1)?.split(')').next()?.trim();
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }
    let at = value.find(|c: char| !(c.is_ascii_digit() || c == '.'))?;
    let (number, unit) = value.split_at(at);
    let number: f64 = number.parse().ok()?;
    let seconds = match unit {
        "s" => number,
        "ms" => number / 1e3,
        "µs" | "us" => number / 1e6,
        "ns" => number / 1e9,
        _ => return None,
    };
    Some(Duration::from_secs_f64(seconds))
}

/// The message of a step stopped by a usage limit that lasts `wait`. The
/// machine-readable `resets_at=` lets the IDE offer to continue at that time.
pub(crate) fn usage_limit_message(wait: Duration) -> String {
    let resets = chrono::Utc::now() + chrono::Duration::from_std(wait).unwrap_or_default();
    format!(
        "The model provider's usage limit was reached. It accepts requests again in about {}, at {} (resets_at={}). Continue the run then: finished tasks are kept.",
        in_words(wait),
        resets.format("%H:%M UTC"),
        resets.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    )
}

fn in_words(wait: Duration) -> String {
    let minutes = wait.as_secs().div_ceil(60);
    if minutes >= 60 {
        format!("{} h {} min", minutes / 60, minutes % 60)
    } else {
        format!("{minutes} min")
    }
}

/// Whether `error` is the model provider interrupting the step (a rate or usage limit, a
/// timeout) rather than anything wrong with the work: such a failure is no reason for the next
/// attempt to change what it does, so it is never shown to a step as a rejection.
pub(crate) fn is_transport_failure(error: &rusty_workflow_core::OrchestrationError) -> bool {
    use rusty_workflow_core::RetryReason;
    error.code == USAGE_LIMIT
        || matches!(
            error.retry_reason,
            Some(RetryReason::BackendRateLimited | RetryReason::BackendTimeout)
        )
}

/// Waits before a step that hit a short rate limit is retried: the
/// provider's advertised wait, else half a minute, at most [`MAX_STEP_WAIT`].
pub(crate) async fn wait_before_retry(message: &str, cancel: &CancellationToken) {
    let wait = advertised_wait(message)
        .unwrap_or(DEFAULT_WAIT)
        .min(MAX_STEP_WAIT);
    tokio::select! {
        _ = tokio::time::sleep(wait) => {}
        _ = cancel.cancelled() => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_advertised_wait_is_read_from_the_error() {
        let wait = |text: &str| advertised_wait(text);
        assert_eq!(
            wait("RateLimited { retry_after: Some(16743s) }"),
            Some(Duration::from_secs(16743))
        );
        assert_eq!(
            wait("RateLimited { retry_after: Some(250ms) }"),
            Some(Duration::from_millis(250))
        );
        assert_eq!(
            wait("RateLimited { retry_after: Some(1.5s) }"),
            Some(Duration::from_millis(1500))
        );
        assert_eq!(wait("RateLimited { retry_after: None }"), None);
        // `ExecutionError::RateLimited` carries whole seconds, without a unit.
        assert_eq!(
            wait("RateLimited { retry_after: Some(8200) }"),
            Some(Duration::from_secs(8200))
        );
    }

    #[test]
    fn only_provider_interruptions_are_transport_failures() {
        use rusty_workflow_core::{OrchestrationError, RetryReason};
        let mut limited = OrchestrationError::new("BACKEND_ERROR", "RateLimited");
        limited.retry_reason = Some(RetryReason::BackendRateLimited);
        assert!(is_transport_failure(&limited));
        assert!(is_transport_failure(&OrchestrationError::new(
            USAGE_LIMIT,
            "resets"
        )));
        assert!(!is_transport_failure(&OrchestrationError::new(
            "verification_failed",
            "C1 unmet"
        )));
    }

    #[test]
    fn a_usage_limit_says_when_it_resets() {
        let message = usage_limit_message(Duration::from_secs(4 * 3600 + 39 * 60));
        assert!(message.contains("about 4 h 39 min"), "{message}");
        assert!(message.contains("resets_at=20"), "{message}");
        assert_eq!(in_words(Duration::from_secs(90)), "2 min");
    }
}
