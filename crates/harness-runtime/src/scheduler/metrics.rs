//! Shared permit-acquisition instrumentation.
//!
//! A single place to record the three metrics every `acquire_*_permit*`
//! method emits (`harness_scheduler_permit_wait_seconds`,
//! `harness_scheduler_permits_in_use`,
//! `harness_scheduler_permit_acquisitions_total`), plus the cancellation
//! counter, so callers share one instrumentation path instead of
//! hand-duplicating it per permit kind.

use std::time::Duration;

use tokio::sync::Semaphore;

/// Metric-name/capacity pair for one permit kind.
#[derive(Clone, Copy)]
pub(crate) struct PermitKind {
    pub(crate) label: &'static str,
    pub(crate) capacity: usize,
}

/// Records the three permit-acquisition metrics for one successful acquire:
/// how long the caller waited, how many permits of this kind are now in
/// use, and a cumulative acquisition count. `sem` is read *after* the
/// permit was taken, so `available_permits()` already reflects this
/// acquisition.
pub(crate) fn record_acquired(kind: PermitKind, sem: &Semaphore, waited: Duration) {
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
pub(crate) fn record_cancelled(kind: PermitKind) {
    metrics::counter!("harness_scheduler_permit_wait_cancelled_total", "kind" => kind.label)
        .increment(1);
}
