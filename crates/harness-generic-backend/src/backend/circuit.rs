//! Circuit breaker shared by every request on one backend instance.

use std::sync::Mutex;
use std::time::Duration;

use harness_model::events::ModelError;

use super::RecoveryPolicy;

#[derive(Debug, Default)]
struct CircuitState {
    consecutive_failures: u32,
    open_until: Option<tokio::time::Instant>,
    half_open_probe_in_flight: bool,
}

/// Opens after `failure_threshold` consecutive transient failures, fails fast
/// for `open_duration`, then lets exactly one probe request through.
pub(super) struct CircuitBreaker {
    state: Mutex<CircuitState>,
    failure_threshold: u32,
    open_duration: Duration,
}

impl CircuitBreaker {
    pub(super) fn new(recovery: &RecoveryPolicy) -> Self {
        Self {
            state: Mutex::new(CircuitState::default()),
            failure_threshold: recovery.circuit_failure_threshold,
            open_duration: recovery.circuit_open_duration,
        }
    }

    pub(super) fn allow_request(&self) -> Result<(), ModelError> {
        let now = tokio::time::Instant::now();
        let mut state = self.lock();
        let Some(open_until) = state.open_until else {
            return Ok(());
        };
        if open_until > now {
            return Err(ModelError::CircuitOpen {
                retry_after: open_until.duration_since(now),
            });
        }
        if state.half_open_probe_in_flight {
            return Err(ModelError::CircuitOpen {
                retry_after: self.open_duration,
            });
        }
        state.half_open_probe_in_flight = true;
        Ok(())
    }

    pub(super) fn record_success(&self) {
        *self.lock() = CircuitState::default();
    }

    pub(super) fn record_failure(&self, error: &ModelError) {
        let mut state = self.lock();
        state.half_open_probe_in_flight = false;
        if !error.is_retryable() {
            return;
        }
        state.consecutive_failures += 1;
        if state.consecutive_failures >= self.failure_threshold {
            state.open_until = Some(tokio::time::Instant::now() + self.open_duration);
        }
    }

    pub(super) fn is_open(&self) -> bool {
        self.lock().open_until.is_some()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, CircuitState> {
        self.state.lock().expect("circuit mutex poisoned")
    }
}
