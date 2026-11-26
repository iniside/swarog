//! [SK3]'s verdict in `one_stored_key`, fed by hand (no fleet, no table): a served key set that is
//! valid on its own, beside a stored row whose `public_jwk` is clean or carries a private member.

use serde_json::json;

use super::*;

/// One public ES256 P-256 signing key with kid `k1`, the JWKS [SK3] accepts.
fn served() -> Vec<Value> {
    vec![json!({
        "kty": "EC", "crv": "P-256", "alg": "ES256", "use": "sig",
        "kid": "k1", "x": "AAAA", "y": "BBBB",
    })]
}

/// The `active` row `k1` storing `public_jwk`.
fn active_row(public_jwk: Value) -> Vec<StoredKey> {
    vec![StoredKey { kid: "k1".into(), status: "active".into(), public_jwk }]
}

#[test]
fn a_clean_stored_row_beside_the_served_key_passes() {
    let (ok, kid, detail) = one_stored_key(&served(), &active_row(served()[0].clone()));
    assert!(ok, "a clean active row must pass: {detail}");
    assert_eq!(kid, "k1");
    assert!(detail.contains("stored_private=[]"), "{detail}");
}

#[test]
fn a_stored_row_with_a_private_member_fails_and_names_it() {
    let mut jwk = served()[0].clone();
    jwk["d"] = json!("private-scalar");
    let (ok, _, detail) = one_stored_key(&served(), &active_row(jwk));
    assert!(!ok, "a stored public_jwk holding `d` must fail [SK3]: {detail}");
    assert!(detail.contains(r#"stored_private=[("k1", ["d"])]"#), "{detail}");
}
