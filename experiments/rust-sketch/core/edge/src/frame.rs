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
    let mut len_buf = [0u8; 4];
    r.read_exact(&mut len_buf).await.map_err(Error::Io)?;
    let n = u32::from_be_bytes(len_buf) as usize;
    if n > MAX_FRAME {
        return Err(Error::FrameTooLarge { size: n, max: MAX_FRAME });
    }
    let mut b = vec![0u8; n];
    r.read_exact(&mut b).await.map_err(Error::Io)?;
    Ok(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn frame_roundtrip_over_in_memory_stream() {
        let payload = b"hello edge frame";
        let framed = frame_bytes(payload).unwrap();
        assert_eq!(&framed[..4], &(payload.len() as u32).to_be_bytes());
        // `&[u8]` implements tokio's AsyncRead — read the frame straight back.
        let mut src = &framed[..];
        let got = read_frame(&mut src).await.unwrap();
        assert_eq!(got, payload);
    }

    #[tokio::test]
    async fn write_then_read_roundtrips() {
        let payload = b"round trip via write_frame";
        let mut buf: Vec<u8> = Vec::new();
        write_frame(&mut buf, payload).await.unwrap();
        let mut src = &buf[..];
        assert_eq!(read_frame(&mut src).await.unwrap(), payload);
    }

    #[tokio::test]
    async fn oversized_write_is_rejected() {
        // A payload one byte over the cap must be refused before any allocation of
        // the peer's read buffer.
        let big = vec![0u8; MAX_FRAME + 1];
        let mut buf: Vec<u8> = Vec::new();
        let err = write_frame(&mut buf, &big).await.unwrap_err();
        assert!(matches!(err, Error::FrameTooLarge { .. }));
        assert!(buf.is_empty(), "nothing should be written for an oversized frame");
    }

    #[tokio::test]
    async fn oversized_length_prefix_is_rejected_on_read() {
        // A corrupt/malicious length prefix claiming > MAX_FRAME must be rejected
        // without attempting the (huge) allocation.
        let mut framed = ((MAX_FRAME as u32) + 1).to_be_bytes().to_vec();
        framed.extend_from_slice(b"whatever");
        let mut src = &framed[..];
        let err = read_frame(&mut src).await.unwrap_err();
        assert!(matches!(err, Error::FrameTooLarge { .. }));
    }

    #[tokio::test]
    async fn truncated_frame_is_unexpected_eof() {
        // Claims 100 bytes, supplies 3 → the read of the body hits EOF early.
        let mut framed = 100u32.to_be_bytes().to_vec();
        framed.extend_from_slice(b"abc");
        let mut src = &framed[..];
        let err = read_frame(&mut src).await.unwrap_err();
        match err {
            Error::Io(e) => assert_eq!(e.kind(), std::io::ErrorKind::UnexpectedEof),
            other => panic!("expected Io(UnexpectedEof), got {other:?}"),
        }
    }

    // Property-ish: random byte payloads of varied lengths survive the frame
    // round-trip exactly (a lightweight stand-in for the Go fuzz seed corpus).
    #[tokio::test]
    async fn random_payloads_survive_roundtrip() {
        // A cheap deterministic LCG — no extra dev-dep, reproducible.
        let mut state: u64 = 0x9E3779B97F4A7C15;
        let mut next = || {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (state >> 33) as u32
        };
        for _ in 0..200 {
            let len = (next() % 4096) as usize;
            let payload: Vec<u8> = (0..len).map(|_| (next() & 0xff) as u8).collect();
            let framed = frame_bytes(&payload).unwrap();
            let mut src = &framed[..];
            let got = read_frame(&mut src).await.unwrap();
            assert_eq!(got, payload);
        }
    }
}
