//! Streaming a session's events to a subscriber: first what it missed, then what happens next.

use std::sync::Arc;

use rusty_protocol::{
    events::AgentEventEnvelope,
    ids::SessionId,
    rpc::{RpcResponse, RpcResponseBody},
};
use tokio::sync::broadcast::{self, error::RecvError};
use tokio_util::sync::CancellationToken;

use super::{connection::ResponseSink, RpcHandler};

/// Replays events after `since_seq` (when the client is resuming), then forwards live events from
/// `receiver` on a background task until the connection ends.
///
/// The receiver was subscribed before the backlog was fetched, so the two can overlap on events
/// appended in between; anything already replayed is skipped so the stream has no duplicates.
pub(super) async fn stream_events<S: ResponseSink>(
    handler: Arc<dyn RpcHandler>,
    sink: S,
    cancel: CancellationToken,
    session_id: SessionId,
    since_seq: Option<u64>,
    receiver: broadcast::Receiver<AgentEventEnvelope>,
) {
    let Some(last_replayed) = replay_backlog(&handler, &sink, session_id, since_seq).await else {
        return;
    };
    tokio::spawn(forward_live_events(
        sink,
        cancel,
        session_id,
        receiver,
        last_replayed,
    ));
}

/// Sends the missed events and returns the highest sequence sent, or `None` if the connection
/// closed while sending.
async fn replay_backlog<S: ResponseSink>(
    handler: &Arc<dyn RpcHandler>,
    sink: &S,
    session_id: SessionId,
    since_seq: Option<u64>,
) -> Option<u64> {
    let backlog = match since_seq {
        Some(since_seq) => handler.events_since(session_id, since_seq).await,
        None => Vec::new(),
    };
    let mut last_sent = since_seq.unwrap_or(0);
    for envelope in backlog {
        if let Some(sequence) = envelope.session_sequence {
            last_sent = last_sent.max(sequence);
        }
        if !sink.send(event_response(envelope)).await {
            return None;
        }
    }
    Some(last_sent)
}

async fn forward_live_events<S: ResponseSink>(
    sink: S,
    cancel: CancellationToken,
    session_id: SessionId,
    mut receiver: broadcast::Receiver<AgentEventEnvelope>,
    mut last_sent: u64,
) {
    loop {
        let received = tokio::select! {
            _ = cancel.cancelled() => return,
            received = receiver.recv() => received,
        };
        let response = match received {
            Ok(envelope) => match fresh_sequence(&envelope, &mut last_sent) {
                true => event_response(envelope),
                false => continue,
            },
            Err(RecvError::Lagged(dropped)) => {
                tracing::warn!(dropped, "subscriber lagged; signalling an event gap");
                gap_response(session_id, last_sent, dropped)
            }
            Err(RecvError::Closed) => return,
        };
        if !sink.send(response).await {
            return;
        }
    }
}

/// Whether the event is newer than anything sent; records its sequence if so. Events without a
/// sequence are always new.
fn fresh_sequence(envelope: &AgentEventEnvelope, last_sent: &mut u64) -> bool {
    match envelope.session_sequence {
        Some(sequence) if sequence <= *last_sent => false,
        Some(sequence) => {
            *last_sent = sequence;
            true
        }
        None => true,
    }
}

fn event_response(envelope: AgentEventEnvelope) -> RpcResponse {
    RpcResponse {
        id: None,
        body: RpcResponseBody::Event(envelope),
    }
}

fn gap_response(session_id: SessionId, last_delivered_sequence: u64, dropped: u64) -> RpcResponse {
    RpcResponse {
        id: None,
        body: RpcResponseBody::EventGap {
            session_id,
            last_delivered_sequence,
            dropped,
            cursor_expired: false,
        },
    }
}
