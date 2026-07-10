use super::keys::policy_allows;
use super::*;
use apikeysapi::KeyRecord;
use axum::http::Request as HttpRequest;
use opsapi::{DecodeFn, EncodeFn, LocalOp, OpSet, Status};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tower::ServiceExt; // for `oneshot`

// ---- API-key test fixtures ----

/// The full-policy demo key every happy-path request carries.
const TEST_KEY: &str = "test-key";
/// A key whose policy names only `other.op` — the denied-method fixture.
const LIMITED_KEY: &str = "limited-key";

/// A [`KeyVerifier`] over a fixed key → policy map — the front-door tests' stand-in
/// for the `apikeys.keys` capability (no store, no TTL cache).
struct FakeKeyVerifier {
    keys: HashMap<String, String>,
}

#[async_trait::async_trait]
impl KeyVerifier for FakeKeyVerifier {
    async fn lookup(&self, key: &str) -> Option<KeyRecord> {
        self.keys
            .get(key)
            .map(|policy| KeyRecord { name: key.to_string(), policy: policy.clone() })
    }
}

/// The demo key set: [`TEST_KEY`] (full) and [`LIMITED_KEY`] (allows only `other.op`).
fn demo_keys() -> Arc<dyn KeyVerifier> {
    let mut keys = HashMap::new();
    keys.insert(TEST_KEY.to_string(), "full".to_string());
    keys.insert(LIMITED_KEY.to_string(), "other.op".to_string());
    Arc::new(FakeKeyVerifier { keys })
}

/// Builds a `FrontDoor` over `slots` with the standard dev session verifier and an
/// injectable key verifier — the single construction seam every test funnels through.
fn front_door_with_keys(slots: Arc<Slots>, keys: Arc<dyn KeyVerifier>) -> Arc<FrontDoor> {
    Arc::new(FrontDoor::new(slots, Arc::new(DevSessionVerifier::new()), keys, Vec::new()))
}

// ---- (a) route matching incl. {wild} extraction ----

#[test]
fn match_pattern_literal_and_wildcard() {
    let pat = parse_pattern("/characters/{id}");
    let args = match_pattern(&pat, &path_segments("/characters/42")).unwrap();
    assert_eq!(args.get("id").map(String::as_str), Some("42"));

    // Wrong literal, wrong arity → no match.
    assert!(match_pattern(&pat, &path_segments("/players/42")).is_none());
    assert!(match_pattern(&pat, &path_segments("/characters")).is_none());
    assert!(match_pattern(&pat, &path_segments("/characters/42/extra")).is_none());
}

#[test]
fn match_pattern_no_wildcards() {
    let pat = parse_pattern("/characters");
    assert!(match_pattern(&pat, &path_segments("/characters")).unwrap().is_empty());
    assert!(match_pattern(&pat, &path_segments("/characters/1")).is_none());
}

// ---- (d) select_backend picks Local iff an invoker exists ----

#[test]
fn select_kind_local_when_invoker_present_else_remote() {
    let mut invokers: HashMap<String, LocalInvoker> = HashMap::new();
    invokers.insert("characters.create".into(), echo_invoker());
    assert_eq!(select_kind(&invokers, "characters.create"), BackendKind::Local);
    assert_eq!(select_kind(&invokers, "inventory.grant"), BackendKind::Remote);
}

#[test]
fn provider_of_takes_prefix() {
    assert_eq!(provider_of("characters.create"), "characters");
    assert_eq!(provider_of("bare"), "bare");
}

// ---- (b) auth-once ----

#[tokio::test]
async fn dev_verifier_accepts_dev_token_only() {
    let v = DevSessionVerifier::new();
    assert_eq!(v.verify("dev-alice").await.unwrap().as_deref(), Some("alice"));
    assert_eq!(v.verify("dev-").await.unwrap(), None); // empty suffix rejected
    assert_eq!(v.verify("alice").await.unwrap(), None); // no prefix rejected
    assert_eq!(v.verify("").await.unwrap(), None);
}

/// A [`SessionVerifier`] that always reports its dependency unreachable — the
/// gateway stand-in for an accounts-svc outage. Distinct from `Ok(None)` (a
/// definitively invalid token), it must surface as 503 / `Status::Unavailable`.
struct UnavailableVerifier;

#[async_trait::async_trait]
impl SessionVerifier for UnavailableVerifier {
    async fn verify(&self, _token: &str) -> Result<Option<String>, VerifyUnavailable> {
        Err(VerifyUnavailable)
    }
}

