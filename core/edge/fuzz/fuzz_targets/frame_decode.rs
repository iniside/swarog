#![no_main]
//! Port of Go's `FuzzReadFrame` + `FuzzFrameRoundTrip` (edge/fuzz_test.go): arbitrary
//! bytes must never panic `read_frame`, regardless of a corrupt length prefix or a
//! truncated body -- the `MAX_FRAME` guard must reject a huge length prefix before it
//! reaches any allocation. And any payload under the cap round-trips losslessly
//! through `frame_bytes` -> `read_frame`.

use std::io::Cursor;

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
    rt.block_on(async {
        // 1. Arbitrary bytes as a raw frame stream must never panic, corrupt length
        // prefix or truncated body alike.
        let mut cursor = Cursor::new(data);
        let _ = edge::read_frame(&mut cursor).await;

        // 2. Round-trip: any payload under the cap survives frame_bytes -> read_frame
        // exactly.
        if data.len() <= edge::MAX_FRAME {
            let framed = edge::frame_bytes(data).expect("payload under cap must frame");
            let mut src = &framed[..];
            let got = edge::read_frame(&mut src).await.expect("frame we just wrote must read back");
            assert_eq!(got, data);
        }
    });
});
