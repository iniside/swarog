use super::*;

// The serde(default) proof at the envelope level: a request with the token AND the
// api_key OMITTED — the shape every pre-key unauthenticated caller sends — must
// parse (it then fails the FRONT's key check as a domain 401, never as a malformed
// envelope here).
#[test]
fn omitted_token_and_api_key_envelope_parses() {
    let req: PlayerRequest =
        serde_json::from_slice(br#"{"method":"leaderboard.top","payload":{"n":10}}"#).unwrap();
    assert_eq!(req.method, "leaderboard.top");
    assert_eq!(req.token, None);
    assert_eq!(req.api_key, None);
    assert_eq!(req.payload.get(), r#"{"n":10}"#);
}

#[test]
fn token_and_api_key_roundtrip_and_absent_fields_are_not_serialised() {
    let with = PlayerRequest {
        method: "characters.create".into(),
        token: Some("dev-alice".into()),
        api_key: Some("dev-key-client".into()),
        payload: RawValue::from_string(r#"{"name":"hero"}"#.into()).unwrap(),
    };
    let bytes = serde_json::to_vec(&with).unwrap();
    let back: PlayerRequest = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(back.token.as_deref(), Some("dev-alice"));
    assert_eq!(back.api_key.as_deref(), Some("dev-key-client"));
    assert_eq!(back.payload.get(), r#"{"name":"hero"}"#);

    let without = PlayerRequest {
        method: "leaderboard.top".into(),
        token: None,
        api_key: None,
        payload: RawValue::from_string("null".into()).unwrap(),
    };
    let s = serde_json::to_string(&without).unwrap();
    assert!(!s.contains("token"), "absent token must not be serialised: {s}");
    assert!(!s.contains("api_key"), "absent api_key must not be serialised: {s}");
}
