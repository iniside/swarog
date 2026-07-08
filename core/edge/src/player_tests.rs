use super::*;

// The serde(default) proof at the envelope level: a request with the token
// OMITTED — the shape every unauthenticated (AuthNone) call sends — must parse.
#[test]
fn omitted_token_envelope_parses() {
    let req: PlayerRequest =
        serde_json::from_slice(br#"{"method":"leaderboard.top","payload":{"n":10}}"#).unwrap();
    assert_eq!(req.method, "leaderboard.top");
    assert_eq!(req.token, None);
    assert_eq!(req.payload.get(), r#"{"n":10}"#);
}

#[test]
fn token_roundtrips_and_absent_token_is_not_serialised() {
    let with = PlayerRequest {
        method: "characters.create".into(),
        token: Some("dev-alice".into()),
        payload: RawValue::from_string(r#"{"name":"hero"}"#.into()).unwrap(),
    };
    let bytes = serde_json::to_vec(&with).unwrap();
    let back: PlayerRequest = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(back.token.as_deref(), Some("dev-alice"));
    assert_eq!(back.payload.get(), r#"{"name":"hero"}"#);

    let without = PlayerRequest {
        method: "leaderboard.top".into(),
        token: None,
        payload: RawValue::from_string("null".into()).unwrap(),
    };
    let s = serde_json::to_string(&without).unwrap();
    assert!(!s.contains("token"), "absent token must not be serialised: {s}");
}
