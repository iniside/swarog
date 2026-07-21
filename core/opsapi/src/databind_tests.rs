//! Tests for the routing-as-data builder ([`crate::databind`]): a route rebuilt PURELY
//! from an [`OpManifest`] (no compile-time `<name>rpc` import) must produce the SAME
//! `Operation` (verb/path/auth/success/**retry_mode**) and a decode/encode that agrees
//! with the generated typed closures on the wire JSON. Pure, in-process, zero I/O — the
//! LIVE end-to-end is D4's splitproof.

use std::collections::HashMap;

use serde_json::{json, Value};

use crate::databind;
use crate::{ArgMapping, ArgSource, AuthReq, OpManifest, RetryMode, Status};

/// `match.report` — three BODY args carrying the Go-parity `Winner`/`Loser`/`ReportId`
/// keys, `#[retry_safe]` (so `OnceAfterReconnect`), success 202.
fn match_report_manifest() -> OpManifest {
    OpManifest {
        method: "match.report".to_string(),
        verb: "POST".to_string(),
        path: "/match/report".to_string(),
        auth: AuthReq::None,
        success: 202,
        retry_mode: RetryMode::OnceAfterReconnect,
        args: vec![
            ArgMapping {
                param: "report_id".to_string(),
                wire_key: "ReportId".to_string(),
                source: ArgSource::Body,
            },
            ArgMapping {
                param: "winner".to_string(),
                wire_key: "Winner".to_string(),
                source: ArgSource::Body,
            },
            ArgMapping {
                param: "loser".to_string(),
                wire_key: "Loser".to_string(),
                source: ArgSource::Body,
            },
        ],
    }
}

/// `characters.delete` — one PATH arg lifted from `/characters/{id}` into the wire request
/// under its `wire_key`, `AuthReq::Player`, success 204, not retry-safe.
fn characters_delete_manifest() -> OpManifest {
    OpManifest {
        method: "characters.delete".to_string(),
        verb: "DELETE".to_string(),
        path: "/characters/{id}".to_string(),
        auth: AuthReq::Player,
        success: 204,
        retry_mode: RetryMode::Never,
        args: vec![ArgMapping {
            param: "character_id".to_string(),
            wire_key: "character_id".to_string(),
            source: ArgSource::Path {
                wildcard: "id".to_string(),
            },
        }],
    }
}

// ---- Operation: faithful route + retry_mode --------------------------------

#[test]
fn operation_carries_verb_path_auth_success_and_retry_mode() {
    let op = databind::operation(&match_report_manifest());
    assert_eq!(op.method, "match.report");
    assert_eq!(op.verb, "POST");
    assert_eq!(op.path, "/match/report");
    assert_eq!(op.auth, AuthReq::None);
    assert_eq!(op.success, 202);
    // The faithful retry_mode — the whole point of D1.5a: a describe-built op keeps its
    // one-replay-after-reconnect instead of silently defaulting to Never.
    assert_eq!(op.retry_mode, RetryMode::OnceAfterReconnect);

    let del = databind::operation(&characters_delete_manifest());
    assert_eq!(del.verb, "DELETE");
    assert_eq!(del.path, "/characters/{id}");
    assert_eq!(del.auth, AuthReq::Player);
    assert_eq!(del.success, 204);
    assert_eq!(del.retry_mode, RetryMode::Never);
}

// ---- decode: body args pass through under their external keys ---------------

#[test]
fn decode_body_args_are_relayed_verbatim() {
    let binding = databind::binding(&match_report_manifest());
    let body = br#"{"ReportId":"r-1","Winner":"alice","Loser":"bob"}"#;
    let wire = (binding.decode)(Some(body), &HashMap::new()).expect("valid body decodes");
    let got: Value = serde_json::from_slice(&wire).unwrap();
    assert_eq!(
        got,
        json!({"ReportId": "r-1", "Winner": "alice", "Loser": "bob"})
    );
}

#[test]
fn decode_empty_body_is_empty_object() {
    // Caveat (iii): absent/empty body → `{}`; the svc zero-fills via serde defaults.
    let binding = databind::binding(&match_report_manifest());
    let none = (binding.decode)(None, &HashMap::new()).expect("empty decodes");
    assert_eq!(serde_json::from_slice::<Value>(&none).unwrap(), json!({}));
    let empty = (binding.decode)(Some(b""), &HashMap::new()).expect("empty-slice decodes");
    assert_eq!(serde_json::from_slice::<Value>(&empty).unwrap(), json!({}));
}

