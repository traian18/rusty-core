//! Concurrency throttling for agent sessions using Tokio semaphores.
//!
//! [`Scheduler`] owns five independent semaphores that cap:
//!
//! * The number of **sessions** that can be active simultaneously.
//! * The number of **agents** (runners) that can be live across all sessions.
//! * The number of **concurrent backend (LLM) requests** in flight.
//! * The number of **concurrent tool executions** in flight.
//! * The number of **concurrent child processes** in flight.
//!
//! In addition to the global concurrency ceilings, the scheduler provides
//! a [`BackendLimiters`](backend_limiter::BackendLimiters) system that
//! enforces per-backend concurrency and sliding-window rate limits
//! (requests per minute, tokens per minute).
//!
//! Each semaphore is backed by a `tokio::sync::Semaphore` and exposes
//! `acquire_*_permit` methods that return an [`OwnedSemaphorePermit`].
//! The permit is `Drop`-based: when it goes out of scope the semaphore slot
//! is automatically released, so callers never need to remember to release it
//! manually.
//!
//! # Cancellation
//!
//! The plain `acquire_*_permit` methods wait unconditionally for a slot.
//! Callers that must remain cancellable while queued for a permit (an M2
//! correctness requirement: "cancel during scheduler wait") should use the
//! `*_cancellable` variants instead. These race the acquire against a
//! [`CancellationToken`] and return `None` if cancellation wins before a
//! permit was obtained, without ever holding a permit that is then
//! immediately dropped.
//!
//! # Architecture & SOLID Principles
//!
//! The scheduler is composed of small, independently testable subsystems
//! instead of one struct holding all the state and logic (Single
//! Responsibility Principle):
//!
//! - **Configuration & errors ([`config`])**: [`SchedulerConfig`] capacities
//!   and the typed [`CapacityError`] bounded-wait rejection.
//! - **Global ceilings ([`permits`])**: [`permits::GlobalPermits`] owns the
//!   five capacity semaphores and all acquire/try-acquire/cancellable logic
//!   for them, plus the [`PermitSnapshot`]/[`SchedulerSnapshot`] diagnostics
//!   types.
//! - **Per-backend throttling ([`backend_limiter`])**: [`backend_limiter::BackendLimiters`]
//!   owns per-[`BackendId`] concurrency semaphores and sliding-window rate
//!   bookkeeping ([`rate_window`]), entirely independent of the global
//!   ceilings — a request needs a permit from both systems.
//! - **Metrics ([`metrics`])**: one shared instrumentation path
//!   (`record_acquired`/`record_cancelled`) reused by every acquire method
//!   instead of duplicating it per permit kind.
//! - **Scheduler ([`Scheduler`])**: a thin facade that composes the above
//!   subsystems and exposes the stable public API — every method signature
//!   below is unchanged from before this decomposition, so existing callers
//!   and tests require no changes.
//!
//! # Construction
//!
//! ```ignore
//! let config = SchedulerConfig {
//!     max_active_sessions: 16,
//!     ..Default::default()
//! };
//! let scheduler = Arc::new(Scheduler::new(config));
//! ```

use std::sync::Arc;

use tokio::sync::OwnedSemaphorePermit;
use tokio_util::sync::CancellationToken;

use harness_protocol::ids::BackendId;

pub mod backend_limiter;
pub mod config;
mod metrics;
pub mod permits;
mod rate_window;

pub use backend_limiter::{BackendPermitGuard, BackendRateLimits};
pub use config::{CapacityError, SchedulerConfig};
pub use permits::{PermitSnapshot, SchedulerSnapshot};

use backend_limiter::BackendLimiters;
use permits::GlobalPermits;

#[cfg(test)]
mod cancellation_tests;

// ---------------------------------------------------------------------------
// Scheduler
// ---------------------------------------------------------------------------

/// Top-level concurrency throttle for the harness runtime.
///
/// A thin facade composing [`GlobalPermits`] (the five capacity semaphores)
/// and [`BackendLimiters`] (per-backend concurrency and rate limiting).
/// Every method below acquires an [`OwnedSemaphorePermit`] (or a
/// [`BackendPermitGuard`]) that is independent of any borrow on `self`,
/// so permits can be moved into spawned [`tokio::spawn`] tasks without
/// lifetime headaches.
pub struct Scheduler {
    permits: GlobalPermits,
    backend_limiters: BackendLimiters,
}

impl Scheduler {
    /// Creates a new `Scheduler` with the given capacities.
    pub fn new(config: SchedulerConfig) -> Self {
        Self {
            permits: GlobalPermits::new(config),
            backend_limiters: BackendLimiters::new(),
        }
    }

    /// Acquires a permit for creating a new session.
    ///
    /// Blocks until the number of active sessions is below
    /// [`SchedulerConfig::max_active_sessions`].
    pub async fn acquire_session_permit(self: &Arc<Self>) -> OwnedSemaphorePermit {
        self.permits.acquire_session().await
    }

    /// E1: bounded-wait variant of [`Self::acquire_session_permit`] — waits
    /// up to `self.config.admission_timeout` for a slot, then rejects typed
    /// (`CapacityError`) instead of queueing indefinitely. This is the
    /// method `SessionManager::create_session` actually calls; the plain
    /// unconditional-wait `acquire_session_permit` remains available for
    /// callers (mostly tests) that genuinely want to wait forever.
    pub async fn try_acquire_session_permit(
        self: &Arc<Self>,
    ) -> Result<OwnedSemaphorePermit, CapacityError> {
        self.permits.try_acquire_session().await
    }