#[tokio::test]
async fn authenticate_paths() {
    let v = DevSessionVerifier::new();

    // Valid bearer → identity threaded.
    let mut h = HeaderMap::new();
    h.insert(header::AUTHORIZATION, HeaderValue::from_static("Bearer dev-alice"));
    let id = authenticate(&h, &v).await.unwrap();
    assert_eq!(id.player_id(), Some("alice"));

    // Missing header → 401.
    let empty = HeaderMap::new();
    let resp = authenticate(&empty, &v).await.unwrap_err();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    // Invalid token → 401.
    let mut bad = HeaderMap::new();
    bad.insert(header::AUTHORIZATION, HeaderValue::from_static("Bearer nope"));
    let resp = authenticate(&bad, &v).await.unwrap_err();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn authenticate_verifier_outage_is_503_not_401() {
    // A verifier that cannot reach accounts must surface as 503 SERVICE_UNAVAILABLE —
    // NOT 401 (which would mass-log-out players the moment accounts blips). The token
    // is well-formed; only the dependency is down.
    let v = UnavailableVerifier;
    let mut h = HeaderMap::new();
    h.insert(header::AUTHORIZATION, HeaderValue::from_static("Bearer dev-alice"));
    let resp = authenticate(&h, &v).await.unwrap_err();
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
}

// ---- (b/c) end-to-end through the axum fallback ----

/// A LocalInvoker echoing the caller identity + the wire request bytes as a
/// JSON object (an AuthPlayer op: rejects a missing identity with Invalid).
fn echo_invoker() -> LocalInvoker {
    Arc::new(|ident: Identity, req: Vec<u8>| {
        Box::pin(async move {
            let pid = ident
                .player_id()
                .ok_or_else(|| Error::invalid("no identity"))?
                .to_string();
            let req = String::from_utf8(req).unwrap();
            Ok(format!(r#"{{"status":"Ok","pid":"{pid}","echo":{req}}}"#).into_bytes())
        })
    })
}

/// Builds a demo `OpSet` for `POST /demo/{id}` (AuthPlayer, success 200) whose
/// decode packs `{id, body}`, invoke echoes identity, and encode drops the status
/// envelope on Ok / surfaces a non-Ok status as an `Err`.
fn demo_opset() -> OpSet {
    let decode: DecodeFn = Arc::new(|body, path| {
        let id = path.get("id").cloned().unwrap_or_default();
        let body = body.unwrap_or(b"null");
        Ok(format!(
            r#"{{"id":"{id}","body":{}}}"#,
            std::str::from_utf8(body).map_err(|e| Error::invalid(e.to_string()))?
        )
        .into_bytes())
    });
    let encode: EncodeFn = Arc::new(|resp: &[u8]| {
        let v: serde_json::Value =
            serde_json::from_slice(resp).map_err(|e| Error::internal(e.to_string()))?;
        let status = v.get("status").and_then(|s| s.as_str()).unwrap_or("Ok");
        if status != "Ok" {
            return Err(Error::new(Status::NotFound, "demo not found"));
        }
        Ok((Some(resp.to_vec()), Status::Ok))
    });
    OpSet {
        operation: Operation {
            method: "demo.echo".into(),
            verb: "POST".into(),
            path: "/demo/{id}".into(),
            auth: AuthReq::Player,
            success: 200,
        },
        binding: OpBinding { method: "demo.echo".into(), decode, encode },
        local: LocalOp { method: "demo.echo".into(), invoke: echo_invoker() },
    }
}

/// Wires a `FrontDoor` over a `Slots` carrying the demo op, so a test can drive
/// either plane (the axum fallback or the player handler) through it. Keys resolve
/// via [`demo_keys`] — happy paths send [`TEST_KEY`].
fn demo_front_door() -> Arc<FrontDoor> {
    let slots = Arc::new(Slots::new());
    let op = demo_opset();
    slots.contribute(opsapi::SLOT, op.operation);
    slots.contribute(opsapi::BINDING_SLOT, op.binding);
    slots.contribute(opsapi::LOCAL_SLOT, op.local);
    front_door_with_keys(slots, demo_keys())
}

fn demo_router() -> Router {
    demo_front_door().router()
}

async fn body_string(resp: Response) -> (StatusCode, String) {
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    (status, String::from_utf8(bytes.to_vec()).unwrap())
}

#[tokio::test]
async fn end_to_end_decode_invoke_encode() {
    let router = demo_router();
    let req = HttpRequest::builder()
        .method("POST")
        .uri("/demo/42")
        .header(header::AUTHORIZATION, "Bearer dev-alice")
        .header("X-Api-Key", TEST_KEY)
        .body(Body::from("123"))
        .unwrap();
    let resp = router.oneshot(req).await.unwrap();
    let (status, body) = body_string(resp).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains(r#""pid":"alice""#), "{body}");
    assert!(body.contains(r#""id":"42""#), "{body}");
    assert!(body.contains(r#""body":123"#), "{body}");
}

#[tokio::test]
async fn end_to_end_missing_bearer_is_401_before_dispatch() {
    // A VALID key, so the request passes the key check and fails at session auth —
    // distinguishing the two 401s by body.
    let router = demo_router();
    let req = HttpRequest::builder()
        .method("POST")
        .uri("/demo/42")
        .header("X-Api-Key", TEST_KEY)
        .body(Body::from("1"))
        .unwrap();
    let resp = router.oneshot(req).await.unwrap();
    let (status, body) = body_string(resp).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body, "unauthorized");
}

#[tokio::test]
async fn end_to_end_unmatched_route_is_404() {
    let router = demo_router();
    let req = HttpRequest::builder()
        .method("GET")
        .uri("/nope")
        .body(Body::empty())
        .unwrap();
    let resp = router.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn end_to_end_domain_status_maps_to_http() {
    // Drive an encode that surfaces a non-Ok status: build an op whose invoker
    // returns a non-Ok status envelope, proving encode-Err → mapped HTTP code.
    let slots = Arc::new(Slots::new());
    let decode: DecodeFn = Arc::new(|_b, _p| Ok(b"null".to_vec()));
    let invoke: LocalInvoker = Arc::new(|_id, _req| {
        Box::pin(async move { Ok(br#"{"status":"NotFound"}"#.to_vec()) })
    });
    let encode: EncodeFn = Arc::new(|resp: &[u8]| {
        let v: serde_json::Value = serde_json::from_slice(resp).unwrap();
        if v.get("status").and_then(|s| s.as_str()) != Some("Ok") {
            return Err(Error::new(Status::NotFound, "missing"));
        }
        Ok((Some(resp.to_vec()), Status::Ok))
    });
    slots.contribute(
        opsapi::SLOT,
        Operation {
            method: "demo.get".into(),
            verb: "GET".into(),
            path: "/demo/{id}".into(),
            auth: AuthReq::None,
            success: 200,
        },
    );
    slots.contribute(opsapi::BINDING_SLOT, OpBinding { method: "demo.get".into(), decode, encode });
    slots.contribute(opsapi::LOCAL_SLOT, LocalOp { method: "demo.get".into(), invoke });
    let front = front_door_with_keys(slots, demo_keys());
    let router = front.router();

    let req = HttpRequest::builder()
        .method("GET")
        .uri("/demo/7")
        .header("X-Api-Key", TEST_KEY)
        .body(Body::empty())
        .unwrap();
    let resp = router.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ---- route-pattern label stamped for the metrics layer ----

/// A matched op stamps its route PATTERN (`op.path`) into the response extensions, so
/// `metrics::record` labels it by pattern instead of the fallback's absent `MatchedPath`.
#[tokio::test]
async fn front_door_stamps_route_pattern_on_matched_op() {
    let router = demo_router();
    let req = HttpRequest::builder()
        .method("POST")
        .uri("/demo/42")
        .header(header::AUTHORIZATION, "Bearer dev-alice")
        .header("X-Api-Key", TEST_KEY)
        .body(Body::from("1"))
        .unwrap();
    let resp = router.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let pat = resp
        .extensions()
        .get::<httpmw::RoutePattern>()
        .expect("a matched op response must carry its route pattern");
    assert_eq!(pat.as_str(), "/demo/{id}", "the PATTERN, never the raw /demo/42");
}

/// The stamp lands on EVERY post-match outcome, including an early auth failure (a 401 on
/// `/demo/42` must still be labelled `/demo/{id}`, not `unmatched`).
#[tokio::test]
async fn front_door_stamps_route_pattern_on_auth_failure() {
    let router = demo_router();
    let req = HttpRequest::builder()
        .method("POST")
        .uri("/demo/42")
        .header("X-Api-Key", TEST_KEY)
        .body(Body::from("1"))
        .unwrap();
    let resp = router.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    let pat = resp
        .extensions()
        .get::<httpmw::RoutePattern>()
        .expect("an auth-failure response must still carry the op's route pattern");
    assert_eq!(pat.as_str(), "/demo/{id}");
}

/// An unmatched route with no proxy prefix configured carries NO pattern → `metrics`
/// records it under `"unmatched"` (the prior behaviour is preserved).
#[tokio::test]
async fn front_door_leaves_unmatched_route_unlabelled() {
    let router = demo_router();
    let req = HttpRequest::builder()
        .method("GET")
        .uri("/nope")
        .body(Body::empty())
        .unwrap();
    let resp = router.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    assert!(
        resp.extensions().get::<httpmw::RoutePattern>().is_none(),
        "an unmatched, unproxied route must not be labelled (stays \"unmatched\")"
    );
}

// ---- find_by_method: the player plane's lookup ----

#[test]
fn find_by_method_hit_and_miss() {
    let slots = Slots::new();
    let op = demo_opset();
    slots.contribute(opsapi::SLOT, op.operation);
    slots.contribute(opsapi::BINDING_SLOT, op.binding);
    slots.contribute(opsapi::LOCAL_SLOT, op.local);
    let table = RouteTable::build(&slots).expect("single well-formed op builds");

    let route = table.find_by_method("demo.echo").expect("bound op is found");
    assert_eq!(route.op.method, "demo.echo");
    assert_eq!(route.op.auth, AuthReq::Player);

    // A wire-only internal method was never contributed → invisible (the
    // allow-list gate the player plane relies on).
    assert!(table.find_by_method("characters.ownerOf").is_none());
}

// ---- RouteTable::build: startup-time collision detection ----

/// Builds an `(Operation, OpBinding)` pair for `method` at `path` (GET, AuthNone) — the
/// minimum a route needs to reach the build's collision checks (no local invoker).
fn route_pair(method: &str, path: &str) -> (Operation, OpBinding) {
    let decode: DecodeFn = Arc::new(|_b, _p| Ok(b"null".to_vec()));
    let encode: EncodeFn = Arc::new(|r: &[u8]| Ok((Some(r.to_vec()), Status::Ok)));
    (
        Operation {
            method: method.into(),
            verb: "GET".into(),
            path: path.into(),
            auth: AuthReq::None,
            success: 200,
        },
        OpBinding { method: method.into(), decode, encode },
    )
}

#[test]
fn build_rejects_duplicate_method_id() {
    // Two full OpSets for the SAME method id `demo.echo` — a wiring bug (two modules
    // claiming one op) that used to resolve to a silent last-write-wins hybrid.
    let slots = Slots::new();
    let a = demo_opset();
    let b = demo_opset();
    slots.contribute(opsapi::SLOT, a.operation);
    slots.contribute(opsapi::BINDING_SLOT, a.binding);
    slots.contribute(opsapi::LOCAL_SLOT, a.local);
    slots.contribute(opsapi::SLOT, b.operation);
    slots.contribute(opsapi::BINDING_SLOT, b.binding);
    slots.contribute(opsapi::LOCAL_SLOT, b.local);

    let err = RouteTable::build(&slots).err().expect("build must fail with a collision").to_string();
    assert!(err.contains("demo.echo"), "the bail must name the colliding method: {err}");
}

#[test]
fn build_rejects_overlapping_route_with_differently_named_wildcards() {
    // Distinct method ids (so the method-id check passes) but the SAME verb + path
    // shape, differing only in the wildcard NAME — which `match_pattern` never reads,
    // so both accept `/char/<anything>`. Must be rejected wildcard-name-blind.
    let slots = Slots::new();
    let (op1, b1) = route_pair("char.byId", "/char/{id}");
    let (op2, b2) = route_pair("char.byName", "/char/{name}");
    slots.contribute(opsapi::SLOT, op1);
    slots.contribute(opsapi::BINDING_SLOT, b1);
    slots.contribute(opsapi::SLOT, op2);
    slots.contribute(opsapi::BINDING_SLOT, b2);

    let err = RouteTable::build(&slots).err().expect("build must fail with a collision").to_string();
    assert!(
        err.contains("char.byId") && err.contains("char.byName"),
        "the bail must name BOTH colliding routes: {err}"
    );
}

#[test]
fn build_accepts_same_shape_different_literals() {
    // Same wildcard SHAPE (`/{lit}/{id}`) but a different literal first segment → the
    // two routes accept disjoint request sets, so this is NOT a collision.
    let slots = Slots::new();
    let (op1, b1) = route_pair("char.byId", "/char/{id}");
    let (op2, b2) = route_pair("item.byId", "/item/{id}");
    slots.contribute(opsapi::SLOT, op1);
    slots.contribute(opsapi::BINDING_SLOT, b1);
    slots.contribute(opsapi::SLOT, op2);
    slots.contribute(opsapi::BINDING_SLOT, b2);

    let table = RouteTable::build(&slots).expect("distinct-literal routes must build");
    assert_eq!(table.routes.len(), 2);
}

#[test]
fn build_rejects_duplicate_peer_provider() {
    // Two PeerAddrs for one provider (different addresses) → two remote::Stubs wired
    // the same provider, an ambiguous dispatch target.
    let slots = Slots::new();
    slots.contribute(
        opsapi::PEER_SLOT,
        opsapi::PeerAddr { provider: "characters".into(), addr: "127.0.0.1:9000".into() },
    );
    slots.contribute(
        opsapi::PEER_SLOT,
        opsapi::PeerAddr { provider: "characters".into(), addr: "127.0.0.1:9001".into() },
    );

    let err = RouteTable::build(&slots).err().expect("build must fail with a collision").to_string();
    assert!(err.contains("characters"), "the bail must name the colliding provider: {err}");
}

// ---- the player handler: the pinned {status, err} grammar on every outcome ----

/// Drives the player handler exactly as the `edge::PlayerServer` would, returning
/// the response payload as a string. The handler is Ok on EVERY domain outcome
/// (the pinned grammar), so unwrapping here is itself an assertion.
async fn call_player(
    front: &Arc<FrontDoor>,
    method: &str,
    token: Option<&str>,
    api_key: Option<&str>,
    payload: &[u8],
) -> String {
    let h = front.player_handler();
    let bytes = h(
        method.to_string(),
        token.map(str::to_string),
        api_key.map(str::to_string),
        payload.to_vec(),
    )
    .await
    .expect("domain outcomes never surface as transport Err");
    String::from_utf8(bytes).unwrap()
}

#[tokio::test]
async fn player_missing_token_on_auth_op_is_unauthorized_envelope() {
    let front = demo_front_door();
    // A valid key, so the failure is the SESSION's, not the key check's.
    let body = call_player(&front, "demo.echo", None, Some(TEST_KEY), br#"{"n":1}"#).await;
    // Exact macro grammar: field `err`, Status as bare variant name.
    assert_eq!(body, r#"{"status":"Unauthorized","err":"unauthorized"}"#);
}

#[tokio::test]
async fn player_bad_token_is_unauthorized_envelope() {
    let front = demo_front_door();
    let body =
        call_player(&front, "demo.echo", Some("nope-x"), Some(TEST_KEY), br#"{"n":1}"#).await;
    assert_eq!(body, r#"{"status":"Unauthorized","err":"unauthorized"}"#);
}

#[tokio::test]
async fn player_verifier_outage_is_unavailable_envelope() {
    // A well-formed token whose verification cannot reach accounts → Unavailable
    // envelope (503), NOT Unauthorized: an outage must not read as an invalid session.
    let slots = Arc::new(Slots::new());
    let op = demo_opset();
    slots.contribute(opsapi::SLOT, op.operation);
    slots.contribute(opsapi::BINDING_SLOT, op.binding);
    slots.contribute(opsapi::LOCAL_SLOT, op.local);
    let front = Arc::new(FrontDoor::new(
        slots,
        Arc::new(UnavailableVerifier),
        demo_keys(),
        Vec::new(),
    ));
    let body = call_player(&front, "demo.echo", Some("dev-alice"), Some(TEST_KEY), br#"{"n":1}"#).await;
    assert_eq!(
        body,
        r#"{"status":"Unavailable","err":"session verification unavailable"}"#
    );
}

#[tokio::test]
async fn player_unknown_method_is_not_found_envelope() {
    let front = demo_front_door();
    // `characters.ownerOf` is the canonical wire-only internal: a peer edge
    // serves it, but it is absent from the route table → not player-reachable.
    let body =
        call_player(&front, "characters.ownerOf", Some("dev-alice"), Some(TEST_KEY), b"{}").await;
    assert_eq!(body, r#"{"status":"NotFound","err":"unknown operation"}"#);
}

#[tokio::test]
async fn player_malformed_json_is_invalid_at_the_front() {
    let front = demo_front_door();
    let body =
        call_player(&front, "demo.echo", Some("dev-alice"), Some(TEST_KEY), b"{not json").await;
    assert_eq!(body, r#"{"status":"Invalid","err":"malformed request payload"}"#);
}

#[tokio::test]
async fn player_happy_path_returns_wire_response_verbatim() {
    let front = demo_front_door();
    // No OpBinding::decode on this plane: the payload IS the wire request.
    let body =
        call_player(&front, "demo.echo", Some("dev-alice"), Some(TEST_KEY), br#"{"n":1}"#).await;
    assert!(body.contains(r#""status":"Ok""#), "{body}");
    assert!(body.contains(r#""pid":"alice""#), "{body}");
    assert!(body.contains(r#""echo":{"n":1}"#), "{body}");
}

#[tokio::test]
async fn player_auth_none_op_runs_with_no_identity() {
    // An AuthNone op whose invoker ASSERTS it received no identity.
    let slots = Arc::new(Slots::new());
    let decode: DecodeFn = Arc::new(|_b, _p| Ok(b"null".to_vec()));
    let encode: EncodeFn = Arc::new(|resp: &[u8]| Ok((Some(resp.to_vec()), Status::Ok)));
    let invoke: LocalInvoker = Arc::new(|ident: Identity, _req| {
        Box::pin(async move {
            if ident.player_id().is_some() {
                return Err(Error::internal("AuthNone op must see Identity::none()"));
            }
            Ok(br#"{"status":"Ok","anon":true}"#.to_vec())
        })
    });
    slots.contribute(
        opsapi::SLOT,
        Operation {
            method: "demo.public".into(),
            verb: "GET".into(),
            path: "/public".into(),
            auth: AuthReq::None,
            success: 200,
        },
    );
    slots.contribute(
        opsapi::BINDING_SLOT,
        OpBinding { method: "demo.public".into(), decode, encode },
    );
    slots.contribute(opsapi::LOCAL_SLOT, LocalOp { method: "demo.public".into(), invoke });
    let front = front_door_with_keys(slots, demo_keys());

    // No token at all — must dispatch, not 401. (A key is still required: the key
    // gates the CLIENT class even on an AuthNone op.)
    let body = call_player(&front, "demo.public", None, Some(TEST_KEY), b"{}").await;
    assert_eq!(body, r#"{"status":"Ok","anon":true}"#);
}

#[tokio::test]
async fn player_backend_error_is_reserialized_as_status_err_envelope() {
    // A backend failure (an Err(opsapi::Error), not a status-carrying payload)
    // must still come back in the pinned {status, err} grammar. Drive it with an
    // op that has no local invoker AND no PeerAddr contributed: dispatch fails
    // with Unavailable, which the front re-serializes as the envelope.
    let slots = Arc::new(Slots::new());
    let decode: DecodeFn = Arc::new(|_b, _p| Ok(b"null".to_vec()));
    let encode: EncodeFn = Arc::new(|resp: &[u8]| Ok((Some(resp.to_vec()), Status::Ok)));
    slots.contribute(
        opsapi::SLOT,
        Operation {
            method: "ghostprov.op".into(),
            verb: "GET".into(),
            path: "/ghost".into(),
            auth: AuthReq::None,
            success: 200,
        },
    );
    slots.contribute(
        opsapi::BINDING_SLOT,
        OpBinding { method: "ghostprov.op".into(), decode, encode },
    );
    // NO LOCAL_SLOT contribution → Remote; no PeerAddr contributed for ghostprov,
    // so the front door has no peer address to dial.
    let remote_front = front_door_with_keys(slots, demo_keys());
    let body = call_player(&remote_front, "ghostprov.op", None, Some(TEST_KEY), b"{}").await;
    assert!(body.starts_with(r#"{"status":"Unavailable","err":""#), "{body}");
    assert!(body.contains("no peer contributed"), "{body}");
    assert!(body.contains("ghostprov"), "{body}");
}

// ---- the API-key check: policy evaluation ----

#[test]
fn policy_allows_full_and_exact_and_trimmed_lists() {
    // `full` allows everything, including a method invented tomorrow.
    assert!(policy_allows("full", "match.report"));
    assert!(policy_allows("full", "brand.newOp"));

    // Exact match in a comma list.
    assert!(policy_allows("accounts.login,characters.create", "characters.create"));
    assert!(!policy_allows("accounts.login,characters.create", "match.report"));

    // Entries are trimmed — a spaced list still matches.
    assert!(policy_allows("accounts.login, characters.create", "characters.create"));
    assert!(policy_allows("  demo.echo  ", "demo.echo"));

    // Empty policy allows nothing; an unknown method is denied by a restricted key
    // (the safe-by-default rule for new ops).
    assert!(!policy_allows("", "demo.echo"));
    assert!(!policy_allows("other.op", "demo.echo"));

    // `full` must be the WHOLE policy, not a list entry prefix quirk.
    assert!(!policy_allows("fullish.op", "demo.echo"));
}

// ---- the API-key check: HTTP plane (post-match, pre-auth) ----

#[tokio::test]
async fn http_missing_api_key_is_401() {
    let router = demo_router();
    // Bearer present, key absent → the KEY check answers first (it runs pre-auth).
    let req = HttpRequest::builder()
        .method("POST")
        .uri("/demo/42")
        .header(header::AUTHORIZATION, "Bearer dev-alice")
        .body(Body::from("1"))
        .unwrap();
    let resp = router.oneshot(req).await.unwrap();
    let (status, body) = body_string(resp).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body, "missing api key");
}

#[tokio::test]
async fn http_unknown_api_key_is_401() {
    let router = demo_router();
    let req = HttpRequest::builder()
        .method("POST")
        .uri("/demo/42")
        .header(header::AUTHORIZATION, "Bearer dev-alice")
        .header("X-Api-Key", "bogus-key")
        .body(Body::from("1"))
        .unwrap();
    let resp = router.oneshot(req).await.unwrap();
    let (status, body) = body_string(resp).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body, "invalid api key");
}

#[tokio::test]
async fn http_denied_method_is_403() {
    let router = demo_router();
    // LIMITED_KEY is valid but its policy allows only `other.op`, not `demo.echo`.
    let req = HttpRequest::builder()
        .method("POST")
        .uri("/demo/42")
        .header(header::AUTHORIZATION, "Bearer dev-alice")
        .header("X-Api-Key", LIMITED_KEY)
        .body(Body::from("1"))
        .unwrap();
    let resp = router.oneshot(req).await.unwrap();
    let (status, body) = body_string(resp).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body, "api key policy forbids this operation");
}

/// A non-op route never reaches the key check: an unmatched keyless request stays a
/// plain 404 (the `/healthz`/`/metrics`/passthrough carve-out, at the unit level).
#[tokio::test]
async fn http_unmatched_route_needs_no_api_key() {
    let router = demo_router();
    let req = HttpRequest::builder().method("GET").uri("/nope").body(Body::empty()).unwrap();
    let resp = router.oneshot(req).await.unwrap();
    let (status, body) = body_string(resp).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_ne!(body, "missing api key");
}

// ---- the API-key check: player plane (post-match, pre-auth, envelope grammar) ----

#[tokio::test]
async fn player_missing_api_key_is_unauthorized_envelope() {
    let front = demo_front_door();
    let body = call_player(&front, "demo.echo", Some("dev-alice"), None, br#"{"n":1}"#).await;
    assert_eq!(body, r#"{"status":"Unauthorized","err":"missing api key"}"#);
}

#[tokio::test]
async fn player_unknown_api_key_is_unauthorized_envelope() {
    let front = demo_front_door();
    let body =
        call_player(&front, "demo.echo", Some("dev-alice"), Some("bogus-key"), br#"{"n":1}"#)
            .await;
    assert_eq!(body, r#"{"status":"Unauthorized","err":"invalid api key"}"#);
}

#[tokio::test]
async fn player_denied_method_is_forbidden_envelope() {
    let front = demo_front_door();
    let body =
        call_player(&front, "demo.echo", Some("dev-alice"), Some(LIMITED_KEY), br#"{"n":1}"#)
            .await;
    assert_eq!(
        body,
        r#"{"status":"Forbidden","err":"api key policy forbids this operation"}"#
    );
}

/// The ordering guarantee split-proof P5 relies on: the key check runs AFTER
/// `find_by_method`, so an unknown method stays NotFound even under a key whose
/// policy would deny it — method existence is never leaked through the key check.
#[tokio::test]
async fn player_unknown_method_stays_not_found_with_restrictive_key() {
    let front = demo_front_door();
    let body =
        call_player(&front, "characters.ownerOf", Some("dev-alice"), Some(LIMITED_KEY), b"{}")
            .await;
    assert_eq!(body, r#"{"status":"NotFound","err":"unknown operation"}"#);
}

// ---- RealKeyVerifier: the TTL cache over the apikeys capability ----

/// A scripted `apikeysapi::Keys`: pops the next response off a queue (falling back to
/// `Ok(Some(full))` when exhausted) and counts every capability hit — the seam the
/// cache assertions read.
struct ScriptedKeys {
    calls: AtomicUsize,
    responses: std::sync::Mutex<std::collections::VecDeque<Result<Option<KeyRecord>, Error>>>,
}

impl ScriptedKeys {
    fn new(responses: Vec<Result<Option<KeyRecord>, Error>>) -> Arc<ScriptedKeys> {
        Arc::new(ScriptedKeys {
            calls: AtomicUsize::new(0),
            responses: std::sync::Mutex::new(responses.into_iter().collect()),
        })
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl apikeysapi::Keys for ScriptedKeys {
    async fn lookup_key(&self, key: String) -> Result<Option<KeyRecord>, Error> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.responses.lock().unwrap().pop_front().unwrap_or_else(|| {
            Ok(Some(KeyRecord { name: key, policy: "full".to_string() }))
        })
    }
}

fn full_record(name: &str) -> Option<KeyRecord> {
    Some(KeyRecord { name: name.to_string(), policy: "full".to_string() })
}

#[tokio::test]
async fn key_cache_serves_repeat_lookup_without_requerying() {
    let keys = ScriptedKeys::new(vec![Ok(full_record("client"))]);
    let v = RealKeyVerifier::with_ttl(keys.clone(), Duration::from_secs(60));

    assert_eq!(v.lookup("k1").await.unwrap().name, "client");
    assert_eq!(v.lookup("k1").await.unwrap().name, "client");
    assert_eq!(keys.calls(), 1, "the second lookup must hit the cache");
}

#[tokio::test]
async fn key_cache_caches_ok_none_too() {
    // Ok(None) — a genuinely unknown key — IS cached (bounds bad-key spam): the
    // scripted second response would be Some, but it must never be consulted.
    let keys = ScriptedKeys::new(vec![Ok(None), Ok(full_record("client"))]);
    let v = RealKeyVerifier::with_ttl(keys.clone(), Duration::from_secs(60));

    assert!(v.lookup("unknown").await.is_none());
    assert!(v.lookup("unknown").await.is_none(), "cached Ok(None) must be served");
    assert_eq!(keys.calls(), 1);
}

#[tokio::test]
async fn key_cache_expired_entry_requeries() {
    // TTL zero: every entry is immediately stale, so each lookup re-consults the
    // capability (expiry without sleeping).
    let keys = ScriptedKeys::new(vec![]);
    let v = RealKeyVerifier::with_ttl(keys.clone(), Duration::ZERO);

    assert!(v.lookup("k1").await.is_some());
    assert!(v.lookup("k1").await.is_some());
    assert_eq!(keys.calls(), 2, "a stale entry must be re-queried");
}

#[tokio::test]
async fn key_cache_never_caches_an_err() {
    // First call errors (apikeys blip): THIS request collapses to None, but the
    // failure is NOT cached — the next request re-queries and gets the valid record
    // (an outage must not poison a valid key for a whole TTL).
    let keys = ScriptedKeys::new(vec![
        Err(Error::unavailable("apikeys unreachable")),
        Ok(full_record("client")),
    ]);
    let v = RealKeyVerifier::with_ttl(keys.clone(), Duration::from_secs(60));

    assert!(v.lookup("k1").await.is_none(), "an Err collapses to a per-request deny");
    assert_eq!(v.lookup("k1").await.unwrap().name, "client");
    assert_eq!(keys.calls(), 2, "the Err must not have been cached");
}

// ---- RemoteBackend exercised against a fake Caller ----

/// What a `FakeCaller` records for one relayed call: (method, identity, payload).
type Seen = (String, Option<String>, Vec<u8>);

struct FakeCaller {
    seen: std::sync::Mutex<Option<Seen>>,
}

#[async_trait::async_trait]
impl Caller for FakeCaller {
    async fn call(
        &self,
        method: &str,
        identity: Option<&str>,
        payload: &[u8],
    ) -> Result<Vec<u8>, Error> {
        *self.seen.lock().unwrap() =
            Some((method.to_string(), identity.map(str::to_string), payload.to_vec()));
        Ok(br#"{"status":"Ok","relayed":true}"#.to_vec())
    }
}

#[tokio::test]
async fn remote_backend_relays_method_identity_and_payload() {
    let caller = Arc::new(FakeCaller { seen: std::sync::Mutex::new(None) });
    let backend = RemoteBackend::new(caller.clone());
    let op = Operation {
        method: "characters.create".into(),
        verb: "POST".into(),
        path: "/characters".into(),
        auth: AuthReq::Player,
        success: 201,
    };
    let resp = backend
        .invoke(&op, Identity::player("bob"), b"{\"name\":\"x\"}".to_vec())
        .await
        .unwrap();
    assert_eq!(resp, br#"{"status":"Ok","relayed":true}"#);
    let seen = caller.seen.lock().unwrap().clone().unwrap();
    assert_eq!(seen.0, "characters.create");
    assert_eq!(seen.1.as_deref(), Some("bob"));
    assert_eq!(seen.2, b"{\"name\":\"x\"}");
}

#[tokio::test]
async fn local_backend_missing_invoker_is_internal_error() {
    let backend = LocalBackend::new(Arc::new(HashMap::new()));
    let op = Operation {
        method: "x.y".into(),
        verb: "POST".into(),
        path: "/x".into(),
        auth: AuthReq::None,
        success: 200,
    };
    let err = backend.invoke(&op, Identity::none(), vec![]).await.unwrap_err();
    assert_eq!(err.status, Status::Internal);
}

// ---- evict-on-error: a dead cached remote conn heals on the next request ----

/// A `Caller` that always fails, counting how many times it was tried — the
/// stand-in for a cached edge conn whose peer restarted.
#[derive(Default)]
struct FailingCaller {
    calls: AtomicUsize,
}

#[async_trait::async_trait]
impl Caller for FailingCaller {
    async fn call(&self, _m: &str, _i: Option<&str>, _p: &[u8]) -> Result<Vec<u8>, Error> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Err(Error::unavailable("fake: connection lost"))
    }
}

#[tokio::test]
async fn remote_dispatch_evicts_failed_caller_so_next_request_redials() {
    // A table with NO local invokers → every dispatch selects Remote.
    let table = RouteTable::build(&Slots::new()).expect("empty slots build");
    let op = Operation {
        method: "fakeprov.op".into(),
        verb: "POST".into(),
        path: "/fake".into(),
        auth: AuthReq::None,
        success: 200,
    };

    // Seed the cache as if a previous request had dialed this provider.
    let failing = Arc::new(FailingCaller::default());
    table
        .remotes
        .lock()
        .await
        .insert("fakeprov".into(), failing.clone() as Arc<dyn Caller>);

    // First request: the cached caller fails → the error propagates AND the
    // dead entry is evicted.
    let err = table.dispatch(&op, Identity::none(), b"{}".to_vec()).await.unwrap_err();
    assert_eq!(err.status, Status::Unavailable);
    assert_eq!(failing.calls.load(Ordering::SeqCst), 1);
    assert!(
        !table.remotes.lock().await.contains_key("fakeprov"),
        "failed caller must be evicted"
    );

    // Second request goes back through remote_caller (re-dial). With no PeerAddr
    // contributed for `fakeprov` the re-dial path itself errors — and crucially the
    // DEAD caller was NOT reused (its call count is unchanged).
    let err = table.dispatch(&op, Identity::none(), b"{}".to_vec()).await.unwrap_err();
    assert!(err.msg.contains("no peer contributed"), "{}", err.msg);
    assert_eq!(failing.calls.load(Ordering::SeqCst), 1, "dead caller must not be reused");

    // And once a re-dial succeeds (simulated: a fresh healthy caller lands in the
    // cache, exactly what remote_caller does after dialing), the route works again
    // — the self-heal, with exactly one failed request in between.
    let healthy = Arc::new(FakeCaller { seen: std::sync::Mutex::new(None) });
    table.remotes.lock().await.insert("fakeprov".into(), healthy as Arc<dyn Caller>);
    let resp = table.dispatch(&op, Identity::none(), b"{}".to_vec()).await.unwrap();
    assert_eq!(resp, br#"{"status":"Ok","relayed":true}"#);
    assert!(
        table.remotes.lock().await.contains_key("fakeprov"),
        "a successful call must keep its caller cached"
    );
}
