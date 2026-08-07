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
//! a [`BackendLimiters`] system that enforces per-backend concurrency and
//! sliding-window rate limits (requests per minute, tokens per minute).
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
//! # Construction
//!
//! ```ignore
//! let config = SchedulerConfig {
//!     max_active_sessions: 16,
//!     ..Default::default()
//! };
//! let scheduler = Arc::new(Scheduler::new(config));
//! ```

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::RwLock;
use std::time::{Duration, Instant};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;

use harness_protocol::ids::BackendId;

// ---------------------------------------------------------------------------
// SchedulerConfig
// ---------------------------------------------------------------------------

/// Configuration for [`Scheduler`] semaphore capacities.
///
/// Each field controls the maximum number of concurrent operations in its
/// category.  All fields have sensible defaults tuned for a single-user
/// development environment; production deployments may want to increase
/// `max_active_sessions` and `max_active_agents`.
#[derive(Debug, Clone)]
pub struct SchedulerConfig {
    /// Maximum number of sessions that can be active at once.
    pub max_active_sessions: usize,
    /// Maximum number of agent runners across all sessions.
    pub max_active_agents: usize,
    /// Maximum number of agent runners *per session* (soft ceiling enforced
    /// by the per-session agent semaphore).
    pub max_agents_per_session: usize,
    /// Maximum concurrent backend (LLM API) requests across all agents.
    pub max_concurrent_backend_requests: usize,
    /// Maximum concurrent tool executions across all agents.
    pub max_concurrent_tool_executions: usize,
    /// Maximum concurrent child processes spawned by tools.
    pub max_concurrent_processes: usize,
    /// E1: how long a bounded-wait admission point (currently
    /// `SessionManager::create_session`) waits for a permit before
    /// rejecting typed rather than queueing forever. Smooths over brief
    /// bursts without ever leaving a caller unable to tell "slow" apart
    /// from "will never complete" — see
    /// `docs/production-readiness-roadmap.md`'s E1 finding.
    pub admission_timeout: Duration,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            max_active_sessions: 64,
            max_active_agents: 256,
            max_agents_per_session: 32,
            max_concurrent_backend_requests: 8,
            max_concurrent_tool_executions: 16,
            max_concurrent_processes: 8,
            admission_timeout: Duration::from_secs(5),
        }
    }
}

/// E1: a bounded-wait admission attempt timed out before a permit became
/// available — the typed rejection callers get instead of an indefinite
/// block. `kind` matches the same `PermitKind::label` used for metrics
/// (`"session"`, `"agent"`, `"tool"`, `"backend"`, `"process"`), and `waited`
/// is always `>= ` the configured `admission_timeout` for that acquisition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("at capacity: no {kind} permit became available within {waited:?}")]
pub struct CapacityError {
    pub kind: &'static str,
    pub waited: Duration,
}

// ---------------------------------------------------------------------------
// BackendRateLimits
// ---------------------------------------------------------------------------

/// Per-backend rate and concurrency limits for the [`BackendLimiters`] system.
///
/// These limits are applied **in addition** to the global backend concurrency
/// ceiling ([`SchedulerConfig::max_concurrent_backend_requests`]).  A request
/// must hold both the global permit and the backend-specific permit before it
/// may proceed.
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
// RateWindow — sliding-window bookkeeping for requests_per_minute / tokens_per_minute
// ---------------------------------------------------------------------------

/// Sliding-window rate tracker.
///
/// Stores a deque of `(timestamp, token_count)` entries and provides
/// [`check_and_record`](RateWindow::check_and_record) which purges entries
/// older than 60 seconds, checks the configured limits, and records the new
/// entry if it falls within bounds.
struct RateWindow {
    entries: VecDeque<(Instant, u64)>,
}

impl RateWindow {
    fn new() -> Self {
        Self {
            entries: VecDeque::new(),
        }
    }

