use super::keys::policy_allows;
use super::*;
use apikeysapi::KeyRecord;
use axum::http::Request as HttpRequest;
use opsapi::{DecodeFn, EncodeFn, LocalOp, OpSet, RetryMode, Status};
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
    async fn lookup(&self, key: &str) -> Result<Option<KeyRecord>, LookupUnavailable> {
        Ok(self
            .keys
            .get(key)
            .map(|policy| KeyRecord { name: key.to_string(), policy: policy.clone() }))
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
    let over_cap_dev = format!(
        "dev-{}",
        "x".repeat(accountsapi::MAX_SESSION_TOKEN_BYTES)
    );
    assert_eq!(
        v.verify(&over_cap_dev).await.unwrap(),
        None,
        "the explicit dev fallback must honor the shared session-token byte cap"
    );
}

/// A topology-neutral stand-in for `accounts.sessions`: locally this trait object is
/// the accounts service, while in a split gateway it is the generated RPC client.
/// Counting calls therefore proves an over-cap bearer reaches neither topology.
#[derive(Default)]
struct CountingSessions {
    calls: AtomicUsize,
}

#[async_trait::async_trait]
impl accountsapi::Sessions for CountingSessions {
    async fn verify_session(&self, _token: String) -> Result<Option<String>, Error> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(Some("counted-player".to_string()))
    }
}

#[tokio::test]
async fn sessions_verifier_caps_before_local_or_remote_capability_dispatch() {
    let sessions = Arc::new(CountingSessions::default());
    let verifier = SessionsVerifier::new(sessions.clone());

    let over_cap = "x".repeat(accountsapi::MAX_SESSION_TOKEN_BYTES + 1);
    assert_eq!(verifier.verify(&over_cap).await.unwrap(), None);
    assert_eq!(sessions.calls.load(Ordering::SeqCst), 0);

    let at_cap = "x".repeat(accountsapi::MAX_SESSION_TOKEN_BYTES);
    assert_eq!(
        verifier.verify(&at_cap).await.unwrap().as_deref(),
        Some("counted-player")
    );
    assert_eq!(
        sessions.calls.load(Ordering::SeqCst),
        1,
        "the exact 128-byte boundary remains eligible for verification"
    );
}

// The always-unavailable [`SessionVerifier`] fixture now lives in the always-compiled
// `conformance` module (the harness probes it through the real `authenticate`); the
// tests re-import it from there.
use super::conformance::UnavailableVerifier;

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

