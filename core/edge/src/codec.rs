//! The codec seam (port of Go's `edge/codec.go`). The default is JSON; msgpack (or
//! any other encoding) is a future swap behind this trait — nothing in the
//! transport, framing, or RPC dispatch depends on the concrete encoding.
//!
//! Unlike Go's `Codec` (which takes `any`), the Rust trait has generic methods, so
//! it is a compile-time seam rather than a `dyn` object. The shipped [`JsonCodec`]
//! is what `Client`/`Server` use for the wire envelope; a different codec would be a
//! drop-in `impl Codec`. JSON is the only codec wired here (the task ships JSON;
//! msgpack is a documented future swap).

use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::Error;

/// Encodes and decodes wire values. Generic (compile-time) so a swap costs nothing
/// at runtime.
pub trait Codec: Send + Sync {
    fn encode<T: Serialize>(&self, v: &T) -> Result<Vec<u8>, Error>;
    fn decode<T: DeserializeOwned>(&self, data: &[u8]) -> Result<T, Error>;
}

/// The default [`Codec`], backed by `serde_json`.
#[derive(Clone, Copy, Debug, Default)]
pub struct JsonCodec;

impl Codec for JsonCodec {
    fn encode<T: Serialize>(&self, v: &T) -> Result<Vec<u8>, Error> {
        serde_json::to_vec(v).map_err(Error::Codec)
    }
    fn decode<T: DeserializeOwned>(&self, data: &[u8]) -> Result<T, Error> {
        serde_json::from_slice(data).map_err(Error::Codec)
    }
}

/// The codec used when a `Client`/`Server` is constructed without an explicit one
/// (Go's `defaultCodec`).
pub fn default_codec() -> JsonCodec {
    JsonCodec
}

#[cfg(test)]
#[path = "codec_tests.rs"]
mod tests;