    /// Check whether a request with `tokens` falls within the configured
    /// rate limits and record it if so.
    ///
    /// Returns `true` if the request is within limits (and was recorded),
    /// `false` if it would exceed a limit (no entry is added).
    fn check_and_record(
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
// BackendLimiters — collection of all per-backend limiters
// ---------------------------------------------------------------------------

/// Collection of per-backend concurrency and rate limiters, indexed by
/// [`BackendId`].
///
/// Internally backed by a `RwLock<HashMap<…>>`.  Lookups are cheap (clone an
/// `Arc`) and the lock is never held across await points.
pub struct BackendLimiters {
    limiters: RwLock<HashMap<BackendId, Arc<BackendLimiterState>>>,
}

impl BackendLimiters {
    fn new() -> Self {
        Self {
            limiters: RwLock::new(HashMap::new()),
        }
    }
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
/// [`Scheduler::configure_backend_limits`]), [`acquire_backend_specific_permit`]
/// returns a no-op guard that holds no permit and does nothing on drop.
///
/// # Use with the global permit
///
/// `BackendPermitGuard` only covers the *backend-specific* concurrency slot.
/// The caller must **also** hold the global permit obtained from
/// [`Scheduler::acquire_backend_permit`].  Both permits are required before
/// a request may proceed.
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
// Scheduler
// ---------------------------------------------------------------------------

/// Top-level concurrency throttle for the harness runtime.
///
/// Each field is an `Arc<Semaphore>` so that permits can be acquired with
/// [`acquire_owned`](Semaphore::acquire_owned), yielding an
/// [`OwnedSemaphorePermit`] that is independent of any borrow on `self`.
/// This allows permits to be moved into spawned [`tokio::spawn`] tasks
/// without lifetime headaches.
/// One permit kind's point-in-time capacity/utilization, part of
/// [`SchedulerSnapshot`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PermitSnapshot {
    pub kind: &'static str,
    pub capacity: usize,
    pub in_use: usize,
}

/// Point-in-time snapshot of every [`Scheduler`] permit kind. See
/// [`Scheduler::snapshot`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchedulerSnapshot {
    pub permits: Vec<PermitSnapshot>,
}

pub struct Scheduler {
    /// Limits the number of concurrently active sessions.
    sessions: Arc<Semaphore>,
    /// Limits the number of concurrently active agent runners.
    agents: Arc<Semaphore>,
    /// Limits the number of concurrent backend (LLM) requests.
    backend_requests: Arc<Semaphore>,
    /// Limits the number of concurrent tool executions.
    tool_executions: Arc<Semaphore>,
    /// Limits the number of concurrent child processes.
    processes: Arc<Semaphore>,
    /// Per-backend rate and concurrency limiters.
    backend_limiters: BackendLimiters,
    /// Configured capacities, retained (beyond building the semaphores
    /// above) so `record_acquired`/`in_use_gauge` can report "in use" as
    /// `capacity - available_permits()` — `Semaphore` itself doesn't expose
    /// the value it was constructed with. M6: this is what backs the
    /// `harness_scheduler_permits_in_use` gauge.
    config: SchedulerConfig,
}

/// M6: metric-name/capacity pair for one permit kind, so every
/// `acquire_*_permit*` method emits the same three metrics
/// (`harness_scheduler_permit_wait_seconds`,
/// `harness_scheduler_permits_in_use`,
/// `harness_scheduler_permit_acquisitions_total`) with one shared helper
/// instead of five hand-duplicated instrumentation blocks — see
/// `Scheduler::record_acquired`.
struct PermitKind {
    label: &'static str,
    capacity: usize,
}

impl Scheduler {
    /// Creates a new `Scheduler` with the given capacities.
    pub fn new(config: SchedulerConfig) -> Self {
        Self {
            sessions: Arc::new(Semaphore::new(config.max_active_sessions)),
            agents: Arc::new(Semaphore::new(config.max_active_agents)),
            backend_requests: Arc::new(Semaphore::new(config.max_concurrent_backend_requests)),
            tool_executions: Arc::new(Semaphore::new(config.max_concurrent_tool_executions)),
            processes: Arc::new(Semaphore::new(config.max_concurrent_processes)),
            backend_limiters: BackendLimiters::new(),
            config,
        }
    }

    /// Records the three permit-acquisition metrics for one successful
    /// acquire: how long the caller waited, how many permits of this kind
    /// are now in use, and a cumulative acquisition count. `sem` is read
    /// *after* the permit was taken, so `available_permits()` already
    /// reflects this acquisition.
    fn record_acquired(kind: PermitKind, sem: &Semaphore, waited: Duration) {
        metrics::histogram!("harness_scheduler_permit_wait_seconds", "kind" => kind.label)
            .record(waited.as_secs_f64());
        metrics::gauge!("harness_scheduler_permits_in_use", "kind" => kind.label)
            .set((kind.capacity.saturating_sub(sem.available_permits())) as f64);
        metrics::counter!("harness_scheduler_permit_acquisitions_total", "kind" => kind.label)
            .increment(1);
    }

