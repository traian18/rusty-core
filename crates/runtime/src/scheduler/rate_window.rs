//! Sliding-window rate bookkeeping used by per-backend rate limits.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// Sliding-window rate tracker.
///
/// Stores a deque of `(timestamp, token_count)` entries and provides
/// [`check_and_record`](RateWindow::check_and_record) which purges entries
/// older than 60 seconds, checks the configured limits, and records the new
/// entry if it falls within bounds.
pub(crate) struct RateWindow {
    entries: VecDeque<(Instant, u64)>,
}

impl RateWindow {
    pub(crate) fn new() -> Self {
        Self {
            entries: VecDeque::new(),
        }
    }

    /// Check whether a request with `tokens` falls within the configured
    /// rate limits and record it if so.
    ///
    /// Returns `true` if the request is within limits (and was recorded),
    /// `false` if it would exceed a limit (no entry is added).
    pub(crate) fn check_and_record(
        &mut self,
        now: Instant,
        tokens: u64,
        max_requests: Option<u32>,
        max_tokens: Option<u64>,
    ) -> bool {
        // Purge entries older than 60 seconds.
        let cutoff = now - Duration::from_secs(60);
        while let Some(&(t, _)) = self.entries.front() {
            if t < cutoff {
                self.entries.pop_front();
            } else {
                break;
            }
        }

        // Check request count limit.
        if let Some(max_req) = max_requests {
            if self.entries.len() >= max_req as usize {
                return false;
            }
        }

        // Check token sum limit.
        if let Some(max_tok) = max_tokens {
            let sum: u64 = self.entries.iter().map(|&(_, t)| t).sum();
            if sum.saturating_add(tokens) > max_tok {
                return false;
            }
        }

        // Record the new entry.
        self.entries.push_back((now, tokens));
        true
    }
}
