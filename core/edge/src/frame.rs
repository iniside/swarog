//! Length-prefixed framing: a 4-byte big-endian length followed by the payload,
//! written in a SINGLE write so the header and body are never split across the
//! stream by an intermediate flush (port of Go's `edge/frame.go`).

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::Error;

/// Caps a single frame's payload to guard a malicious or corrupt length prefix from
/// triggering an unbounded allocation. 16 MiB is far above any legitimate RPC
/// envelope (Go's `maxFrameSize`).
pub const MAX_FRAME: usize = 16 << 20; // 16 MiB

/// Builds a length-prefixed frame buffer: 4-byte big-endian length + payload, in one
/// contiguous `Vec` (the "single write" guarantee — the caller hands the whole
/// buffer to one `write_all`). Pure and synchronous so it is directly testable.
pub fn frame_bytes(b: &[u8]) -> Result<Vec<u8>, Error> {
    if b.len() > MAX_FRAME {
        return Err(Error::FrameTooLarge {
            size: b.len(),
            max: MAX_FRAME,
        });
    }
    let mut buf = Vec::with_capacity(4 + b.len());
    buf.extend_from_slice(&(b.len() as u32).to_be_bytes());
    buf.extend_from_slice(b);
    Ok(buf)
}

/// Writes a single length-prefixed frame in one `write_all` (Go's `writeFrame`).
pub async fn write_frame<W: AsyncWrite + Unpin>(w: &mut W, b: &[u8]) -> Result<(), Error> {
    let buf = frame_bytes(b)?;
    w.write_all(&buf).await.map_err(Error::Io)
}

/// Reads a single length-prefixed frame written by [`write_frame`]: the 4-byte
/// length, guarded against [`MAX_FRAME`], then exactly that many payload bytes. A
/// truncated frame surfaces as an `UnexpectedEof` (Go's `io.ErrUnexpectedEOF`).
pub async fn read_frame<R: AsyncRead + Unpin>(r: &mut R) -> Result<Vec<u8>, Error> {
    read_frame_max(r, MAX_FRAME).await
}

/// [`read_frame`] with an explicit cap — the seam that lets the PLAYER plane read
/// with its much tighter [`crate::MAX_PLAYER_FRAME`] while the internal plane keeps
/// [`MAX_FRAME`]. The length prefix is checked BEFORE any body allocation, so an
/// attacker-claimed huge length costs nothing.
pub async fn read_frame_max<R: AsyncRead + Unpin>(r: &mut R, max: usize) -> Result<Vec<u8>, Error> {
    let mut len_buf = [0u8; 4];
    r.read_exact(&mut len_buf).await.map_err(Error::Io)?;
    let n = u32::from_be_bytes(len_buf) as usize;
    if n > max {
        return Err(Error::FrameTooLarge { size: n, max });
    }
    let mut b = vec![0u8; n];
    r.read_exact(&mut b).await.map_err(Error::Io)?;
    Ok(b)
}

#[cfg(test)]
#[path = "frame_tests.rs"]
mod tests;
