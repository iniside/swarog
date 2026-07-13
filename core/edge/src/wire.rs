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

/// A machine-readable classification for an `ok:false` reply, ORTHOGONAL to the
/// human-readable `error` string (which stays for logs/debugging). The internal
/// [`crate::Client`] keys its typed [`crate::Error::UnknownMethod`] classification
/// off THIS field, never the error text: a handler that itself calls another edge
/// peer, receives a genuine unknown-method (whose `Display` is the verbatim
/// `UNKNOWN_METHOD_PREFIX` text) and propagates it via `?` would otherwise re-stamp
/// the sentinel into its own error string and be misclassified by the outer client
/// as a typed 404. A typed code cannot be re-stamped by accident — it is set at
/// exactly one place (the dispatch's no-handler branch).
///
/// Internal edge peers co-deploy from ONE commit, so `#[serde(default)]` on the
/// carrying field is fixture-compat hygiene (an older on-disk fixture without the
/// field still parses as `None`), NOT cross-version wire support.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResponseCode {
    /// The peer's dispatch table has no handler for the requested method. Stamped
    /// only by the internal [`crate::server`] dispatch; the player plane has no
    /// method table and never sets it.
    #[serde(rename = "unknown_method")]
    UnknownMethod,
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
    /// Machine-readable classification of an `ok:false` reply (see [`ResponseCode`]).
    /// Absent (`None`) for `ok:true` and for ordinary handler errors; `serde(default)`
    /// keeps a code-less fixture/reply parsing. Only the internal plane ever sets it;
    /// the player plane always leaves it `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<ResponseCode>,
}

#[cfg(test)]
#[path = "wire_tests.rs"]
mod tests;
