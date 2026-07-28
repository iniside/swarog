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
        code: None,
    };
    let s = serde_json::to_string(&ok).unwrap();
    assert!(!s.contains("error"), "ok response omits error: {s}");
    assert!(!s.contains("code"), "ok response omits code: {s}");
    let back: Response = serde_json::from_str(&s).unwrap();
    assert!(back.ok);
    assert_eq!(back.payload.unwrap().get(), r#"{"id":"c1"}"#);

    let err = Response { ok: false, payload: None, error: Some("boom".into()), code: None };
    let s = serde_json::to_string(&err).unwrap();
    assert!(!s.contains("payload"), "err response omits payload: {s}");
    assert!(!s.contains("code"), "err response omits absent code: {s}");
    let back: Response = serde_json::from_str(&s).unwrap();
    assert!(!back.ok);
    assert_eq!(back.error.as_deref(), Some("boom"));
    assert_eq!(back.code, None);
}

// The typed unknown-method code round-trips as the stable wire string
// `"unknown_method"`, distinct from the human-readable `error` text.
#[test]
fn response_unknown_method_code_roundtrips() {
    let resp = Response {
        ok: false,
        payload: None,
        error: Some("edge: unknown method \"nope\"".into()),
        code: Some(ResponseCode::UnknownMethod),
    };
    let s = serde_json::to_string(&resp).unwrap();
    assert!(s.contains(r#""code":"unknown_method""#), "stable code string: {s}");
    let back: Response = serde_json::from_str(&s).unwrap();
    assert_eq!(back.code, Some(ResponseCode::UnknownMethod));
}

// The typed ill-typed-body code round-trips as the stable wire string
// `"invalid_request"` and is DISTINCT from `"unknown_method"`. Both halves matter: the
// string is the contract between a peer's dispatch and the caller's classification (the
// 400-vs-503 verdict rides on it), and the two codes must never collide — a collision
// would make an unknown method answer 400 (or an ill-typed body answer 404) at the
// front door.
#[test]
fn response_invalid_request_code_roundtrips_and_differs_from_unknown_method() {
    let resp = Response {
        ok: false,
        payload: None,
        error: Some("edge: invalid request body: invalid type: integer `123`".into()),
        code: Some(ResponseCode::InvalidRequest),
    };
    let s = serde_json::to_string(&resp).unwrap();
    assert!(s.contains(r#""code":"invalid_request""#), "stable code string: {s}");
    let back: Response = serde_json::from_str(&s).unwrap();
    assert_eq!(back.code, Some(ResponseCode::InvalidRequest));

    // Distinct on the wire AND as values — no aliasing between the two typed codes.
    assert_ne!(ResponseCode::InvalidRequest, ResponseCode::UnknownMethod);
    assert_ne!(
        serde_json::to_string(&ResponseCode::InvalidRequest).unwrap(),
        serde_json::to_string(&ResponseCode::UnknownMethod).unwrap()
    );
    let unknown: ResponseCode = serde_json::from_str(r#""unknown_method""#).unwrap();
    assert_eq!(unknown, ResponseCode::UnknownMethod);
    let invalid: ResponseCode = serde_json::from_str(r#""invalid_request""#).unwrap();
    assert_eq!(invalid, ResponseCode::InvalidRequest);
}

// Fixture-compat: a reply JSON WITHOUT the `code` field still deserializes, with
// `code` defaulting to `None` — an older on-disk fixture stays valid (the
// co-deploy serde(default) hygiene, not cross-version support).
#[test]
fn response_without_code_field_defaults_to_none() {
    let legacy = r#"{"ok":false,"error":"boom"}"#;
    let back: Response = serde_json::from_str(legacy).unwrap();
    assert!(!back.ok);
    assert_eq!(back.error.as_deref(), Some("boom"));
    assert_eq!(back.code, None, "absent code must default to None");
}
