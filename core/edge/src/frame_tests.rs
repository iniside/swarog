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
