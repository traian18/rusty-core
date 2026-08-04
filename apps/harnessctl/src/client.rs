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
};
use harness_transport_ipc::framing::{read_frame, write_frame};

pub struct HarnessClient {
    stream: UnixStream,
    next_id: AtomicU64,
}

impl HarnessClient {
    pub async fn connect(socket_path: &Path) -> Result<Self> {
        let stream = UnixStream::connect(socket_path)
            .await
            .map_err(|e| anyhow!("connecting to {}: {e}", socket_path.display()))?;
        Ok(Self {
            stream,
            next_id: AtomicU64::new(1),
        })
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
        let request = RpcRequest { id, session_id, body };
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
    pub async fn subscribe(&mut self, session_id: SessionId) -> Result<()> {
        match self.request(Some(session_id), RpcRequestBody::Subscribe).await? {
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
