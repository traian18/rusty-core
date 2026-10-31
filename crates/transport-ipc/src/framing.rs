//! Length-prefixed JSON framing: a 4-byte little-endian `u32` length, followed
//! by that many bytes of `serde_json`-encoded payload. Shared by the server
//! (`serve`) and reusable directly by a client (`harnessctl`), which is why
//! these are generic over `AsyncRead`/`AsyncWrite` rather than tied to
//! `UnixStream` — the same helpers work over an in-memory duplex in tests.

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Reads one frame. Returns `Ok(None)` on a clean EOF at a frame boundary
/// (the peer closed the connection), distinct from an I/O error mid-frame.
pub async fn read_frame<R: AsyncRead + Unpin>(reader: &mut R) -> std::io::Result<Option<Vec<u8>>> {
    let mut len_buf = [0u8; 4];
    match reader.read_exact(&mut len_buf).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = u32::from_le_bytes(len_buf) as usize;
    let mut buf = vec![0u8; len];
    reader.read_exact(&mut buf).await?;
    Ok(Some(buf))
}

/// Writes one frame and flushes so the peer observes it promptly.
pub async fn write_frame<W: AsyncWrite + Unpin>(
    writer: &mut W,
    payload: &[u8],
) -> std::io::Result<()> {
    let len = u32::try_from(payload.len())
        .map_err(|_| std::io::Error::other("frame payload exceeds u32::MAX bytes"))?;
    writer.write_all(&len.to_le_bytes()).await?;
    writer.write_all(payload).await?;
    writer.flush().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn frame_round_trips_over_a_duplex_pipe() {
        let (mut a, mut b) = tokio::io::duplex(1024);
        write_frame(&mut a, b"hello").await.expect("write");
        let received = read_frame(&mut b).await.expect("read").expect("some frame");
        assert_eq!(received, b"hello");
    }

    #[tokio::test]
    async fn read_frame_returns_none_on_clean_eof() {
        let (a, mut b) = tokio::io::duplex(1024);
        drop(a);
        let received = read_frame(&mut b).await.expect("read");
        assert!(received.is_none());
    }

    #[tokio::test]
    async fn multiple_frames_round_trip_in_order() {
        let (mut a, mut b) = tokio::io::duplex(1024);
        write_frame(&mut a, b"first").await.expect("write");
        write_frame(&mut a, b"second").await.expect("write");
        assert_eq!(
            read_frame(&mut b).await.expect("read").expect("frame"),
            b"first"
        );
        assert_eq!(
            read_frame(&mut b).await.expect("read").expect("frame"),
            b"second"
        );
    }
}