#[test]
fn decode_malformed_body_is_invalid_400_at_the_gateway() {
    // Caveat (ii): the parse stays here, so a malformed body is a front-door 400.
    let binding = databind::binding(&match_report_manifest());
    let err = (binding.decode)(Some(b"{not json"), &HashMap::new())
        .expect_err("malformed body rejected");
    assert_eq!(err.status, Status::Invalid);
}

#[test]
fn decode_non_object_body_is_invalid() {
    let binding = databind::binding(&match_report_manifest());
    let err = (binding.decode)(Some(b"[1, 2, 3]"), &HashMap::new())
        .expect_err("array body rejected");
    assert_eq!(err.status, Status::Invalid);
}

#[test]
fn decode_wrong_type_body_passes_through_not_400() {
    // Caveat (iv): the data-driven decode holds no field TYPES, so an ill-typed body
    // (`Winner` as a number where the svc wants a String) is NOT a gateway 400 — it passes
    // through and fails svc-side as a 5xx. This pins the recorded topology-dependence; do NOT
    // "fix" it (opsapi carries arg source, never field type).
    let binding = databind::binding(&match_report_manifest());
    let wire = (binding.decode)(Some(br#"{"Winner":123,"Loser":"bob","ReportId":"r"}"#), &HashMap::new())
        .expect("ill-typed but well-formed body is relayed, not rejected at the gateway");
    let got: Value = serde_json::from_slice(&wire).unwrap();
    assert_eq!(got, json!({"Winner": 123, "Loser": "bob", "ReportId": "r"}));
}

// ---- decode: mixed-shape op (body args + a path wildcard) equals the typed wire form ----

/// A synthetic mixed op — two BODY args (`name: String`, `count: u32`) plus one PATH arg
/// (`widget_id`, lifted from `{id}`) — the untested D1 shape. `POST /widgets/{id}/make`.
fn widgets_make_manifest() -> OpManifest {
    OpManifest {
        method: "widgets.make".to_string(),
        verb: "POST".to_string(),
        path: "/widgets/{id}/make".to_string(),
        auth: AuthReq::Player,
        success: 201,
        retry_mode: RetryMode::Never,
        args: vec![
            ArgMapping {
                param: "widget_id".to_string(),
                wire_key: "widget_id".to_string(),
                source: ArgSource::Path { wildcard: "id".to_string() },
            },
            ArgMapping {
                param: "name".to_string(),
                wire_key: "name".to_string(),
                source: ArgSource::Body,
            },
            ArgMapping {
                param: "count".to_string(),
                wire_key: "count".to_string(),
                source: ArgSource::Body,
            },
        ],
    }
}

/// The typed request struct the svc deserializes into — `#[serde(default)]` + `Default`,
/// exactly like every generated `<Method>Request`. Proves the generic decode's untyped wire
/// bytes zero-fill into the SAME typed value the compile-time path would (caveats (i)/(iii)):
/// wire-JSON-EQUIVALENT, not byte-identical, so the assertion is on the DESERIALIZED value.
#[derive(serde::Deserialize, Default, PartialEq, Eq, Debug)]
#[serde(default)]
struct MakeReq {
    widget_id: String,
    name: String,
    count: u32,
}

#[test]
fn decode_mixed_shape_matches_typed_wire_form() {
    let binding = databind::binding(&widgets_make_manifest());
    let mut path = HashMap::new();
    path.insert("id".to_string(), "w-9".to_string());
    let wire = (binding.decode)(Some(br#"{"name":"gizmo","count":3}"#), &path)
        .expect("mixed decode");
    let got: MakeReq = serde_json::from_slice(&wire).expect("svc deserializes the wire request");
    assert_eq!(
        got,
        MakeReq { widget_id: "w-9".to_string(), name: "gizmo".to_string(), count: 3 }
    );
}

#[test]
fn decode_mixed_shape_omitted_body_field_zero_fills_svc_side() {
    // Caveat (iii): an omitted body field is absent from the generic `{}`-seeded wire object;
    // the svc's `#[serde(default)]` zero-fills it — identical to the typed `Request::default()`.
    let binding = databind::binding(&widgets_make_manifest());
    let mut path = HashMap::new();
    path.insert("id".to_string(), "w-1".to_string());
    let wire = (binding.decode)(Some(br#"{"name":"gizmo"}"#), &path).expect("decode");
    let got: MakeReq = serde_json::from_slice(&wire).expect("svc deserializes");
    assert_eq!(
        got,
        MakeReq { widget_id: "w-1".to_string(), name: "gizmo".to_string(), count: 0 }
    );
}

#[test]
fn decode_path_only_op_ignores_a_present_garbage_body() {
    // Finding 2 / caveat (iii): a PATH-ONLY op (no body arg) mirrors `gen_decode`'s
    // `has_body = false` — the body is NEVER parsed, so garbage/whitespace/non-object bodies
    // are IGNORED and the op routes, never a spurious 400 (which the typed path never raises).
    let binding = databind::binding(&characters_delete_manifest());
    let mut path = HashMap::new();
    path.insert("id".to_string(), "c-7".to_string());
    for garbage in [&b"   "[..], &b"[1,2,3]"[..], &b"{not json"[..], &b"\"nope\""[..]] {
        let wire = (binding.decode)(Some(garbage), &path)
            .expect("path-only op ignores the body, never 400s on it");
        let got: Value = serde_json::from_slice(&wire).unwrap();
        assert_eq!(got, json!({"character_id": "c-7"}));
    }
}

// ---- decode: path wildcard injected under its wire_key ----------------------

#[test]
fn decode_path_wildcard_is_injected_under_wire_key() {
    let binding = databind::binding(&characters_delete_manifest());
    let mut path = HashMap::new();
    path.insert("id".to_string(), "char-1".to_string());
    // No body on a DELETE; the sole arg is lifted from the path wildcard `{id}`.
    let wire = (binding.decode)(None, &path).expect("path decode");
    let got: Value = serde_json::from_slice(&wire).unwrap();
    assert_eq!(got, json!({"character_id": "char-1"}));
}

#[test]
fn decode_missing_path_wildcard_defaults_to_empty_string() {
    // Mirrors the generated `path.get(w).cloned().unwrap_or_default()`.
    let binding = databind::binding(&characters_delete_manifest());
    let wire = (binding.decode)(None, &HashMap::new()).expect("decode");
    let got: Value = serde_json::from_slice(&wire).unwrap();
    assert_eq!(got, json!({"character_id": ""}));
}

// ---- encode: envelope reduction (value present / absent / non-Ok) ----------

#[test]
fn encode_ok_with_value_returns_domain_body() {
    let binding = databind::binding(&match_report_manifest());
    let resp = br#"{"status":"Ok","err":"","value":{"accepted":true}}"#;
    let (body, status) = (binding.encode)(resp).expect("ok encodes");
    assert_eq!(status, Status::Ok);
    let body = body.expect("value present → body");
    assert_eq!(
        serde_json::from_slice::<Value>(&body).unwrap(),
        json!({"accepted": true})
    );
}

#[test]
fn encode_ok_without_value_is_no_content() {
    let binding = databind::binding(&characters_delete_manifest());
    let resp = br#"{"status":"Ok","err":""}"#;
    let (body, status) = (binding.encode)(resp).expect("ok encodes");
    assert_eq!(status, Status::Ok);
    assert!(body.is_none(), "absent value key → 204 (no body)");
}

#[test]
fn encode_ok_with_null_value_is_a_present_body() {
    // Spec: a `value` key present EVEN when null is a returned value, not a 204.
    let binding = databind::binding(&match_report_manifest());
    let resp = br#"{"status":"Ok","err":"","value":null}"#;
    let (body, _status) = (binding.encode)(resp).expect("ok encodes");
    let body = body.expect("null value key is still present → body");
    assert_eq!(serde_json::from_slice::<Value>(&body).unwrap(), Value::Null);
}

#[test]
fn encode_non_ok_status_becomes_a_typed_error() {
    let binding = databind::binding(&characters_delete_manifest());
    let resp = br#"{"status":"NotFound","err":"no such character"}"#;
    let err = (binding.encode)(resp).expect_err("non-ok status is an error");
    assert_eq!(err.status, Status::NotFound);
    assert_eq!(err.msg, "no such character");
}

#[test]
fn encode_conflict_status_maps_through() {
    let binding = databind::binding(&match_report_manifest());
    let resp = br#"{"status":"Conflict","err":"duplicate report"}"#;
    let err = (binding.encode)(resp).expect_err("conflict is an error");
    assert_eq!(err.status, Status::Conflict);
    assert_eq!(err.status.http(), 409);
}

// ---- route_binding: bundles both, method-consistent -------------------------

#[test]
fn route_binding_bundles_operation_and_binding_with_matching_method() {
    let rb = databind::route_binding(&match_report_manifest());
    assert_eq!(rb.operation.method, "match.report");
    assert_eq!(rb.binding.method, "match.report");
    assert_eq!(rb.operation.retry_mode, RetryMode::OnceAfterReconnect);
}