    /// Records a cancellable acquire that lost the race to cancellation —
    /// distinct from a successful acquisition so an operator can tell
    /// "queued and got in eventually" apart from "queued and gave up."
    fn record_cancelled(kind: PermitKind) {
        metrics::counter!("harness_scheduler_permit_wait_cancelled_total", "kind" => kind.label)
            .increment(1);
    }

    /// Acquires a permit for creating a new session.
    ///
    /// Blocks until the number of active sessions is below
    /// [`SchedulerConfig::max_active_sessions`].
    pub async fn acquire_session_permit(self: &Arc<Self>) -> OwnedSemaphorePermit {
        let start = Instant::now();
        let permit = self
            .sessions
            .clone()
            .acquire_owned()
            .await
            .expect("Scheduler semaphore should never be closed");
        Self::record_acquired(
            PermitKind { label: "session", capacity: self.config.max_active_sessions },
            &self.sessions,
            start.elapsed(),
        );
        permit
    }

    /// E1: bounded-wait variant of [`Self::acquire_session_permit`] — waits
    /// up to `self.config.admission_timeout` for a slot, then rejects typed
    /// (`CapacityError`) instead of queueing indefinitely. This is the
    /// method `SessionManager::create_session` actually calls; the plain
    /// unconditional-wait `acquire_session_permit` remains available for
    /// callers (mostly tests) that genuinely want to wait forever.
    pub async fn try_acquire_session_permit(self: &Arc<Self>) -> Result<OwnedSemaphorePermit, CapacityError> {
        let kind = PermitKind { label: "session", capacity: self.config.max_active_sessions };
        let start = Instant::now();
        let sem = self.sessions.clone();
        match tokio::time::timeout(self.config.admission_timeout, sem.acquire_owned()).await {
            Ok(permit) => {
                let permit = permit.expect("Scheduler semaphore should never be closed");
                Self::record_acquired(kind, &self.sessions, start.elapsed());
                Ok(permit)
            }
            Err(_) => {
                metrics::counter!("harness_scheduler_permit_admission_rejected_total", "kind" => kind.label)
                    .increment(1);
                Err(CapacityError { kind: kind.label, waited: start.elapsed() })
            }
        }
    }

    /// Acquires a permit for spawning a new agent runner.
    pub async fn acquire_agent_permit(self: &Arc<Self>) -> OwnedSemaphorePermit {
        let start = Instant::now();
        let permit = self
            .agents
            .clone()
            .acquire_owned()
            .await
            .expect("Scheduler semaphore should never be closed");
        Self::record_acquired(
            PermitKind { label: "agent", capacity: self.config.max_active_agents },
            &self.agents,
            start.elapsed(),
        );
        permit
    }

    /// Acquires a permit for executing a backend (LLM) request.
    ///
    /// Blocks until the number of in-flight backend requests is below
    /// [`SchedulerConfig::max_concurrent_backend_requests`].
    pub async fn acquire_backend_permit(self: &Arc<Self>) -> OwnedSemaphorePermit {
        let start = Instant::now();
        let permit = self
            .backend_requests
            .clone()
            .acquire_owned()
            .await
            .expect("Scheduler semaphore should never be closed");
        Self::record_acquired(
            PermitKind { label: "backend", capacity: self.config.max_concurrent_backend_requests },
            &self.backend_requests,
            start.elapsed(),
        );
        permit
    }

