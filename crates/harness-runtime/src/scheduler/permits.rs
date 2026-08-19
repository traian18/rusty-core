//! Global concurrency ceilings: session, agent, backend-request, tool, and
//! process semaphores.
//!
//! [`GlobalPermits`] owns the five independent `tokio::sync::Semaphore`s that
//! cap harness-wide concurrency and is the only place that knows how to
//! acquire, time out, or cancel a wait against them. It is deliberately
//! unaware of per-backend rate limiting — see [`BackendLimiters`](crate::scheduler::backend_limiter::BackendLimiters)
//! for that concern.

use std::sync::Arc;
use std::time::Instant;

use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;

use crate::scheduler::config::{CapacityError, SchedulerConfig};
use crate::scheduler::metrics::{record_acquired, record_cancelled, PermitKind};

// ---------------------------------------------------------------------------
// Snapshot types
// ---------------------------------------------------------------------------

/// One permit kind's point-in-time capacity/utilization, part of
/// [`SchedulerSnapshot`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PermitSnapshot {
    pub kind: &'static str,
    pub capacity: usize,
    pub in_use: usize,
}

/// Point-in-time snapshot of every [`Scheduler`](crate::scheduler::Scheduler)
/// permit kind. See [`Scheduler::snapshot`](crate::scheduler::Scheduler::snapshot).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchedulerSnapshot {
    pub permits: Vec<PermitSnapshot>,
}

// ---------------------------------------------------------------------------
// GlobalPermits
// ---------------------------------------------------------------------------

/// Owns the five independent semaphores so that permits can be acquired with
/// [`acquire_owned`](Semaphore::acquire_owned), yielding an
/// [`OwnedSemaphorePermit`] that is independent of any borrow on `self`.
/// This allows permits to be moved into spawned [`tokio::spawn`] tasks
/// without lifetime headaches.
pub(crate) struct GlobalPermits {
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
    /// Configured capacities, retained (beyond building the semaphores
    /// above) so acquisitions can report "in use" as
    /// `capacity - available_permits()` — `Semaphore` itself doesn't expose
    /// the value it was constructed with. M6: this is what backs the
    /// `harness_scheduler_permits_in_use` gauge.
    config: SchedulerConfig,
}

impl GlobalPermits {
    pub(crate) fn new(config: SchedulerConfig) -> Self {
        Self {
            sessions: Arc::new(Semaphore::new(config.max_active_sessions)),
            agents: Arc::new(Semaphore::new(config.max_active_agents)),
            backend_requests: Arc::new(Semaphore::new(config.max_concurrent_backend_requests)),
            tool_executions: Arc::new(Semaphore::new(config.max_concurrent_tool_executions)),
            processes: Arc::new(Semaphore::new(config.max_concurrent_processes)),
            config,
        }
    }

    /// Acquires a permit for creating a new session.
    ///
    /// Blocks until the number of active sessions is below
    /// [`SchedulerConfig::max_active_sessions`].
    pub(crate) async fn acquire_session(&self) -> OwnedSemaphorePermit {
        let start = Instant::now();
        let permit = self
            .sessions
            .clone()
            .acquire_owned()
            .await
            .expect("Scheduler semaphore should never be closed");
        record_acquired(
            PermitKind {
                label: "session",
                capacity: self.config.max_active_sessions,
            },
            &self.sessions,
            start.elapsed(),
        );
        permit
    }

    /// E1: bounded-wait variant of [`Self::acquire_session`] — waits up to
    /// `self.config.admission_timeout` for a slot, then rejects typed
    /// (`CapacityError`) instead of queueing indefinitely.
    pub(crate) async fn try_acquire_session(&self) -> Result<OwnedSemaphorePermit, CapacityError> {
        let kind = PermitKind {
            label: "session",
            capacity: self.config.max_active_sessions,
        };
        let start = Instant::now();
        let sem = self.sessions.clone();
        match tokio::time::timeout(self.config.admission_timeout, sem.acquire_owned()).await {
            Ok(permit) => {
                let permit = permit.expect("Scheduler semaphore should never be closed");
                record_acquired(kind, &self.sessions, start.elapsed());
                Ok(permit)
            }
            Err(_) => {
                metrics::counter!("harness_scheduler_permit_admission_rejected_total", "kind" => kind.label)
                    .increment(1);
                Err(CapacityError {
                    kind: kind.label,
                    waited: start.elapsed(),
                })
            }
        }
    }

    /// Acquires a permit for spawning a new agent runner.
    pub(crate) async fn acquire_agent(&self) -> OwnedSemaphorePermit {
        let start = Instant::now();
        let permit = self
            .agents
            .clone()
            .acquire_owned()
            .await
            .expect("Scheduler semaphore should never be closed");
        record_acquired(
            PermitKind {
                label: "agent",
                capacity: self.config.max_active_agents,
            },
            &self.agents,
            start.elapsed(),
        );
        permit
    }

