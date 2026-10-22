//! Model-layer results and errors translated into execution-protocol events.

use std::time::Duration;

use tokio::sync::broadcast;

use harness_model::events::{ModelError, ModelResult};
use harness_protocol::backend::{ExecutionError, ExecutionEvent, ExecutionResult};
use harness_protocol::ids::RequestId;

pub(super) fn to_execution_result(request_id: RequestId, result: ModelResult) -> ExecutionResult {
    ExecutionResult {
        request_id,
        usage: result.usage,
        cost: result.cost,
        // The agent loop uses `tool_use` to keep the run alive until tool
        // results arrive. Chat Completions uses `tool_calls` instead; passing
        // it through would finish the run while its tools are still running.
        finish_reason: match result.stop_reason.as_str() {
            "tool_calls" | "function_call" => "tool_use".to_string(),
            _ => result.stop_reason,
        },
    }
}

/// Kept as one exhaustive match so a new `ModelError` variant fails to
/// compile here instead of falling into a catch-all.
pub(super) fn to_execution_error(error: ModelError) -> ExecutionError {
    match error {
        ModelError::BackendError { message, code } => {
            ExecutionError::BackendError { message, code }
        }
        ModelError::RateLimited { retry_after } => ExecutionError::RateLimited {
            retry_after: retry_after.map(whole_seconds_rounded_up),
        },
        ModelError::InvalidRequest { message } => ExecutionError::InvalidRequest { message },
        ModelError::Cancelled => ExecutionError::Cancelled,
        ModelError::Timeout => ExecutionError::Timeout,
        ModelError::CircuitOpen { retry_after } => coded_backend_error(
            format!(
                "provider circuit is open; retry after {} ms",
                retry_after.as_millis()
            ),
            "CIRCUIT_OPEN",
        ),
        ModelError::Protocol { message } => coded_backend_error(message, "PROTOCOL_ERROR"),
        ModelError::StreamInterrupted { message } => {
            coded_backend_error(message, "STREAM_INTERRUPTED")
        }
        ModelError::UnsupportedCapability { capability, detail } => {
            ExecutionError::UnsupportedCapability { capability, detail }
        }
    }
}

fn whole_seconds_rounded_up(delay: Duration) -> u64 {
    let millis = delay.as_millis().min(u128::from(u64::MAX)) as u64;
    millis.saturating_add(999) / 1_000
}

fn coded_backend_error(message: String, code: &str) -> ExecutionError {
    ExecutionError::BackendError {
        message,
        code: code.to_string(),
    }
}

pub(super) fn emit_terminal(
    sink: &broadcast::Sender<ExecutionEvent>,
    request_id: RequestId,
    result: &Result<ExecutionResult, ExecutionError>,
) {
    let event = match result {
        Ok(result) => ExecutionEvent::Completed {
            request_id,
            result: result.clone(),
        },
        Err(error) => ExecutionEvent::Error {
            request_id,
            error: error.clone(),
        },
    };
    send_event(sink, event);
}

pub(super) fn send_event(sink: &broadcast::Sender<ExecutionEvent>, event: ExecutionEvent) {
    // Broadcast delivery is best-effort: no subscribers is expected when the
    // caller only awaits the returned result.
    let _ = sink.send(event);
}
