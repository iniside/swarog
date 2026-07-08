//! The on-wire envelopes for a single call (port of Go's `edge/wire.go`). One
//! stream carries exactly one request/response pair, so the stream itself is the
//! correlation — there is no request id.

use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;

/// The on-wire envelope for a single request.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Request {
    pub method: String,
    /// The caller's verified identity, injected by the gateway (after bearer verify)
    /// or a generated adapter. Absent for an unauthenticated call. Read for trust
    /// ONLY because the edge hop is mutually authenticated (mTLS) — the peer proved a
    /// CA-signed client cert before this envelope was dispatched.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<String>,
    /// The already-encoded request payload, preserved verbatim as raw JSON (Go's
    /// `json.RawMessage`) so the transport never re-parses the domain body.
    pub payload: Box<RawValue>,
}

/// The on-wire envelope for a single reply. `ok` distinguishes a successful
/// `payload` from a handler/dispatch `error`. Public (unlike [`Request`]) because
/// BOTH planes share it: the internal mTLS plane and the player plane reply with the
/// same shape, and a player-side tool needs to decode it. `ok:false` is reserved for
/// TRANSPORT faults (framing, envelope parse, missing handler/method); a completed
/// operation is always `ok:true` with its domain status riding INSIDE `payload`.
#[derive(Debug, Serialize, Deserialize)]
pub struct Response {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<Box<RawValue>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[cfg(test)]
#[path = "wire_tests.rs"]
mod tests;