    /// Acquires a permit for executing a backend (LLM) request.
    ///
    /// Blocks until the number of in-flight backend requests is below
    /// [`SchedulerConfig::max_concurrent_backend_requests`].
    pub(crate) async fn acquire_backend(&self) -> OwnedSemaphorePermit {
        let start = Instant::now();
        let permit = self
            .backend_requests
            .clone()
            .acquire_owned()
            .await
            .expect("Scheduler semaphore should never be closed");
        record_acquired(
            PermitKind {
                label: "backend",
                capacity: self.config.max_concurrent_backend_requests,
            },
            &self.backend_requests,
            start.elapsed(),
        );
        permit
    }

    /// Cancellable variant of [`Self::acquire_backend`].
    ///
    /// Returns `None` if `cancel` fires before a permit becomes available.
    pub(crate) async fn acquire_backend_cancellable(
        &self,
        cancel: &CancellationToken,
    ) -> Option<OwnedSemaphorePermit> {
        let start = Instant::now();
        let sem = self.backend_requests.clone();
        let kind = PermitKind {
            label: "backend",
            capacity: self.config.max_concurrent_backend_requests,
        };
        tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                record_cancelled(kind);
                None
            }
            permit = sem.acquire_owned() => {
                let permit = permit.expect("Scheduler semaphore should never be closed");
                record_acquired(kind, &self.backend_requests, start.elapsed());
                Some(permit)
            }
        }
    }

    /// Acquires a permit for executing a tool.
    ///
    /// Blocks until the number of in-flight tool executions is below
    /// [`SchedulerConfig::max_concurrent_tool_executions`].
    pub(crate) async fn acquire_tool(&self) -> OwnedSemaphorePermit {
        let start = Instant::now();
        let permit = self
            .tool_executions
            .clone()
            .acquire_owned()
            .await
            .expect("Scheduler semaphore should never be closed");
        record_acquired(
            PermitKind {
                label: "tool",
                capacity: self.config.max_concurrent_tool_executions,
            },
            &self.tool_executions,
            start.elapsed(),
        );
        permit
    }

    /// Cancellable variant of [`Self::acquire_tool`].
    ///
    /// Returns `None` if `cancel` fires before a permit becomes available.
    pub(crate) async fn acquire_tool_cancellable(
        &self,
        cancel: &CancellationToken,
    ) -> Option<OwnedSemaphorePermit> {
        let start = Instant::now();
        let sem = self.tool_executions.clone();
        let kind = PermitKind {
            label: "tool",
            capacity: self.config.max_concurrent_tool_executions,
        };
        tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                record_cancelled(kind);
                None
            }
            permit = sem.acquire_owned() => {
                let permit = permit.expect("Scheduler semaphore should never be closed");
                record_acquired(kind, &self.tool_executions, start.elapsed());
                Some(permit)
            }
        }
    }

    /// Acquires a permit for spawning a child process.
    pub(crate) async fn acquire_process(&self) -> OwnedSemaphorePermit {
        let start = Instant::now();
        let permit = self
            .processes
            .clone()
            .acquire_owned()
            .await
            .expect("Scheduler semaphore should never be closed");
        record_acquired(
            PermitKind {
                label: "process",
                capacity: self.config.max_concurrent_processes,
            },
            &self.processes,
            start.elapsed(),
        );
        permit
    }

    /// Cancellable variant of [`Self::acquire_process`].
    ///
    /// Returns `None` if `cancel` fires before a permit becomes available.
    pub(crate) async fn acquire_process_cancellable(
        &self,
        cancel: &CancellationToken,
    ) -> Option<OwnedSemaphorePermit> {
        let start = Instant::now();
        let sem = self.processes.clone();
        let kind = PermitKind {
            label: "process",
            capacity: self.config.max_concurrent_processes,
        };
        tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                record_cancelled(kind);
                None
            }
            permit = sem.acquire_owned() => {
                let permit = permit.expect("Scheduler semaphore should never be closed");
                record_acquired(kind, &self.processes, start.elapsed());
                Some(permit)
            }
        }
    }

    /// Point-in-time snapshot of every permit kind's capacity/in-use state,
    /// for the M6 diagnostics RPC (`GetDiagnostics`) — a non-Prometheus,
    /// directly-queryable view of the same underlying semaphores the
    /// metrics above report on, useful for a host that wants current
    /// saturation without scraping/parsing metrics text.
    pub(crate) fn snapshot(&self) -> SchedulerSnapshot {
        let permit = |label: &'static str, capacity: usize, sem: &Semaphore| PermitSnapshot {
            kind: label,
            capacity,
            in_use: capacity.saturating_sub(sem.available_permits()),
        };
        SchedulerSnapshot {
            permits: vec![
                permit("session", self.config.max_active_sessions, &self.sessions),
                permit("agent", self.config.max_active_agents, &self.agents),
                permit(
                    "backend",
                    self.config.max_concurrent_backend_requests,
                    &self.backend_requests,
                ),
                permit(
                    "tool",
                    self.config.max_concurrent_tool_executions,
                    &self.tool_executions,
                ),
                permit(
                    "process",
                    self.config.max_concurrent_processes,
                    &self.processes,
                ),
            ],
        }
    }
}
