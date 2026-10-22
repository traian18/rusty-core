//! Run time that leaves out time spent waiting for the user.

use std::time::Duration;

use harness_core::orchestration::{error_codes, OrchestrationError};
use tokio::time::Instant;

pub(super) struct RunClock {
    /// Elapsed time carried over from before this invocation (a resumed run).
    elapsed_before_ms: u64,
    started: Instant,
    /// Time this invocation spent waiting for the user to answer a question;
    /// it does not count against the elapsed budget.
    waited: Duration,
    waiting_since: Option<Instant>,
}

impl RunClock {
    pub(super) fn new(elapsed_before_ms: u64) -> Self {
        Self {
            elapsed_before_ms,
            started: Instant::now(),
            waited: Duration::ZERO,
            waiting_since: None,
        }
    }

    /// Run time so far, leaving out time spent waiting for the user.
    pub(super) fn elapsed_ms(&self) -> u64 {
        let waiting = self
            .waiting_since
            .map_or(Duration::ZERO, |since| since.elapsed());
        let working = self.started.elapsed().saturating_sub(self.waited + waiting);
        self.elapsed_before_ms + working.as_millis() as u64
    }

    /// Start or stop the clock that excludes waiting for the user.
    pub(super) fn track_input_wait(&mut self, waiting: bool) {
        match (waiting, self.waiting_since) {
            (true, None) => self.waiting_since = Some(Instant::now()),
            (false, Some(since)) => {
                self.waited += since.elapsed();
                self.waiting_since = None;
            }
            _ => {}
        }
    }

    /// When the elapsed budget runs out. None without a limit, or while the
    /// run waits for the user.
    pub(super) fn deadline(&self, limit_ms: Option<u64>) -> Option<Instant> {
        let limit = limit_ms?;
        if self.waiting_since.is_some() {
            return None;
        }
        let remaining = limit.saturating_sub(self.elapsed_ms());
        Some(Instant::now() + Duration::from_millis(remaining))
    }

    /// The abort error once a running run has used its elapsed budget.
    pub(super) fn exhausted(
        &self,
        limit_ms: Option<u64>,
        terminal: bool,
    ) -> Option<OrchestrationError> {
        let limit = limit_ms?;
        let elapsed = self.elapsed_ms();
        (elapsed >= limit && !terminal).then(|| {
            OrchestrationError::new(
                error_codes::BUDGET_EXHAUSTED,
                format!("elapsed budget of {limit}ms exhausted ({elapsed}ms used)"),
            )
        })
    }
}