#[tokio::test]
async fn authenticate_overlong_token_is_401_without_capability_dispatch() {
    let sessions = Arc::new(CountingSessions::default());
    let verifier = SessionsVerifier::new(sessions.clone());
    let token = "x".repeat(accountsapi::MAX_SESSION_TOKEN_BYTES + 1);
    let mut headers = HeaderMap::new();
    headers.insert(
        header::AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
    );

    let resp = authenticate(&headers, &verifier).await.unwrap_err();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(sessions.calls.load(Ordering::SeqCst), 0);
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
            retry_mode: RetryMode::Never,
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
            retry_mode: RetryMode::Never,
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
            retry_mode: RetryMode::Never,
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
fn build_rejects_overlapping_route_with_literal_vs_wildcard() {
    // The bug this step closes: `pattern_shape_eq` (Wild==Wild, Lit==Lit only) let
    // GET /x/{id} and GET /x/me both register because their SHAPES differ (Wild vs
    // Lit at position 2) — yet a request to `/x/me` matches both patterns, so
    // `find()` silently picked whichever was contributed first. `pattern_overlaps`
    // must reject this pair.
    let slots = Slots::new();
    let (op1, b1) = route_pair("x.byId", "/x/{id}");
    let (op2, b2) = route_pair("x.me", "/x/me");
    slots.contribute(opsapi::SLOT, op1);
    slots.contribute(opsapi::BINDING_SLOT, b1);
    slots.contribute(opsapi::SLOT, op2);
    slots.contribute(opsapi::BINDING_SLOT, b2);

    let err = RouteTable::build(&slots).err().expect("build must fail with a collision").to_string();
    assert!(
        err.contains("x.byId") && err.contains("x.me"),
        "the bail must name BOTH colliding methods: {err}"
    );
    assert!(err.contains("/x/{id}") && err.contains("/x/me"), "the bail must name BOTH paths: {err}");
}

// ---- pattern_overlaps: the request-set-overlap matrix ----

#[test]
fn pattern_overlaps_matrix() {
    // Lit vs Lit, equal → overlaps (the plain duplicate case `pattern_shape_eq` also
    // caught — must stay caught).
    assert!(pattern_overlaps(&parse_pattern("/x/me"), &parse_pattern("/x/me")));
    // Lit vs Lit, different → no overlap.
    assert!(!pattern_overlaps(&parse_pattern("/x/me"), &parse_pattern("/x/you")));
    // Wild vs Wild → overlaps regardless of the wildcard's name.
    assert!(pattern_overlaps(&parse_pattern("/char/{id}"), &parse_pattern("/char/{name}")));
    // Lit vs Wild (either order) → overlaps: this is the case `pattern_shape_eq`
    // missed (`/x/{id}` vs `/x/me`).
    assert!(pattern_overlaps(&parse_pattern("/x/{id}"), &parse_pattern("/x/me")));
    assert!(pattern_overlaps(&parse_pattern("/x/me"), &parse_pattern("/x/{id}")));
    // Different lengths → never overlap, regardless of segment content.
    assert!(!pattern_overlaps(&parse_pattern("/x/{id}"), &parse_pattern("/x/{id}/extra")));
    assert!(!pattern_overlaps(&parse_pattern("/x"), &parse_pattern("/x/y")));
    // Different literal PREFIX with the same wildcard shape → no overlap (disjoint
    // request sets, not a collision).
    assert!(!pattern_overlaps(&parse_pattern("/char/{id}"), &parse_pattern("/item/{id}")));
}

#[test]
fn build_rejects_duplicate_peer_provider() {
    // Two PeerAddrs for one provider (different addresses) → two remote::Stubs wired
    // the same provider, an ambiguous dispatch target.
    let slots = Slots::new();
    slots.contribute(
        opsapi::PEER_SLOT,
        opsapi::PeerAddr { provider: "characters".into(), addrs: vec!["127.0.0.1:9000".into()] },
    );
    slots.contribute(
        opsapi::PEER_SLOT,
        opsapi::PeerAddr { provider: "characters".into(), addrs: vec!["127.0.0.1:9001".into()] },
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
async fn player_overlong_token_is_unauthorized_without_capability_dispatch() {
    let sessions = Arc::new(CountingSessions::default());
    let slots = Arc::new(Slots::new());
    let op = demo_opset();
    slots.contribute(opsapi::SLOT, op.operation);
    slots.contribute(opsapi::BINDING_SLOT, op.binding);
    slots.contribute(opsapi::LOCAL_SLOT, op.local);
    let front = Arc::new(FrontDoor::new(
        slots,
        Arc::new(SessionsVerifier::new(sessions.clone())),
        demo_keys(),
        Vec::new(),
    ));
    let token = "x".repeat(accountsapi::MAX_SESSION_TOKEN_BYTES + 1);

    let body = call_player(
        &front,
        "demo.echo",
        Some(&token),
        Some(TEST_KEY),
        br#"{"n":1}"#,
    )
    .await;
    assert_eq!(body, r#"{"status":"Unauthorized","err":"unauthorized"}"#);
    assert_eq!(sessions.calls.load(Ordering::SeqCst), 0);
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
            retry_mode: RetryMode::Never,
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
            retry_mode: RetryMode::Never,
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

    assert_eq!(v.lookup("k1").await.unwrap().unwrap().name, "client");
    assert_eq!(v.lookup("k1").await.unwrap().unwrap().name, "client");
    assert_eq!(keys.calls(), 1, "the second lookup must hit the cache");
}

#[tokio::test]
async fn key_cache_caches_ok_none_too() {
    // Ok(None) — a genuinely unknown key — IS cached (bounds bad-key spam): the
    // scripted second response would be Some, but it must never be consulted.
    let keys = ScriptedKeys::new(vec![Ok(None), Ok(full_record("client"))]);
    let v = RealKeyVerifier::with_ttl(keys.clone(), Duration::from_secs(60));

    assert!(v.lookup("unknown").await.unwrap().is_none());
    assert!(v.lookup("unknown").await.unwrap().is_none(), "cached Ok(None) must be served");
    assert_eq!(keys.calls(), 1);
}

#[tokio::test]
async fn key_cache_expired_entry_requeries() {
    // TTL zero: every entry is immediately stale, so each lookup re-consults the
    // capability (expiry without sleeping).
    let keys = ScriptedKeys::new(vec![]);
    let v = RealKeyVerifier::with_ttl(keys.clone(), Duration::ZERO);

    assert!(v.lookup("k1").await.unwrap().is_some());
    assert!(v.lookup("k1").await.unwrap().is_some());
    assert_eq!(keys.calls(), 2, "a stale entry must be re-queried");
}

#[tokio::test]
async fn key_cache_never_caches_an_err() {
    // First call errors (apikeys blip): THIS request surfaces LookupUnavailable (a
    // retryable 503, NOT a false 401), and the failure is NOT cached — the next
    // request re-queries and gets the valid record (an outage must not poison a
    // valid key for a whole TTL).
    let keys = ScriptedKeys::new(vec![
        Err(Error::unavailable("apikeys unreachable")),
        Ok(full_record("client")),
    ]);
    let v = RealKeyVerifier::with_ttl(keys.clone(), Duration::from_secs(60));

    assert!(
        matches!(v.lookup("k1").await, Err(LookupUnavailable)),
        "a store Err must surface as LookupUnavailable, not a key verdict"
    );
    assert_eq!(v.lookup("k1").await.unwrap().unwrap().name, "client");
    assert_eq!(keys.calls(), 2, "the Err must not have been cached");
}

#[tokio::test]
async fn overlong_key_never_reaches_capability() {
    // An over-length string is definitively NOT a key: Ok(None) → 401, not a 503.
    let keys = ScriptedKeys::new(vec![]);
    let v = RealKeyVerifier::with_ttl(keys.clone(), Duration::from_secs(60));
    assert!(v.lookup(&"x".repeat(257)).await.unwrap().is_none());
    assert_eq!(keys.calls(), 0);
}

#[tokio::test]
async fn concurrent_same_key_miss_is_single_flight() {
    let keys = ScriptedKeys::new(vec![Ok(full_record("client"))]);
    let v = Arc::new(RealKeyVerifier::with_ttl(keys.clone(), Duration::from_secs(60)));
    let mut tasks = Vec::new();
    for _ in 0..32 {
        let v = v.clone();
        tasks.push(tokio::spawn(async move { v.lookup("same").await }));
    }
    for task in tasks { assert!(task.await.unwrap().unwrap().is_some()); }
    assert_eq!(keys.calls(), 1);
}

/// An `apikeysapi::Keys` whose lookups park on a test-held gate — the fixture that
/// keeps N capability calls in flight so the global semaphore can be saturated.
struct BlockingKeys {
    calls: AtomicUsize,
    gate: tokio::sync::Mutex<()>,
}

#[async_trait::async_trait]
impl apikeysapi::Keys for BlockingKeys {
    async fn lookup_key(&self, key: String) -> Result<Option<KeyRecord>, Error> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let _parked = self.gate.lock().await;
        Ok(Some(KeyRecord { name: key, policy: "full".to_string() }))
    }
}

#[tokio::test]
async fn global_semaphore_shed_is_unavailable_not_invalid() {
    // Saturate the global in-flight semaphore (KEY_LOOKUP_MAX_IN_FLIGHT = 64) with 64
    // DISTINCT uncached keys parked inside the capability, then look up a 65th
    // distinct key: the shed must be Err(LookupUnavailable) → KeyDenial::Unavailable
    // → Status::Unavailable (503) — a valid-but-uncached key under distinct-key spam
    // must NOT be told "invalid api key" (401).
    let keys = Arc::new(BlockingKeys {
        calls: AtomicUsize::new(0),
        gate: tokio::sync::Mutex::new(()),
    });
    let held_gate = keys.gate.lock().await;
    let v = Arc::new(RealKeyVerifier::with_ttl(keys.clone(), Duration::from_secs(60)));

    let mut tasks = Vec::new();
    for i in 0..64 {
        let v = v.clone();
        tasks.push(tokio::spawn(async move { v.lookup(&format!("k{i}")).await }));
    }
    // Wait until all 64 hold a permit (they are parked inside lookup_key).
    while keys.calls.load(Ordering::SeqCst) < 64 {
        tokio::task::yield_now().await;
    }

    // The 65th distinct key is shed — through check_api_key it must map to
    // Unavailable/503, never Invalid/401.
    assert!(matches!(v.lookup("valid-but-uncached").await, Err(LookupUnavailable)));
    let denial = check_api_key(&*v, Some("valid-but-uncached"), "demo.echo").await.unwrap_err();
    assert!(matches!(denial, KeyDenial::Unavailable), "a shed is not a key verdict");
    assert!(matches!(denial.status(), Status::Unavailable));
    assert_eq!(denial.status().http(), 503);
    assert_eq!(denial.message(), "api key verification unavailable");

    // Release the parked lookups; they all complete Ok (the shed was per-request).
    drop(held_gate);
    for task in tasks {
        assert!(task.await.unwrap().unwrap().is_some());
    }
}

#[tokio::test]
async fn check_api_key_maps_lookup_outcomes() {
    let v = RealKeyVerifier::with_ttl(ScriptedKeys::new(vec![Ok(None)]), Duration::from_secs(60));

    // (b) A definitively unknown key stays Invalid → 401.
    let denial = check_api_key(&v, Some("nope"), "demo.echo").await.unwrap_err();
    assert!(matches!(denial, KeyDenial::Invalid));
    assert_eq!(denial.status().http(), 401);

    // (c) An oversize key is definitively NOT a key: Invalid → 401, not a 503.
    let denial = check_api_key(&v, Some(&"x".repeat(257)), "demo.echo").await.unwrap_err();
    assert!(matches!(denial, KeyDenial::Invalid));
    assert_eq!(denial.status().http(), 401);
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
        _retry_mode: RetryMode,
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
        retry_mode: RetryMode::Never,
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

// BLAST RADIUS (Step 7, 2026-07-11 remediation plan): `From<edge::Error> for
// opsapi::Error` is the single conversion behind EVERY generated rpc client and
// this Remote dispatch. With `edge::Error::UnknownMethod → Status::NotFound`, a
// gateway→svc method mismatch (version skew, misdeploy) now surfaces to the front
// as a 404 that is INDISTINGUISHABLE from a domain not-found. That aliasing is
// intentional (unknown-method is non-retryable; a 503 would invite pointless
// retries) — this test pins the contract over a REAL loopback edge hop.
#[tokio::test]
async fn remote_dispatch_to_unserved_method_surfaces_as_not_found() {
    let ca = edge::DevCA::generate().unwrap();
    // A live peer whose dispatch table does NOT serve the op's method.
    let running = edge::Server::new()
        .listen("127.0.0.1:0".parse().unwrap(), &ca)
        .unwrap();
    let client = edge::Client::dial(running.local_addr(), &ca).await.unwrap();

    let backend = RemoteBackend::new(Arc::new(client));
    let op = Operation {
        method: "characters.create".into(),
        verb: "POST".into(),
        path: "/characters".into(),
        auth: AuthReq::Player,
        success: 201,
        retry_mode: RetryMode::Never,
    };
    let err = backend
        .invoke(&op, Identity::player("bob"), b"{}".to_vec())
        .await
        .unwrap_err();
    assert_eq!(err.status, Status::NotFound, "{err:?}");
    assert_eq!(err.status.http(), 404);

    running.close();
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
        retry_mode: RetryMode::Never,
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
    async fn call(
        &self,
        _m: &str,
        _i: Option<&str>,
        _p: &[u8],
        _retry_mode: RetryMode,
    ) -> Result<Vec<u8>, Error> {
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
        retry_mode: RetryMode::Never,
    };

    // Seed the cache as if a previous request had dialed this provider.
    let failing = Arc::new(FailingCaller::default());
    table
        .remotes
        .lock()
        .unwrap()
        .insert("fakeprov".into(), failing.clone() as Arc<dyn Caller>);

    // First request: the cached caller fails → the error propagates AND the
    // dead entry is evicted.
    let err = table.dispatch(&op, Identity::none(), b"{}".to_vec()).await.unwrap_err();
    assert_eq!(err.status, Status::Unavailable);
    assert_eq!(failing.calls.load(Ordering::SeqCst), 1);
    assert!(
        !table.remotes.lock().unwrap().contains_key("fakeprov"),
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
    table.remotes.lock().unwrap().insert("fakeprov".into(), healthy as Arc<dyn Caller>);
    let resp = table.dispatch(&op, Identity::none(), b"{}".to_vec()).await.unwrap();
    assert_eq!(resp, br#"{"status":"Ok","relayed":true}"#);
    assert!(
        table.remotes.lock().unwrap().contains_key("fakeprov"),
        "a successful call must keep its caller cached"
    );
}

// ---- per-provider dial singleflight: no lock held across the dial await ----

/// A minimal Remote op for `provider` (no local invoker contributed → Remote).
fn remote_op(provider: &str) -> Operation {
    Operation {
        method: format!("{provider}.op"),
        verb: "POST".into(),
        path: format!("/{provider}"),
        auth: AuthReq::None,
        success: 200,
        retry_mode: RetryMode::Never,
    }
}

/// Finding 1a (round 4): a provider whose dial HANGS (peer addr bound but silent)
/// must not block requests to a healthy provider. Before the flight rework one
/// `tokio::sync::Mutex` was held across `edge::Client::dial` in `remote_caller`,
/// so the hung dial serialised EVERY other provider's first cache lookup behind
/// it; now the cache is a sync mutex and only the hung provider's own flight is
/// held across the dial.
#[tokio::test]
async fn hung_dial_to_one_provider_does_not_block_another() {
    // A bound-but-silent UDP socket: the QUIC dial to it hangs until the edge
    // client's DIAL_DEADLINE (5s) — far longer than the healthy call's budget.
    let silent = std::net::UdpSocket::bind("127.0.0.1:0").expect("bind silent socket");
    let silent_addr = silent.local_addr().unwrap();

    let slots = Slots::new();
    slots.contribute(
        opsapi::PEER_SLOT,
        opsapi::PeerAddr { provider: "slowprov".into(), addrs: vec![silent_addr.to_string()] },
    );
    let table = Arc::new(RouteTable::build(&slots).expect("peer-only slots build"));

    // The healthy provider is already cached (as after a successful earlier dial).
    let healthy = Arc::new(FakeCaller { seen: std::sync::Mutex::new(None) });
    table.remotes.lock().unwrap().insert("fastprov".into(), healthy as Arc<dyn Caller>);

    // Start the doomed dispatch and give it time to be inside the hung dial.
    let slow_table = table.clone();
    let slow = tokio::spawn(async move {
        slow_table.dispatch(&remote_op("slowprov"), Identity::none(), b"{}".to_vec()).await
    });
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(!slow.is_finished(), "the silent peer's dial must still be in flight");

    // The healthy provider's call must complete immediately — well inside the
    // 5s the hung dial still has to run.
    let resp = tokio::time::timeout(
        Duration::from_secs(1),
        table.dispatch(&remote_op("fastprov"), Identity::none(), b"{}".to_vec()),
    )
    .await
    .expect("healthy provider must not be blocked by another provider's hung dial")
    .expect("cached healthy caller must serve the call");
    assert_eq!(resp, br#"{"status":"Ok","relayed":true}"#);

    // The hung dial eventually fails on its own deadline (Step 1) — bounded, not
    // leaked. Not awaited here to keep the test fast; drop cancels it.
    slow.abort();
}

/// Duplicate-dial suppression: a second request to the SAME uncached provider
/// waits on that provider's flight and reuses the winner's client instead of
/// dialing again. The first dialer is simulated by holding the provider's flight
/// (exactly what `remote_caller` holds across its dial): the second dispatch must
/// park on it — had it proceeded to dial it would have failed loudly with "no
/// peer contributed" — and, once the winner publishes its client and releases the
/// flight, complete over the cached client without any dial of its own.
#[tokio::test]
async fn concurrent_requests_to_same_provider_share_one_dial() {
    let table = Arc::new(RouteTable::build(&Slots::new()).expect("empty slots build"));

    // First dialer: acquire the provider's flight as remote_caller would.
    let flight = table.flight("flightprov");
    let winner_guard = flight.clone().lock_owned().await;

    // Second caller arrives while the dial is in flight.
    let waiter_table = table.clone();
    let waiter = tokio::spawn(async move {
        waiter_table.dispatch(&remote_op("flightprov"), Identity::none(), b"{}".to_vec()).await
    });
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(!waiter.is_finished(), "second caller must wait on the provider's flight");

    // Winner finishes its dial: publish the client, release the flight.
    let healthy = Arc::new(FakeCaller { seen: std::sync::Mutex::new(None) });
    table.remotes.lock().unwrap().insert("flightprov".into(), healthy as Arc<dyn Caller>);
    drop(winner_guard);

    // The waiter re-checks the cache and reuses the winner's client — success
    // proves it never dialed itself (no peer is contributed for flightprov, so
    // its own dial path would have errored "no peer contributed").
    let resp = waiter.await.unwrap().expect("waiter must reuse the winner's client");
    assert_eq!(resp, br#"{"status":"Ok","relayed":true}"#);

    // The finished flight self-GCs: the next flight() call purges dead weaks, so
    // only the entry it resolves survives.
    drop(flight);
    let _other = table.flight("otherprov");
    let flights = table.flights.lock().unwrap();
    assert!(!flights.contains_key("flightprov"), "dead flight must be purged");
    assert_eq!(flights.len(), 1);
}

// ---- bounded credential admission (Step 10): one deadline over key + session ----

/// The message + envelope a fired admission budget produces (the EXISTING
/// Unavailable class on both fronts — no new status mapping).
const ADMISSION_TIMEOUT_MSG: &str =
    "credential admission timed out (CREDENTIAL_ADMISSION_TIMEOUT_MS)";

/// Well above any test budget (100ms) but far below the test-hang ceiling:
/// admission outcomes must land within this or the deadline is not working.
const BOUNDED: Duration = Duration::from_secs(2);

/// A [`KeyVerifier`] whose lookup NEVER resolves — the direct stand-in for a hung
/// apikeys backend (the edge client bounds only the dial, not the RPC round-trip).
struct HungKeyVerifier;

#[async_trait::async_trait]
impl KeyVerifier for HungKeyVerifier {
    async fn lookup(&self, _key: &str) -> Result<Option<KeyRecord>, LookupUnavailable> {
        std::future::pending().await
    }
}

/// A [`SessionVerifier`] whose verify NEVER resolves — the hung-accounts stand-in.
struct HungSessionVerifier;

#[async_trait::async_trait]
impl SessionVerifier for HungSessionVerifier {
    async fn verify(&self, _token: &str) -> Result<Option<String>, VerifyUnavailable> {
        std::future::pending().await
    }
}

/// An `apikeysapi::Keys` capability whose lookups never resolve — drives the REAL
/// key verifier (flight lock + global permits) into the hung-backend shape.
struct HangingKeys;

#[async_trait::async_trait]
impl apikeysapi::Keys for HangingKeys {
    async fn lookup_key(&self, _key: String) -> Result<Option<KeyRecord>, Error> {
        std::future::pending().await
    }
}

/// An `apikeysapi::Keys` that hangs on its FIRST call and answers every later one —
/// the heal-after-outage fixture for the recovery proof.
struct HangOnceKeys {
    calls: AtomicUsize,
}

#[async_trait::async_trait]
impl apikeysapi::Keys for HangOnceKeys {
    async fn lookup_key(&self, key: String) -> Result<Option<KeyRecord>, Error> {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            std::future::pending::<()>().await;
        }
        Ok(Some(KeyRecord { name: key, policy: "full".to_string() }))
    }
}

/// A `FrontDoor` over the demo op with injectable verifiers and an explicit
/// admission budget — the construction seam for every hung-backend proof.
fn admission_front_door(
    keys: Arc<dyn KeyVerifier>,
    verifier: Arc<dyn SessionVerifier>,
    budget: Duration,
) -> Arc<FrontDoor> {
    let slots = Arc::new(Slots::new());
    let op = demo_opset();
    slots.contribute(opsapi::SLOT, op.operation);
    slots.contribute(opsapi::BINDING_SLOT, op.binding);
    slots.contribute(opsapi::LOCAL_SLOT, op.local);
    Arc::new(FrontDoor::new(slots, verifier, keys, Vec::new()).with_admission_budget(budget))
}

fn demo_http_request() -> HttpRequest<Body> {
    HttpRequest::builder()
        .method("POST")
        .uri("/demo/42")
        .header(header::AUTHORIZATION, "Bearer dev-alice")
        .header("X-Api-Key", TEST_KEY)
        .body(Body::from("1"))
        .unwrap()
}

#[tokio::test]
async fn hung_key_verifier_http_front_is_503_within_budget() {
    let front = admission_front_door(
        Arc::new(HungKeyVerifier),
        Arc::new(DevSessionVerifier::new()),
        Duration::from_millis(100),
    );
    let started = std::time::Instant::now();
    let resp = front.router().oneshot(demo_http_request()).await.unwrap();
    let (status, body) = body_string(resp).await;
    assert!(started.elapsed() < BOUNDED, "admission must be bounded by the budget");
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body, ADMISSION_TIMEOUT_MSG);
}

#[tokio::test]
async fn hung_key_verifier_player_front_is_unavailable_within_budget() {
    let front = admission_front_door(
        Arc::new(HungKeyVerifier),
        Arc::new(DevSessionVerifier::new()),
        Duration::from_millis(100),
    );
    let started = std::time::Instant::now();
    let body = call_player(&front, "demo.echo", Some("dev-alice"), Some(TEST_KEY), b"{}").await;
    assert!(started.elapsed() < BOUNDED, "admission must be bounded by the budget");
    assert_eq!(
        body,
        format!(r#"{{"status":"Unavailable","err":"{ADMISSION_TIMEOUT_MSG}"}}"#)
    );
}

#[tokio::test]
async fn hung_session_verifier_http_front_is_503_within_budget() {
    // The key check answers fast (a full-policy fake); the SESSION verify hangs —
    // the ONE budget must cover the second await too.
    let front = admission_front_door(
        demo_keys(),
        Arc::new(HungSessionVerifier),
        Duration::from_millis(100),
    );
    let started = std::time::Instant::now();
    let resp = front.router().oneshot(demo_http_request()).await.unwrap();
    let (status, body) = body_string(resp).await;
    assert!(started.elapsed() < BOUNDED, "admission must be bounded by the budget");
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body, ADMISSION_TIMEOUT_MSG);
}

#[tokio::test]
async fn hung_session_verifier_player_front_is_unavailable_within_budget() {
    let front = admission_front_door(
        demo_keys(),
        Arc::new(HungSessionVerifier),
        Duration::from_millis(100),
    );
    let started = std::time::Instant::now();
    let body = call_player(&front, "demo.echo", Some("dev-alice"), Some(TEST_KEY), b"{}").await;
    assert!(started.elapsed() < BOUNDED, "admission must be bounded by the budget");
    assert_eq!(
        body,
        format!(r#"{{"status":"Unavailable","err":"{ADMISSION_TIMEOUT_MSG}"}}"#)
    );
}

/// Two concurrent requests for the SAME key against a hung backend, through the REAL
/// key verifier: the first holds the per-key flight lock inside the hung lookup, the
/// second queues on that flight — and BOTH must resolve within ~one budget (it is
/// admissible for both to time out concurrently; what is banned is the second
/// serially waiting 2x behind the first's dropped flight).
///
/// PAUSED CLOCK: everything here is in-process (tower `oneshot`, a pending-future
/// backend, `tokio::time`-based admission timeouts), so virtual time makes the
/// parallel-vs-serial distinction exact — parallel resolves at virtual ~100ms,
/// a serial accumulation at virtual ~200ms — with zero real-clock race.
#[tokio::test(start_paused = true)]
async fn flight_lock_second_caller_is_bounded_too() {
    let real = Arc::new(RealKeyVerifier::new(Arc::new(HangingKeys)));
    let front = admission_front_door(
        real,
        Arc::new(DevSessionVerifier::new()),
        Duration::from_millis(100),
    );
    let started = tokio::time::Instant::now();
    let (a, b) = tokio::join!(
        front.router().oneshot(demo_http_request()),
        front.router().oneshot(demo_http_request()),
    );
    let elapsed = started.elapsed();
    let (status_a, body_a) = body_string(a.unwrap()).await;
    let (status_b, body_b) = body_string(b.unwrap()).await;
    // Virtual time: both callers share ONE 100ms budget window (a serial wait
    // behind the first's flight would read ~200ms). 150ms splits the two cases
    // deterministically — the paused clock advances only when tasks are idle,
    // so machine load cannot move this measurement.
    assert!(
        elapsed < Duration::from_millis(150),
        "the second same-key caller must resolve within the FIRST caller's budget \
         window, never serially behind it (virtual elapsed: {elapsed:?})"
    );
    assert_eq!(status_a, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(status_b, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body_a, ADMISSION_TIMEOUT_MSG);
    assert_eq!(body_b, ADMISSION_TIMEOUT_MSG);
}

/// RECOVERY: after a timed-out admission for key K the backend heals (hangs once,
/// answers after) — the NEXT request for K must verify OK. What this pins is the
/// end-to-end behavior: no persistent 503 for K after a timed-out admission. The
/// MECHANISM (the dropped future releases the `lock_owned` flight guard and its
/// `Weak` table entry dies) is established by code review of `keys.rs`' drop-safety,
/// not asserted directly here — no test seam is built into `RealKeyVerifier`'s
/// flight table for that.
#[tokio::test]
async fn healed_backend_serves_same_key_after_admission_timeout() {
    let real = Arc::new(RealKeyVerifier::new(Arc::new(HangOnceKeys {
        calls: AtomicUsize::new(0),
    })));
    let front = admission_front_door(
        real,
        Arc::new(DevSessionVerifier::new()),
        Duration::from_millis(100),
    );

    // First request: the backend hangs → bounded 503.
    let resp = front.router().oneshot(demo_http_request()).await.unwrap();
    let (status, body) = body_string(resp).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body, ADMISSION_TIMEOUT_MSG);

    // Second request, SAME key: the healed backend answers → full dispatch.
    let resp = front.router().oneshot(demo_http_request()).await.unwrap();
    let (status, body) = body_string(resp).await;
    assert_eq!(status, StatusCode::OK, "no persistent 503 after the backend heals: {body}");
    assert!(body.contains(r#""pid":"alice""#), "{body}");
}

/// Happy path under a generous budget: a valid key + token dispatches exactly as
/// before the admission seam existed — on both fronts.
#[tokio::test]
async fn generous_budget_leaves_happy_path_unaffected() {
    let front = admission_front_door(
        demo_keys(),
        Arc::new(DevSessionVerifier::new()),
        Duration::from_secs(30),
    );
    let resp = front.router().oneshot(demo_http_request()).await.unwrap();
    let (status, body) = body_string(resp).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains(r#""pid":"alice""#), "{body}");

    let body =
        call_player(&front, "demo.echo", Some("dev-alice"), Some(TEST_KEY), br#"{"n":1}"#).await;
    assert!(body.contains(r#""status":"Ok""#), "{body}");
    assert!(body.contains(r#""pid":"alice""#), "{body}");
}
