//! Running one backend request off the agent loop and streaming its events
//! into the agent's mailbox.

use std::collections::VecDeque;
use std::sync::Arc;

use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use harness_protocol::backend::{ExecutionError, ExecutionEvent, ExecutionRequest};
use harness_protocol::commands::AgentCommand;
use harness_protocol::ids::{RequestId, RunId};

use super::mailbox::deliver;
use crate::scheduler::Scheduler;
use crate::traits::ExecutionBackend;

/// Capacity of the broadcast sink a backend streams one request's events
/// into. A `broadcast` channel overwrites its oldest entry once the producer
/// gets this far ahead of the receiver, so this must cover the largest burst
/// [`forward_backend_events`] can be left unpolled for. Each scheduler slice
/// of the producer is capped by tokio's cooperative budget (128 channel
/// operations), so this leaves several slices of headroom.
pub(super) const BACKEND_EVENT_BUFFER: usize = 1024;

/// Relays one request's backend events from its broadcast sink into the
/// agent mailbox. Returns whether a terminal (`Completed`/`Error`) event was
/// among them, so the driver knows whether to synthesize one.
///
/// Receiving never waits on the mailbox: events the mailbox has not accepted
/// yet are parked in a local queue. The sink drops its oldest entries once a
/// producer gets `BACKEND_EVENT_BUFFER` events ahead of this receiver, so the
/// receiver must keep draining even while the mailbox is full -- it would
/// stop for the whole duration of a mailbox stall if it awaited
/// `commands.send` directly. A stall now costs memory proportional to the
/// burst rather than dropped text deltas.
pub(super) async fn forward_backend_events(
    mut event_rx: broadcast::Receiver<ExecutionEvent>,
    commands: mpsc::Sender<AgentCommand>,
    run_id: RunId,
    cancel: CancellationToken,
) -> bool {
    let mut terminal_forwarded = false;
    let mut queued: VecDeque<ExecutionEvent> = VecDeque::new();
    let mut source_open = true;
    loop {
        if !source_open && queued.is_empty() {
            break;
        }
        tokio::select! {
            // Draining the sink takes priority over feeding the mailbox: the
            // sink is the lossy side, the local queue is not.
            biased;
            _ = cancel.cancelled() => break,
            event = event_rx.recv(), if source_open => match event {
                Ok(event) => queued.push_back(event),
                Err(broadcast::error::RecvError::Closed) => source_open = false,
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(run_id = ?run_id, skipped = n, "backend event receiver lagged");
                }
            },
            permit = commands.reserve(), if !queued.is_empty() => match permit {
                Ok(permit) => {
                    let event = queued
                        .pop_front()
                        .expect("branch is guarded by !queued.is_empty()");
                    terminal_forwarded |= matches!(
                        &event,
                        ExecutionEvent::Completed { .. } | ExecutionEvent::Error { .. }
                    );
                    permit.send(AgentCommand::BackendEvent { run_id, event });
                }
                Err(_) => break,
            },
        }
    }
    terminal_forwarded
}

/// The terminal event for a request refused before it reached the backend.
pub(super) fn invalid_request(
    run_id: RunId,
    request_id: RequestId,
    message: String,
) -> AgentCommand {
    AgentCommand::BackendEvent {
        run_id,
        event: ExecutionEvent::Error {
            request_id,
            error: ExecutionError::InvalidRequest { message },
        },
    }
}

/// What one backend request runs with, off the agent loop.
pub(super) struct BackendRun {
    pub(super) backend: Arc<dyn ExecutionBackend>,
    pub(super) scheduler: Arc<Scheduler>,
    pub(super) commands: mpsc::Sender<AgentCommand>,
    /// The runner's token: forwarding stops when the runner stops.
    pub(super) runner_cancel: CancellationToken,
    /// This run's token: cancels the request itself.
    pub(super) token: CancellationToken,
}

impl BackendRun {
    /// A forwarding task relays streamed events to the mailbox while a
    /// driver task holds the scheduler permits and runs the request.
    pub(super) fn start(self, request: ExecutionRequest) {
        let (event_tx, event_rx) = broadcast::channel(BACKEND_EVENT_BUFFER);
        let forward_handle = tokio::spawn(forward_backend_events(
            event_rx,
            self.commands.clone(),
            request.run_id,
            self.runner_cancel.clone(),
        ));
        tokio::spawn(self.drive(request, event_tx, forward_handle));
    }

    /// The driver waits for the forwarding task to drain before
    /// synthesizing a terminal event, so streamed events are never overtaken.
    async fn drive(
        self,
        request: ExecutionRequest,
        event_tx: broadcast::Sender<ExecutionEvent>,
        forward_handle: JoinHandle<bool>,
    ) {
        let backend_id = self.backend.descriptor().id;
        let run_id = request.run_id;
        let request_id = request.request_id;
        // Capacity waits must never block the agent mailbox. Both waits
        // are also run-cancellable, so a queued request cannot survive a
        // cancellation and execute later.
        let _global_permit = tokio::select! {
            permit = self.scheduler.acquire_backend_permit() => permit,
            _ = self.token.cancelled() => return,
        };
        let _backend_guard = tokio::select! {
            guard = self.scheduler.acquire_backend_specific_permit(backend_id) => guard,
            _ = self.token.cancelled() => return,
        };
        let outcome = self
            .backend
            .execute(request, event_tx, self.token.clone())
            .await;
        // A forwarder that panicked relayed nothing we can count on, so the
        // terminal event is synthesized then too.
        let terminal_forwarded = forward_handle.await.unwrap_or(false);
        if terminal_forwarded {
            return;
        }
        let event = match outcome {
            Ok(result) => ExecutionEvent::Completed { request_id, result },
            Err(error) => ExecutionEvent::Error { request_id, error },
        };
        deliver(&self.commands, AgentCommand::BackendEvent { run_id, event }).await;
    }
}