    /// Cancellable variant of [`Self::acquire_backend_permit`].
    ///
    /// Returns `None` if `cancel` fires before a permit becomes available.
    pub async fn acquire_backend_permit_cancellable(
        self: &Arc<Self>,
        cancel: &CancellationToken,
    ) -> Option<OwnedSemaphorePermit> {
        let start = Instant::now();
        let sem = self.backend_requests.clone();
        let kind = PermitKind { label: "backend", capacity: self.config.max_concurrent_backend_requests };
        tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                Self::record_cancelled(kind);
                None
            }
            permit = sem.acquire_owned() => {
                let permit = permit.expect("Scheduler semaphore should never be closed");
                Self::record_acquired(kind, &self.backend_requests, start.elapsed());
                Some(permit)
            }
        }
    }

    /// Acquires a permit for executing a tool.
    ///
    /// Blocks until the number of in-flight tool executions is below
    /// [`SchedulerConfig::max_concurrent_tool_executions`].
    pub async fn acquire_tool_permit(self: &Arc<Self>) -> OwnedSemaphorePermit {
        let start = Instant::now();
        let permit = self
            .tool_executions
            .clone()
            .acquire_owned()
            .await
            .expect("Scheduler semaphore should never be closed");
        Self::record_acquired(
            PermitKind { label: "tool", capacity: self.config.max_concurrent_tool_executions },
            &self.tool_executions,
            start.elapsed(),
        );
        permit
    }

    /// Cancellable variant of [`Self::acquire_tool_permit`].
    ///
    /// Returns `None` if `cancel` fires before a permit becomes available.
    pub async fn acquire_tool_permit_cancellable(
        self: &Arc<Self>,
        cancel: &CancellationToken,
    ) -> Option<OwnedSemaphorePermit> {
        let start = Instant::now();
        let sem = self.tool_executions.clone();
        let kind = PermitKind { label: "tool", capacity: self.config.max_concurrent_tool_executions };
        tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                Self::record_cancelled(kind);
                None
            }
            permit = sem.acquire_owned() => {
                let permit = permit.expect("Scheduler semaphore should never be closed");
                Self::record_acquired(kind, &self.tool_executions, start.elapsed());
                Some(permit)
            }
        }
    }

    /// Acquires a permit for spawning a child process.
    pub async fn acquire_process_permit(self: &Arc<Self>) -> OwnedSemaphorePermit {
        let start = Instant::now();
        let permit = self
            .processes
            .clone()
            .acquire_owned()
            .await
            .expect("Scheduler semaphore should never be closed");
        Self::record_acquired(
            PermitKind { label: "process", capacity: self.config.max_concurrent_processes },
            &self.processes,
            start.elapsed(),
        );
        permit
    }

    /// Cancellable variant of [`Self::acquire_process_permit`].
    ///
    /// Returns `None` if `cancel` fires before a permit becomes available.
    pub async fn acquire_process_permit_cancellable(
        self: &Arc<Self>,
        cancel: &CancellationToken,
    ) -> Option<OwnedSemaphorePermit> {
        let start = Instant::now();
        let sem = self.processes.clone();
        let kind = PermitKind { label: "process", capacity: self.config.max_concurrent_processes };
        tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                Self::record_cancelled(kind);
                None
            }
            permit = sem.acquire_owned() => {
                let permit = permit.expect("Scheduler semaphore should never be closed");
                Self::record_acquired(kind, &self.processes, start.elapsed());
                Some(permit)
            }
        }
    }

    /// Point-in-time snapshot of every permit kind's capacity/in-use/queued
    /// state, for the M6 diagnostics RPC (`GetDiagnostics`) — a
    /// non-Prometheus, directly-queryable view of the same underlying
    /// semaphores the metrics above report on, useful for a host that wants
    /// current saturation without scraping/parsing metrics text.
    pub fn snapshot(&self) -> SchedulerSnapshot {
        let permit = |label: &'static str, capacity: usize, sem: &Semaphore| PermitSnapshot {
            kind: label,
            capacity,
            in_use: capacity.saturating_sub(sem.available_permits()),
        };
        SchedulerSnapshot {
            permits: vec![
                permit("session", self.config.max_active_sessions, &self.sessions),
                permit("agent", self.config.max_active_agents, &self.agents),
                permit("backend", self.config.max_concurrent_backend_requests, &self.backend_requests),
                permit("tool", self.config.max_concurrent_tool_executions, &self.tool_executions),
                permit("process", self.config.max_concurrent_processes, &self.processes),
            ],
        }
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
        let state = Arc::new(BackendLimiterState {
            concurrency: Arc::new(Semaphore::new(limits.max_concurrent_requests)),
            window: Mutex::new(RateWindow::new()),
            limits,
        });
        let mut guard = self
            .backend_limiters
            .limiters
            .write()
            .expect("BackendLimiters RwLock poisoned");
        guard.insert(backend, state);
    }

    /// Acquire a backend-specific permit for the given [`BackendId`].
    ///
    /// **Must be called in addition to** [`acquire_backend_permit`] — both the
    /// global semaphore permit and the backend-specific permit are required
    /// before a request may proceed.
    ///
    /// If no limits have been configured for `backend` via
    /// [`configure_backend_limits`], this method returns a no-op guard
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
        self.acquire_backend_specific_permit_inner(backend, None)
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
        self.acquire_backend_specific_permit_inner(backend, Some(cancel))
            .await
    }

    async fn acquire_backend_specific_permit_inner(
        self: &Arc<Self>,
        backend: BackendId,
        cancel: Option<&CancellationToken>,
    ) -> Option<BackendPermitGuard> {
        // Look up the per-backend state.  If none is configured we return
        // a no-op guard immediately.
        let state = {
            let guard = self
                .backend_limiters
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
                    _ = tokio::time::sleep(Duration::from_millis(100)) => {}
                }
            } else {
                tokio::time::sleep(Duration::from_millis(100)).await;
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

#[cfg(test)]
mod cancellation_tests {
    use super::*;

    /// M2 race: a caller queued behind an exhausted global tool-execution
    /// semaphore must be able to abandon the wait via cancellation instead
    /// of blocking forever.
    #[tokio::test]
    async fn cancel_wins_race_against_exhausted_tool_permit() {
        let scheduler = Arc::new(Scheduler::new(SchedulerConfig {
            max_concurrent_tool_executions: 1,
            ..SchedulerConfig::default()
        }));

        // Hold the only permit so a second acquire has to wait.
        let _held = scheduler.acquire_tool_permit().await;

        let cancel = CancellationToken::new();
        let waiter_scheduler = scheduler.clone();
        let waiter_cancel = cancel.clone();
        let waiter = tokio::spawn(async move {
            waiter_scheduler
                .acquire_tool_permit_cancellable(&waiter_cancel)
                .await
        });

        tokio::time::sleep(Duration::from_millis(20)).await;
        cancel.cancel();

        let result = tokio::time::timeout(Duration::from_secs(1), waiter)
            .await
            .expect("cancellable acquire should return promptly after cancellation")
            .expect("waiter task should not panic");
        assert!(
            result.is_none(),
            "cancellable acquire must yield None once cancellation wins the race"
        );
    }

    /// A cancellable acquire that wins its race for a permit before
    /// cancellation must still return `Some`.
    #[tokio::test]
    async fn cancellable_acquire_succeeds_when_permit_is_available() {
        let scheduler = Arc::new(Scheduler::new(SchedulerConfig::default()));
        let cancel = CancellationToken::new();
        let permit = scheduler.acquire_tool_permit_cancellable(&cancel).await;
        assert!(permit.is_some());
    }

    /// A backend-specific rate-limit wait must also be cancellable.
    #[tokio::test]
    async fn cancel_wins_race_against_backend_rate_limit_wait() {
        let scheduler = Arc::new(Scheduler::new(SchedulerConfig::default()));
        let backend = BackendId::new();
        scheduler.configure_backend_limits(
            backend,
            BackendRateLimits {
                max_concurrent_requests: 4,
                requests_per_minute: Some(1),
                tokens_per_minute: None,
            },
        );

        // Consume the one-per-minute allowance.
        let _first = scheduler
            .acquire_backend_specific_permit(backend)
            .await;

        let cancel = CancellationToken::new();
        let waiter_scheduler = scheduler.clone();
        let waiter_cancel = cancel.clone();
        let waiter = tokio::spawn(async move {
            waiter_scheduler
                .acquire_backend_specific_permit_cancellable(backend, &waiter_cancel)
                .await
        });

        tokio::time::sleep(Duration::from_millis(50)).await;
        cancel.cancel();

        let result = tokio::time::timeout(Duration::from_secs(2), waiter)
            .await
            .expect("cancellable backend-specific acquire should return promptly")
            .expect("waiter task should not panic");
        assert!(
            result.is_none(),
            "cancellable backend-specific acquire must yield None once cancellation wins"
        );
    }
}