    /// Acquires a permit for spawning a new agent runner.
    pub async fn acquire_agent_permit(self: &Arc<Self>) -> OwnedSemaphorePermit {
        self.permits.acquire_agent().await
    }

    /// Acquires a permit for executing a backend (LLM) request.
    ///
    /// Blocks until the number of in-flight backend requests is below
    /// [`SchedulerConfig::max_concurrent_backend_requests`].
    pub async fn acquire_backend_permit(self: &Arc<Self>) -> OwnedSemaphorePermit {
        self.permits.acquire_backend().await
    }

    /// Cancellable variant of [`Self::acquire_backend_permit`].
    ///
    /// Returns `None` if `cancel` fires before a permit becomes available.
    pub async fn acquire_backend_permit_cancellable(
        self: &Arc<Self>,
        cancel: &CancellationToken,
    ) -> Option<OwnedSemaphorePermit> {
        self.permits.acquire_backend_cancellable(cancel).await
    }

    /// Acquires a permit for executing a tool.
    ///
    /// Blocks until the number of in-flight tool executions is below
    /// [`SchedulerConfig::max_concurrent_tool_executions`].
    pub async fn acquire_tool_permit(self: &Arc<Self>) -> OwnedSemaphorePermit {
        self.permits.acquire_tool().await
    }

    /// Cancellable variant of [`Self::acquire_tool_permit`].
    ///
    /// Returns `None` if `cancel` fires before a permit becomes available.
    pub async fn acquire_tool_permit_cancellable(
        self: &Arc<Self>,
        cancel: &CancellationToken,
    ) -> Option<OwnedSemaphorePermit> {
        self.permits.acquire_tool_cancellable(cancel).await
    }

    /// Acquires a permit for spawning a child process.
    pub async fn acquire_process_permit(self: &Arc<Self>) -> OwnedSemaphorePermit {
        self.permits.acquire_process().await
    }

    /// Cancellable variant of [`Self::acquire_process_permit`].
    ///
    /// Returns `None` if `cancel` fires before a permit becomes available.
    pub async fn acquire_process_permit_cancellable(
        self: &Arc<Self>,
        cancel: &CancellationToken,
    ) -> Option<OwnedSemaphorePermit> {
        self.permits.acquire_process_cancellable(cancel).await
    }

    /// Point-in-time snapshot of every permit kind's capacity/in-use/queued
    /// state, for the M6 diagnostics RPC (`GetDiagnostics`) — a
    /// non-Prometheus, directly-queryable view of the same underlying
    /// semaphores the metrics above report on, useful for a host that wants
    /// current saturation without scraping/parsing metrics text.
    pub fn snapshot(&self) -> SchedulerSnapshot {
        self.permits.snapshot()
    }

    // -----------------------------------------------------------------------
    // Backend-specific limiting
    // -----------------------------------------------------------------------

    /// Configure per-backend concurrency and rate limits for the given
    /// [`BackendId`].
    ///
    /// If limits for this backend were previously configured they are
    /// replaced atomically.  Backends without configured limits are not
    /// throttled by the backend-specific system — only the global backend
    /// semaphore (from [`SchedulerConfig::max_concurrent_backend_requests`])
    /// applies to them.
    ///
    /// This method is synchronous and can be called before any async runtime
    /// is started.
    pub fn configure_backend_limits(&self, backend: BackendId, limits: BackendRateLimits) {
        self.backend_limiters.configure(backend, limits);
    }

    /// Acquire a backend-specific permit for the given [`BackendId`].
    ///
    /// **Must be called in addition to** [`Self::acquire_backend_permit`] — both the
    /// global semaphore permit and the backend-specific permit are required
    /// before a request may proceed.
    ///
    /// If no limits have been configured for `backend` via
    /// [`Self::configure_backend_limits`], this method returns a no-op guard
    /// immediately.
    ///
    /// # Rate-limit back-pressure
    ///
    /// When the configured `requests_per_minute` or `tokens_per_minute` limit
    /// has been reached, this method **blocks** (sleeps in 100 ms intervals)
    /// until the sliding window allows a new request.  The concurrency
    /// semaphore permit is not acquired until the rate window check passes,
    /// so waiting for the rate limit does not occupy a concurrency slot.
    pub async fn acquire_backend_specific_permit(
        self: &Arc<Self>,
        backend: BackendId,
    ) -> BackendPermitGuard {
        self.backend_limiters
            .acquire(backend, None)
            .await
            .expect("uncancellable acquire never returns None")
    }

    /// Cancellable variant of [`Self::acquire_backend_specific_permit`].
    ///
    /// Returns `None` if `cancel` fires while waiting on either the
    /// sliding-window rate limit or the per-backend concurrency semaphore.
    pub async fn acquire_backend_specific_permit_cancellable(
        self: &Arc<Self>,
        backend: BackendId,
        cancel: &CancellationToken,
    ) -> Option<BackendPermitGuard> {
        self.backend_limiters.acquire(backend, Some(cancel)).await
    }
}
