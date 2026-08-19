//! Per-backend concurrency and sliding-window rate limiting.
//!
//! [`BackendLimiters`] is the single subsystem responsible for everything
//! that is specific to one [`BackendId`]: its own concurrency ceiling and
//! its own `requests_per_minute` / `tokens_per_minute` sliding window. It is
//! deliberately independent of the global [`GlobalPermits`](crate::scheduler::permits::GlobalPermits)
//! ceilings — a request must hold a permit from *both* systems before it may
//! proceed.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Instant;

use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;

use harness_protocol::ids::BackendId;

use crate::scheduler::rate_window::RateWindow;

// ---------------------------------------------------------------------------
// BackendRateLimits
// ---------------------------------------------------------------------------

/// Per-backend rate and concurrency limits for the [`BackendLimiters`] system.
///
/// These limits are applied **in addition** to the global backend concurrency
/// ceiling ([`SchedulerConfig::max_concurrent_backend_requests`](crate::scheduler::SchedulerConfig::max_concurrent_backend_requests)).
/// A request must hold both the global permit and the backend-specific
/// permit before it may proceed.
#[derive(Debug, Clone)]
pub struct BackendRateLimits {
    /// Maximum number of concurrent requests to this backend.
    pub max_concurrent_requests: usize,
    /// Maximum number of requests per minute (sliding window).
    ///
    /// `None` means unlimited.
    pub requests_per_minute: Option<u32>,
    /// Maximum number of tokens per minute (sliding window).
    ///
    /// `None` means unlimited.
    pub tokens_per_minute: Option<u64>,
}

// ---------------------------------------------------------------------------
// BackendLimiterState — per-backend runtime state
// ---------------------------------------------------------------------------

/// Runtime state for a single backend's concurrency semaphore and sliding-window
/// rate bookkeeping.
struct BackendLimiterState {
    /// Per-backend concurrency semaphore.
    concurrency: Arc<Semaphore>,
    /// Sliding-window rate bookkeeping (requests_per_minute / tokens_per_minute).
    window: Mutex<RateWindow>,
    /// The limits that were configured for this backend (used by the acquire
    /// method to inspect rate limit parameters without an extra lookup).
    limits: BackendRateLimits,
}

// ---------------------------------------------------------------------------
// BackendPermitGuard — RAII guard for backend-specific permits
// ---------------------------------------------------------------------------

/// A RAII guard that holds a backend-specific concurrency permit.
///
/// When this guard is dropped the permit is automatically released, freeing
/// one slot in the backend's concurrency semaphore.
///
/// # No-op variant
///
/// If no limits have been configured for a backend (via
/// [`Scheduler::configure_backend_limits`](crate::scheduler::Scheduler::configure_backend_limits)),
/// [`Scheduler::acquire_backend_specific_permit`](crate::scheduler::Scheduler::acquire_backend_specific_permit)
/// returns a no-op guard that holds no permit and does nothing on drop.
///
/// # Use with the global permit
///
/// `BackendPermitGuard` only covers the *backend-specific* concurrency slot.
/// The caller must **also** hold the global permit obtained from
/// [`Scheduler::acquire_backend_permit`](crate::scheduler::Scheduler::acquire_backend_permit).
/// Both permits are required before a request may proceed.
pub struct BackendPermitGuard {
    _concurrency_permit: Option<OwnedSemaphorePermit>,
}

impl BackendPermitGuard {
    fn new(permit: OwnedSemaphorePermit) -> Self {
        Self {
            _concurrency_permit: Some(permit),
        }
    }

    fn noop() -> Self {
        Self {
            _concurrency_permit: None,
        }
    }
}

// ---------------------------------------------------------------------------
// BackendLimiters — collection of all per-backend limiters
// ---------------------------------------------------------------------------

/// Collection of per-backend concurrency and rate limiters, indexed by
/// [`BackendId`].
///
/// Internally backed by a `RwLock<HashMap<…>>`.  Lookups are cheap (clone an
/// `Arc`) and the lock is never held across await points.
pub(crate) struct BackendLimiters {
    limiters: RwLock<HashMap<BackendId, Arc<BackendLimiterState>>>,
}

impl BackendLimiters {
    pub(crate) fn new() -> Self {
        Self {
            limiters: RwLock::new(HashMap::new()),
        }
    }

    /// Configure per-backend concurrency and rate limits for the given
    /// [`BackendId`].
    ///
    /// If limits for this backend were previously configured they are
    /// replaced atomically.  Backends without configured limits are not
    /// throttled by the backend-specific system — only the global backend
    /// semaphore applies to them.
    ///
    /// This method is synchronous and can be called before any async runtime
    /// is started.
    pub(crate) fn configure(&self, backend: BackendId, limits: BackendRateLimits) {
        let state = Arc::new(BackendLimiterState {
            concurrency: Arc::new(Semaphore::new(limits.max_concurrent_requests)),
            window: Mutex::new(RateWindow::new()),
            limits,
        });
        let mut guard = self
            .limiters
            .write()
            .expect("BackendLimiters RwLock poisoned");
        guard.insert(backend, state);
    }

    /// Acquire a backend-specific permit for the given [`BackendId`].
    ///
    /// If no limits have been configured for `backend`, returns a no-op
    /// guard immediately.
    ///
    /// # Rate-limit back-pressure
    ///
    /// When the configured `requests_per_minute` or `tokens_per_minute` limit
    /// has been reached, this method **blocks** (sleeps in 100 ms intervals)
    /// until the sliding window allows a new request.  The concurrency
    /// semaphore permit is not acquired until the rate window check passes,
    /// so waiting for the rate limit does not occupy a concurrency slot.
    ///
    /// # Cancellation
    ///
    /// When `cancel` is `Some`, returns `None` if it fires while waiting on
    /// either the sliding-window rate limit or the per-backend concurrency
    /// semaphore. When `cancel` is `None` this always resolves to `Some`.
    pub(crate) async fn acquire(
        &self,
        backend: BackendId,
        cancel: Option<&CancellationToken>,
    ) -> Option<BackendPermitGuard> {
        // Look up the per-backend state.  If none is configured we return
        // a no-op guard immediately.
        let state = {
            let guard = self
                .limiters
                .read()
                .expect("BackendLimiters RwLock poisoned");
            guard.get(&backend).cloned()
        };

        let state = match state {
            Some(s) => s,
            None => return Some(BackendPermitGuard::noop()),
        };

        // Wait until the sliding-window rate limit allows a new request.
        //
        // We do NOT hold the concurrency semaphore permit while waiting
        // for the rate window so that waiting does not consume a slot.
        loop {
            if let Some(cancel) = cancel {
                if cancel.is_cancelled() {
                    return None;
                }
            }

            let can_proceed = {
                let mut window = state
                    .window
                    .lock()
                    .expect("BackendLimiterState rate window mutex poisoned");
                window.check_and_record(
                    Instant::now(),
                    0, // token count is unknown at permit-acquisition time
                    state.limits.requests_per_minute,
                    state.limits.tokens_per_minute,
                )
            };

            if can_proceed {
                break;
            }

            if let Some(cancel) = cancel {
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => return None,
                    _ = tokio::time::sleep(std::time::Duration::from_millis(100)) => {}
                }
            } else {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        }

        // Acquire a concurrency slot for this backend.
        let concurrency = state.concurrency.clone();
        let permit = if let Some(cancel) = cancel {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => return None,
                permit = concurrency.acquire_owned() => permit,
            }
        } else {
            concurrency.acquire_owned().await
        }
        .expect("Backend concurrency semaphore should never be closed");

        Some(BackendPermitGuard::new(permit))
    }
}
