//! Thin client for the `harness-transport-ipc` wire protocol.
//!
//! Reuses that crate's frame read/write helpers directly rather than
//! reimplementing them — both sides of one wire protocol should share one
//! implementation of "how to read/write a frame."

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{anyhow, bail, Result};
use tokio::net::UnixStream;

use harness_protocol::ids::SessionId;
use harness_protocol::rpc::{
    RequestCorrelationId, RpcRequest, RpcRequestBody, RpcResponse, RpcResponseBody,
    PROTOCOL_VERSION,
};
use harness_transport_ipc::framing::{read_frame, write_frame};

pub struct HarnessClient {
    stream: UnixStream,
    next_id: AtomicU64,
}

impl HarnessClient {
    /// Connects to `socket_path` and immediately performs the `Hello`
    /// protocol-version handshake before returning.
    ///
    /// Failing fast here means a client/daemon version mismatch surfaces as
    /// a clear connection-time error instead of a confusing failure deep
    /// into a session.
    pub async fn connect(socket_path: &Path) -> Result<Self> {
        let stream = UnixStream::connect(socket_path)
            .await
            .map_err(|e| anyhow!("connecting to {}: {e}", socket_path.display()))?;
        let mut client = Self {
            stream,
            next_id: AtomicU64::new(1),
        };
        client.hello().await?;
        Ok(client)
    }

    /// Sends `Hello` and validates the daemon's protocol version matches
    /// this client's. Called automatically by [`connect`](Self::connect).
    async fn hello(&mut self) -> Result<()> {
        match self
            .request(None, RpcRequestBody::Hello { protocol_version: PROTOCOL_VERSION })
            .await?
        {
            RpcResponseBody::Hello { protocol_version, .. } if protocol_version == PROTOCOL_VERSION => {
                Ok(())
            }
            RpcResponseBody::Hello { protocol_version, .. } => bail!(
                "protocol version mismatch: client speaks {PROTOCOL_VERSION}, daemon speaks {protocol_version}"
            ),

            RpcResponseBody::Failure(error) => bail!("handshake failed: {}", error.message),
            other => bail!("unexpected response to Hello: {other:?}"),
        }
    }

    /// Sends one request and waits for its correlated response.
    ///
    /// Any frame that arrives with a different (or absent) correlation id is
    /// skipped — after a prior `Subscribe`, pushed `Event` frames (which
    /// always carry `id: None`) can interleave with ordinary responses on the
    /// same connection.
    pub async fn request(
        &mut self,
        session_id: Option<SessionId>,
        body: RpcRequestBody,
    ) -> Result<RpcResponseBody> {
        let id = RequestCorrelationId(self.next_id.fetch_add(1, Ordering::Relaxed));
        let request = RpcRequest {
            id,
            session_id,
            body,
        };
        write_frame(&mut self.stream, &serde_json::to_vec(&request)?).await?;

        loop {
            let bytes = read_frame(&mut self.stream)
                .await?
                .ok_or_else(|| anyhow!("connection closed by harnessd"))?;
            let response: RpcResponse = serde_json::from_slice(&bytes)?;
            if response.id == Some(id) {
                return Ok(response.body);
            }
        }
    }

    /// Sends `Subscribe` for `session_id`; on success, subsequent
    /// [`next_event`](Self::next_event) calls read pushed events on this
    /// connection until it closes.
    ///
    /// When `since_seq` is `Some(n)`, the daemon first replays every durable
    /// event with `session_sequence > n` (oldest first) before live events
    /// begin — pass the highest `session_sequence` this client has already
    /// observed to resume without gaps or duplicates after a reconnect.
    /// `None` behaves like a fresh subscription: live events only.
    pub async fn subscribe(&mut self, session_id: SessionId, since_seq: Option<u64>) -> Result<()> {
        match self
            .request(Some(session_id), RpcRequestBody::Subscribe { since_seq })
            .await?
        {
            RpcResponseBody::Ack => Ok(()),
            other => bail!("subscribe failed: {other:?}"),
        }
    }

    /// Reads one pushed frame. Returns `Ok(None)` when the connection closes.
    pub async fn next_event(&mut self) -> Result<Option<RpcResponseBody>> {
        match read_frame(&mut self.stream).await? {
            Some(bytes) => Ok(Some(serde_json::from_slice::<RpcResponse>(&bytes)?.body)),
            None => Ok(None),
        }
    }

    /// Consumes the client and returns the underlying stream, so a caller
    /// that needs concurrent read/write access (e.g. `chat`'s background
    /// event-reader task plus a foreground writer) can `.into_split()` it
    /// itself rather than being limited to this client's request/response
    /// shape.
    pub fn into_stream(self) -> UnixStream {
        self.stream
    }
}
