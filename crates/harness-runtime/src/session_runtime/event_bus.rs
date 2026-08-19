//! Session-wide event aggregation: merges every agent runner's event stream
//! into one ordered stream for subscribers.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use harness_protocol::events::AgentEventEnvelope;
use harness_protocol::ids::AgentId;

use crate::traits::EventSink;

// ---------------------------------------------------------------------------
// BridgeEventSink — connects EventSink trait to external persistence/logging
// ---------------------------------------------------------------------------

/// An [`EventSink`] implementation that forwards events to an optional
/// external sink (e.g. persistence, logging).
///
/// Note: The agent runner's [`emit`](crate::agent_runner::AgentRunner::emit)
/// already delivers events directly to the agent's `task.events` broadcast
/// channel (which the [`SessionEventBus`] polls). This bridge only forwards
/// to an optional secondary sink — it does NOT duplicate events back into
/// `task.events`.
pub(super) struct BridgeEventSink {
    /// An optional external sink for persistence / logging.
    pub(super) external_sink: Option<Arc<dyn EventSink>>,
}

impl EventSink for BridgeEventSink {
    fn send(&self, envelope: AgentEventEnvelope) {
        if let Some(ref sink) = self.external_sink {
            sink.send(envelope);
        }
    }
}

// ---------------------------------------------------------------------------
// SessionEventBus
// ---------------------------------------------------------------------------

/// Aggregates event broadcasts from all agent runners in a session and fans
/// them out to external subscribers as a single ordered stream.
///
/// # Ordering
///
/// When a session has an authoritative
/// [`SessionCommitter`](harness_session_store::SessionCommitter) (RC-301),
/// every event already carries its final `session_sequence` and the bus
/// **preserves it** — stored and observed order agree. Without a committer,
/// the bus assigns a monotonically increasing sequence itself. Combined
/// with the agent-local
/// [`agent_sequence`](AgentEventEnvelope::agent_sequence), this gives
/// consumers a total order across all agents in the session.
///
/// # Lifecycle
///
/// 1. Create with [`SessionEventBus::new`].
/// 2. As each agent runner is spawned, call
///    [`register_agent`](SessionEventBus::register_agent) with the broadcast
///    sender from that runner's
///    [`AgentTask`](crate::agent_runner::AgentTask).
/// 3. Spawn the [`run`](SessionEventBus::run) loop as a background tokio task.
/// 4. External consumers obtain a receiver via
///    [`subscribe`](SessionEventBus::subscribe).
pub struct SessionEventBus {
    /// Broadcast receivers for each registered agent. Wrapped in a [`Mutex`]
    /// so that agents can be registered from any thread while the run loop
    /// polls them concurrently.
    receivers: Mutex<Vec<broadcast::Receiver<AgentEventEnvelope>>>,

    /// Shared sender for external subscribers carved from the broadcast
    /// channel created in [`new`](SessionEventBus::new).
    subscriber_sender: broadcast::Sender<AgentEventEnvelope>,

    /// Monotonically increasing session-level sequence counter. Used only
    /// when events arrive without a committer-assigned sequence.
    session_sequence: AtomicU64,
}

impl SessionEventBus {
    /// Create a new event bus with the given subscriber channel capacity.
    pub fn new(capacity: usize) -> Self {
        let (subscriber_sender, _) = broadcast::channel(capacity);
        Self {
            receivers: Mutex::new(Vec::new()),
            subscriber_sender,
            session_sequence: AtomicU64::new(0),
        }
    }

    /// Register a new agent's event source with the bus.
    ///
    /// The `sender` should be the [`broadcast::Sender`] from the agent's
    /// [`AgentTask::events`](crate::agent_runner::AgentTask). The bus
    /// creates a receiver and will poll it in the
    /// [`run`](SessionEventBus::run) loop.
    pub fn register_agent(
        &self,
        _agent_id: AgentId,
        sender: broadcast::Sender<AgentEventEnvelope>,
    ) {
        let rx = sender.subscribe();
        self.receivers
            .lock()
            .expect("receivers mutex poisoned")
            .push(rx);
    }

    /// Return a new subscriber receiver that will receive all session events.
    pub fn subscribe(&self) -> broadcast::Receiver<AgentEventEnvelope> {
        self.subscriber_sender.subscribe()
    }

    /// Run the event bus's forwarding loop.
    ///
    /// Polls every registered agent receiver via `try_recv`,
    /// assigns a monotonically increasing `session_sequence` to each event
    /// that does not already carry one, and forwards to all subscribers.
    /// Exits when `bus_cancel` is triggered.
    pub async fn run(&self, bus_cancel: CancellationToken) {
        loop {
            if bus_cancel.is_cancelled() {
                // Drain any remaining events.  This is important when the
                // cancellation token fires concurrently with a session
                // cancel: the agent runner may emit its terminal event
                // before we get to poll again, and we need to forward it.
                self.poll_receivers();
                break;
            }

            // poll_receivers is synchronous — the mutex guard is scoped
            // inside it and is released before we reach any await point.
            let had_data = self.poll_receivers();

            if !had_data {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            } else {
                tokio::task::yield_now().await;
            }
        }
    }

    /// Poll all registered receivers once, forwarding events to subscribers.
    ///
    /// Returns `true` if at least one event was forwarded.
    ///
    /// The mutex guard is scoped to this function so that the lock is
    /// not held across any await point in the caller.
    fn poll_receivers(&self) -> bool {
        let mut had_data = false;

        let mut receivers = self.receivers.lock().expect("receivers mutex poisoned");

        // Drain all available events from each receiver.
        // `retain_mut` returns `true` to keep the receiver, `false` to
        // remove it when the sender has been dropped.
        receivers.retain_mut(|rx| loop {
            match rx.try_recv() {
                Ok(mut envelope) => {
                    // RC-301: preserve a committer-assigned sequence so the
                    // observed stream matches the stored durable order.
                    // Without one, assign the next bus sequence.
                    if envelope.session_sequence.is_none() {
                        let seq = self.session_sequence.fetch_add(1, Ordering::Relaxed);
                        envelope.session_sequence = Some(seq);
                    }
                    let _ = self.subscriber_sender.send(envelope);
                    had_data = true;
                    // Continue draining this receiver.
                }
                Err(broadcast::error::TryRecvError::Empty) => {
                    // No more events from this receiver for now.
                    break true;
                }
                Err(broadcast::error::TryRecvError::Closed) => {
                    // Receiver was dropped; remove it.
                    break false;
                }
                Err(broadcast::error::TryRecvError::Lagged(n)) => {
                    tracing::warn!("Session event bus lagged by {n} events");
                    // Continue draining; the channel is still live.
                }
            }
        });

        had_data
    }
}
