#![no_main]
//! Port of Go's `FuzzCodecDecodeRequest` (edge/fuzz_test.go): the default JSON codec
//! must never panic decoding arbitrary bytes into a wire envelope -- errors are fine,
//! panics are not. `Response` is edge's public envelope type (the wire shape shared
//! by both the internal mTLS plane and the player plane).

use edge::Codec;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let codec = edge::default_codec();
    let _ = codec.decode::<edge::Response>(data);
});
