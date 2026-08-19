use std::time::Duration;

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
    let _first = scheduler.acquire_backend_specific_permit(backend).await;

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
