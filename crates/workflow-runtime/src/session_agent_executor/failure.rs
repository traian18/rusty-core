//! Turning an agent failure into an execution error, with the retry reason when a retry can help.

use rusty_protocol::commands::AgentError;
use rusty_workflow_core::RetryReason;

use super::rate_limit;
use crate::orchestration::AgentExecutionError;

/// Maps an agent failure to a retry reason. Backend errors reach the agent as
/// `BACKEND_ERROR` with the `ExecutionError` debug text as the message, so
/// transient kinds are recognized by that prefix. Anything else — auth,
/// invalid requests, tool failures the agent could not recover from — is not
/// retried automatically.
pub(super) fn classify_failure(error: AgentError) -> AgentExecutionError {
    let reason = match error.code.as_str() {
        "BACKEND_ERROR" if error.message.starts_with("RateLimited") => {
            match rate_limit::advertised_wait(&error.message) {
                // A usage limit: no retry can help before it resets.
                Some(wait) if wait > rate_limit::MAX_STEP_WAIT => {
                    return AgentExecutionError {
                        code: rate_limit::USAGE_LIMIT.into(),
                        message: rate_limit::usage_limit_message(wait),
                        retry_reason: None,
                    };
                }
                _ => Some(RetryReason::BackendRateLimited),
            }
        }
        "BACKEND_ERROR" if error.message.starts_with("Timeout") => {
            Some(RetryReason::BackendTimeout)
        }
        "TOOL_TIMEOUT" => Some(RetryReason::ToolTimeout),
        _ => None,
    };
    AgentExecutionError {
        code: error.code,
        message: error.message,
        retry_reason: reason,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transient_backend_failures_are_retryable_and_others_are_not() {
        let error = |code: &str, message: &str| AgentError {
            code: code.into(),
            message: message.into(),
            details: None,
        };
        assert_eq!(
            classify_failure(error("BACKEND_ERROR", "RateLimited { retry_after: None }"))
                .retry_reason,
            Some(RetryReason::BackendRateLimited)
        );
        let usage_limit = classify_failure(error(
            "BACKEND_ERROR",
            "RateLimited { retry_after: Some(16743s) }",
        ));
        assert_eq!(usage_limit.retry_reason, None);
        assert_eq!(usage_limit.code, "usage_limit");
        assert!(usage_limit.message.contains("resets_at="));
        // As it reaches the agent from `ExecutionError` (whole seconds): the 2026-11-09 runs
        // retried this as a short limit three times.
        assert_eq!(
            classify_failure(error(
                "BACKEND_ERROR",
                "RateLimited { retry_after: Some(8200) }"
            ))
            .code,
            "usage_limit"
        );
        assert_eq!(
            classify_failure(error("BACKEND_ERROR", "Timeout")).retry_reason,
            Some(RetryReason::BackendTimeout)
        );
        assert_eq!(
            classify_failure(error(
                "BACKEND_ERROR",
                "InvalidRequest { message: \"bad key\" }"
            ))
            .retry_reason,
            None
        );
        assert_eq!(
            classify_failure(error("TOOL_FAILED", "Timeout")).retry_reason,
            None
        );
        assert_eq!(
            classify_failure(error("TOOL_TIMEOUT", "3 consecutive tool calls timed out"))
                .retry_reason,
            Some(RetryReason::ToolTimeout)
        );
    }
}
