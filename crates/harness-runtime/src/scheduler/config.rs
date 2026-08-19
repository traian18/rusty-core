//! Scheduler capacity configuration and the typed admission-rejection error.

use std::time::Duration;

// ---------------------------------------------------------------------------
// SchedulerConfig
// ---------------------------------------------------------------------------

/// Configuration for [`Scheduler`](crate::scheduler::Scheduler) semaphore capacities.
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
