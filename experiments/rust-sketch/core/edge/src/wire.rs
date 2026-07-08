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
mod tests {
    use super::*;

    #[test]
    fn request_envelope_roundtrips_with_identity() {
        let raw = RawValue::from_string(r#"{"name":"alice"}"#.to_string()).unwrap();
        let req = Request {
            method: "characters.create".into(),
            identity: Some("player-1".into()),
            payload: raw,
        };
        let bytes = serde_json::to_vec(&req).unwrap();
        let back: Request = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(back.method, "characters.create");
        assert_eq!(back.identity.as_deref(), Some("player-1"));
        assert_eq!(back.payload.get(), r#"{"name":"alice"}"#);
    }

    #[test]
    fn request_omits_absent_identity() {
        let req = Request {
            method: "leaderboard.top".into(),
            identity: None,
            payload: RawValue::from_string("null".into()).unwrap(),
        };
        let s = serde_json::to_string(&req).unwrap();
        assert!(!s.contains("identity"), "absent identity must not be serialised: {s}");
    }

    #[test]
    fn response_ok_and_error_roundtrip() {
        let ok = Response {
            ok: true,
            payload: Some(RawValue::from_string(r#"{"id":"c1"}"#.into()).unwrap()),
            error: None,
        };
        let s = serde_json::to_string(&ok).unwrap();
        assert!(!s.contains("error"), "ok response omits error: {s}");
        let back: Response = serde_json::from_str(&s).unwrap();
        assert!(back.ok);
        assert_eq!(back.payload.unwrap().get(), r#"{"id":"c1"}"#);

        let err = Response { ok: false, payload: None, error: Some("boom".into()) };
        let s = serde_json::to_string(&err).unwrap();
        assert!(!s.contains("payload"), "err response omits payload: {s}");
        let back: Response = serde_json::from_str(&s).unwrap();
        assert!(!back.ok);
        assert_eq!(back.error.as_deref(), Some("boom"));
    }
}
