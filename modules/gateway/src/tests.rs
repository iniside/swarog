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
// `conformance` module (the harness probes it through the real `verify_bearer`); the
// tests re-import it from there.
use super::conformance::UnavailableVerifier;

/// The HTTP status a bearer denial renders as — the mapping the front door applies to
/// every plane's denial.
fn denial_status(denial: &AdmissionDenial) -> StatusCode {
    admission_denial_response(denial).status()
}

#[tokio::test]
async fn verify_bearer_paths() {
    let v = DevSessionVerifier::new();

    // Valid bearer → identity threaded.
    let id = match verify_bearer(&v, Some("dev-alice"), AuthReq::Player).await {
        Ok(id) => id,
        Err(denial) => panic!("expected an identity, got {}", denial.message()),
    };
    assert_eq!(id.player_id(), Some("alice"));

    // Missing bearer → 401.
    let denial = verify_bearer(&v, None, AuthReq::Player).await.expect_err("denied");
    assert_eq!(denial_status(&denial), StatusCode::UNAUTHORIZED);

    // Invalid token → 401.
    let denial = verify_bearer(&v, Some("nope"), AuthReq::Player).await.expect_err("denied");
    assert_eq!(denial_status(&denial), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn verify_bearer_verifier_outage_is_503_not_401() {
    // A verifier that cannot reach accounts must surface as 503 SERVICE_UNAVAILABLE —
    // NOT 401 (which would mass-log-out players the moment accounts blips). The token
    // is well-formed; only the dependency is down.
    let denial = verify_bearer(&UnavailableVerifier, Some("dev-alice"), AuthReq::Player)
        .await
        .expect_err("an unavailable verifier denies");
    assert_eq!(denial_status(&denial), StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn verify_bearer_overlong_token_is_401_without_capability_dispatch() {
    let sessions = Arc::new(CountingSessions::default());
    let verifier = SessionsVerifier::new(sessions.clone());
    let token = "x".repeat(accountsapi::MAX_SESSION_TOKEN_BYTES + 1);

    let denial = verify_bearer(&verifier, Some(&token), AuthReq::Player)
        .await
        .expect_err("an over-long token is denied");
    assert_eq!(denial_status(&denial), StatusCode::UNAUTHORIZED);
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

/// C2: a provider that resolved to TWO instances carries the WHOLE set into the route
/// table's `peers` map — the property the old `.into_iter().next()` collapse (and, one
/// layer up, gateway-svc's `exactly_one`) destroyed. `remote_caller` then builds a
/// `remote::Pool` over this set so HTTP-dispatched Remote ops round-robin across both.
/// (The pool's distribution across `[A,B]` is proven in `core/remote`'s
/// `pool_distributes_round_robin_across_two_instances`; the wire-level spread through the
/// gateway is the C4 splitproof assertion — an in-process test cannot fake the two edge
/// servers a real Pool dials.)
#[test]
fn build_carries_the_full_instance_set_for_a_multi_instance_provider() {
    let slots = Slots::new();
    slots.contribute(
        opsapi::PEER_SLOT,
        opsapi::PeerAddr {
            provider: "characters".into(),
            addrs: vec!["127.0.0.1:9000".into(), "127.0.0.1:9100".into()],
        },
    );
    let table = RouteTable::build(&slots).expect("multi-instance peer set builds");
    assert_eq!(
        table.peers.get("characters"),
        Some(&vec!["127.0.0.1:9000".to_string(), "127.0.0.1:9100".to_string()]),
        "the whole instance set must reach `peers` (not a collapsed first element)"
    );
}

/// C2: a single-instance provider is a one-element set — a pool-of-1, byte-identical to
/// the monolith/standalone path. `remote_caller` builds a caller (a `remote::Pool`) and
/// caches it without dialing (construction is synchronous).
#[tokio::test]
async fn remote_caller_builds_and_caches_a_pool_for_a_single_instance_provider() {
    let slots = Slots::new();
    slots.contribute(
        opsapi::PEER_SLOT,
        opsapi::PeerAddr { provider: "characters".into(), addrs: vec!["127.0.0.1:9000".into()] },
    );
    let table = RouteTable::build(&slots).expect("single-instance peer set builds");
    // Construction is sync (no dial), so this resolves promptly and caches the pool.
    let caller = table.remote_caller("characters").await.expect("a pool is built for a wired peer");
    assert!(
        table.cached_remote("characters").is_some(),
        "the built pool must be cached for reuse across requests"
    );
    // A second call reuses the SAME cached pool (Arc identity), never rebuilding.
    let again = table.remote_caller("characters").await.unwrap();
    assert!(Arc::ptr_eq(&caller, &again), "a cached pool must be reused, not rebuilt");
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

// ---- C2 (F1): a self-healing pool survives a per-instance failure, NOT evicted ----

/// Simulates a self-healing `remote::Pool`: the FIRST call lands on a dead instance and
/// fails (`Unavailable`), but every later call is served by a healthy instance (the pool's
/// internal skip-dead engaging once its probe marks the corpse non-selectable). Counts
/// calls so the test can prove the SAME cached caller kept serving (never rebuilt).
struct SelfHealingPoolFake {
    calls: AtomicUsize,
}

#[async_trait::async_trait]
impl Caller for SelfHealingPoolFake {
    async fn call(
        &self,
        _m: &str,
        _i: Option<&str>,
        _p: &[u8],
        _retry_mode: RetryMode,
    ) -> Result<Vec<u8>, Error> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        if n == 0 {
            // First request hits the dead instance (cursor start, cold-start optimism).
            Err(Error::unavailable("fake pool: instance A down"))
        } else {
            // Skip-dead has engaged: a healthy instance serves.
            Ok(br#"{"status":"Ok","relayed":true}"#.to_vec())
        }
    }
}

/// F1: the route table must keep a self-healing pool PERMANENT across a per-instance
/// failure — NOT evict it. Evicting the whole pool would abort the probe that marks the
/// dead instance non-selectable AND reset the round-robin cursor to 0, re-picking the dead
/// first instance every request → 100% failure to a provider one of whose N instances is
/// down, the exact "part of the traffic nowhere" C2 removes. This pins: (a) after the
/// first-instance failure the provider still serves (not 100%-down), and (b) the cached
/// `Arc` is STABLE across the failure (`Arc::ptr_eq` — the pool is never evicted/rebuilt).
#[tokio::test]
async fn remote_dispatch_keeps_self_healing_pool_across_a_per_instance_failure() {
    let table = RouteTable::build(&Slots::new()).expect("empty slots build");
    let op = Operation {
        method: "fakeprov.op".into(),
        verb: "POST".into(),
        path: "/fake".into(),
        auth: AuthReq::None,
        success: 200,
        retry_mode: RetryMode::Never,
    };

    // Seed the cache with a self-healing pool stand-in (as `remote_caller` would build a
    // real `remote::Pool`).
    let pool = Arc::new(SelfHealingPoolFake { calls: AtomicUsize::new(0) });
    table.remotes.lock().unwrap().insert("fakeprov".into(), pool.clone() as Arc<dyn Caller>);
    let before = table.cached_remote("fakeprov").expect("pool cached");

    // Request 1: the dead instance fails (Unavailable, non-definitive) — the OLD
    // evict-on-error would have torn the pool down here.
    let err = table.dispatch(&op, Identity::none(), b"{}".to_vec()).await.unwrap_err();
    assert_eq!(err.status, Status::Unavailable);

    // (b) The pool is NOT evicted — same Arc still cached.
    let after_fail = table.cached_remote("fakeprov").expect("pool must survive the failure");
    assert!(
        Arc::ptr_eq(&before, &after_fail),
        "a self-healing pool must NOT be evicted on a per-instance/transient error"
    );

    // (a) The provider still serves: the next request routes to a healthy instance (the
    // pool's skip-dead), over the SAME permanent pool.
    let resp = table.dispatch(&op, Identity::none(), b"{}".to_vec()).await.unwrap();
    assert_eq!(resp, br#"{"status":"Ok","relayed":true}"#);
    let after_ok = table.cached_remote("fakeprov").expect("pool still cached");
    assert!(Arc::ptr_eq(&before, &after_ok), "the same pool served the recovery — never rebuilt");
    assert_eq!(pool.calls.load(Ordering::SeqCst), 2, "both requests went through the one pool");
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

// ---------------------------------------------------------------------------
// D2 routing-as-data: a route table built PURELY from describe manifests
// ---------------------------------------------------------------------------

/// `match.report` as a describe manifest: POST /match/report, public, 202,
/// `#[retry_safe]` (OnceAfterReconnect), three Go-parity body args.
fn manifest_match_report() -> opsapi::OpManifest {
    opsapi::OpManifest {
        method: "match.report".into(),
        verb: "POST".into(),
        path: "/match/report".into(),
        auth: AuthReq::None,
        success: 202,
        retry_mode: RetryMode::OnceAfterReconnect,
        args: vec![
            opsapi::ArgMapping { param: "report_id".into(), wire_key: "ReportId".into(), source: opsapi::ArgSource::Body },
            opsapi::ArgMapping { param: "winner".into(), wire_key: "Winner".into(), source: opsapi::ArgSource::Body },
            opsapi::ArgMapping { param: "loser".into(), wire_key: "Loser".into(), source: opsapi::ArgSource::Body },
        ],
    }
}

/// `characters.delete` as a describe manifest: DELETE /characters/{id}, player-auth,
/// 204, one PATH arg (`id` wildcard → `character_id`).
fn manifest_characters_delete() -> opsapi::OpManifest {
    opsapi::OpManifest {
        method: "characters.delete".into(),
        verb: "DELETE".into(),
        path: "/characters/{id}".into(),
        auth: AuthReq::Player,
        success: 204,
        retry_mode: RetryMode::Never,
        args: vec![opsapi::ArgMapping {
            param: "character_id".into(),
            wire_key: "character_id".into(),
            source: opsapi::ArgSource::Path { wildcard: "id".into() },
        }],
    }
}

/// Assembles a `fetched` map (provider → (addrs, manifest)) from a compact spec.
fn fetched(
    entries: Vec<(&str, Vec<&str>, Vec<opsapi::OpManifest>)>,
) -> HashMap<String, (Vec<String>, opsapi::DescribeManifest)> {
    entries
        .into_iter()
        .map(|(provider, addrs, ops)| {
            (
                provider.to_string(),
                (
                    addrs.into_iter().map(String::from).collect(),
                    opsapi::DescribeManifest { ops },
                ),
            )
        })
        .collect()
}

/// One recorded `RecordingCaller::call` — (method, identity, payload, retry_mode).
type SeenCall = (String, Option<String>, Vec<u8>, RetryMode);

/// A `Caller` that records everything it was handed (incl. `retry_mode`) so a test can
/// prove a describe-built route reaches the transport seam with the right shape.
#[derive(Default)]
struct RecordingCaller {
    seen: std::sync::Mutex<Option<SeenCall>>,
}

#[async_trait::async_trait]
impl Caller for RecordingCaller {
    async fn call(
        &self,
        method: &str,
        identity: Option<&str>,
        payload: &[u8],
        retry_mode: RetryMode,
    ) -> Result<Vec<u8>, Error> {
        *self.seen.lock().unwrap() = Some((
            method.to_string(),
            identity.map(str::to_string),
            payload.to_vec(),
            retry_mode,
        ));
        Ok(br#"{"status":"Ok","relayed":true}"#.to_vec())
    }
}

/// A route built PURELY from describe data (no compile-time `<name>rpc` import) carries the
/// full op shape and reaches the owning peer with the right verb/path/auth/success/**retry_mode**
/// and wire request — the whole point of routing-as-data.
#[tokio::test]
async fn describe_built_route_reaches_the_right_peer_with_full_op_shape() {
    let map = fetched(vec![
        ("match", vec!["127.0.0.1:9006"], vec![manifest_match_report()]),
        ("characters", vec!["127.0.0.1:9000"], vec![manifest_characters_delete()]),
    ]);
    let table = Arc::new(build_describe_table(&map).expect("describe table builds"));

    // Op shape rebuilt from data — including the FAITHFUL retry_mode (D1.5a).
    let (report, _) = table.find("POST", "/match/report").expect("match.report route");
    assert_eq!(report.op.method, "match.report");
    assert_eq!(report.op.success, 202);
    assert_eq!(report.op.auth, AuthReq::None);
    assert_eq!(report.op.retry_mode, RetryMode::OnceAfterReconnect);
    let report_op = report.op.clone();
    let report_decode = report.binding.decode.clone();

    // The path-wildcard op: the `{id}` segment is extracted and the route matched.
    let (del, del_args) = table
        .find("DELETE", "/characters/char-9")
        .expect("characters.delete route matches a concrete path");
    assert_eq!(del.op.method, "characters.delete");
    assert_eq!(del.op.auth, AuthReq::Player);
    assert_eq!(del.op.success, 204);
    assert_eq!(del.op.retry_mode, RetryMode::Never);
    assert_eq!(del_args.get("id").map(String::as_str), Some("char-9"));

    // Reaches the right peer: intercept "match" with a recording caller and dispatch a
    // describe-decoded body. dispatch → Remote (no local invoker) → provider_of("match.report").
    let caller = Arc::new(RecordingCaller::default());
    table.remotes.lock().unwrap().insert("match".into(), caller.clone() as Arc<dyn Caller>);
    let wire = (report_decode)(
        Some(br#"{"Winner":"alice","Loser":"bob","ReportId":"r-1"}"#),
        &PathArgs::new(),
    )
    .expect("body decodes");
    let resp = table
        .dispatch(&report_op, Identity::none(), wire)
        .await
        .expect("describe-built route dispatches to its peer");
    assert_eq!(resp, br#"{"status":"Ok","relayed":true}"#);

    let seen = caller.seen.lock().unwrap().clone().expect("caller was reached");
    assert_eq!(seen.0, "match.report", "the wire method reaches the right peer");
    assert_eq!(seen.3, RetryMode::OnceAfterReconnect, "retry_mode is carried to the transport");
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&seen.2).unwrap(),
        serde_json::json!({"Winner": "alice", "Loser": "bob", "ReportId": "r-1"}),
        "the body args are relayed under their wire keys"
    );
}

/// The collision-`bail!` still fires over DESCRIBE-contributed entries: a manifest declaring
/// the same method id twice is a peer-config/codegen bug that must not resolve to a silent
/// last-write-wins hybrid — `build_from_parts` is the shared authority, so the bail is
/// identical to the slot-built path and re-checked on EVERY re-fetch.
///
/// The duplicate is WITHIN one peer's manifest on purpose: since `build_describe_table`
/// rejects a method whose prefix is not the fetching peer, two DIFFERENT peers can no longer
/// both legitimately reach the collision check with one method id — that shape now trips the
/// provider-prefix guard first, so the collision branch is only reachable same-peer.
#[test]
fn describe_table_bails_on_duplicate_method_in_a_manifest() {
    let clash = |verb: &str, path: &str| opsapi::OpManifest {
        method: "clash.op".into(),
        verb: verb.into(),
        path: path.into(),
        auth: AuthReq::None,
        success: 200,
        retry_mode: RetryMode::Never,
        args: vec![],
    };
    let map = fetched(vec![(
        "clash",
        vec!["127.0.0.1:1"],
        vec![clash("GET", "/a"), clash("GET", "/b")],
    )]);
    let err = build_describe_table(&map)
        .err()
        .expect("a duplicate method in a describe manifest must bail")
        .to_string();
    assert!(err.contains("clash.op"), "the bail must name the colliding method: {err}");
    // BRANCH-UNIQUE text (the collision guard in `build_from_parts`), not just the method id:
    // the provider-prefix guard added in c437153 also names the offending method, so
    // `contains("clash.op")` alone no longer pins WHICH branch fired — a fixture that drifted
    // into tripping the prefix guard first would still be green while the collision branch went
    // uncovered. `duplicate OpBinding` is emitted by `build_from_parts` and by nothing else.
    assert!(
        err.contains("duplicate OpBinding"),
        "the COLLISION branch must be the one that fired, not the provider-prefix guard: {err}"
    );
    assert!(
        !err.contains("may only advertise its own ops"),
        "this fixture must not trip the provider-prefix guard: {err}"
    );
}

/// The periodic-refresh property — the WHOLE point of Option A: a peer DOWN at boot
/// contributes no route, and once it answers `__describe` on a later pass its route is
/// installed WITHOUT restarting the front door. Also pins error-keeps-last: a peer that goes
/// down AGAIN keeps its last-known route rather than losing it on the transient failure.
#[tokio::test]
async fn peer_down_at_boot_is_routed_after_a_refetch() {
    use std::sync::atomic::AtomicBool;

    let slots = Arc::new(Slots::new());
    let front = Arc::new(
        FrontDoor::new(slots, Arc::new(DevSessionVerifier::new()), demo_keys(), Vec::new())
            .into_dynamic_routing(),
    );

    // A fetcher whose reachability the test toggles: down → describe Unavailable.
    let down = Arc::new(AtomicBool::new(true));
    let fetch: DescribeFetcher = {
        let down = down.clone();
        Arc::new(move |_provider: String, _addrs: Vec<String>| {
            let down = down.clone();
            Box::pin(async move {
                if down.load(Ordering::SeqCst) {
                    Err(opsapi::Error::unavailable("peer down"))
                } else {
                    Ok(opsapi::DescribeManifest { ops: vec![manifest_match_report()] })
                }
            })
        })
    };
    let peers = vec![opsapi::PeerAddr { provider: "match".into(), addrs: vec!["127.0.0.1:9006".into()] }];
    let mut router = DescribeRouter::new(front.clone(), peers, fetch);

    // Pass 1 (down): tolerated (not an Err), but the route is absent — fail-closed.
    router.refresh_once().await.expect("a down peer is tolerated at boot");
    assert!(
        front.table().find_by_method("match.report").is_none(),
        "a peer down at boot contributes no route"
    );

    // Peer comes up; pass 2 installs the route — no restart.
    down.store(false, Ordering::SeqCst);
    router.refresh_once().await.expect("second pass builds");
    assert!(
        front.table().find_by_method("match.report").is_some(),
        "once the peer answers describe, the re-fetch installs its route"
    );

    // Peer flaps down again; pass 3 KEEPS the last-known route (error-keeps-last).
    down.store(true, Ordering::SeqCst);
    router.refresh_once().await.expect("a later failure is tolerated");
    assert!(
        front.table().find_by_method("match.report").is_some(),
        "a transient describe failure must keep the peer's last-known route, not drop it"
    );
}

/// Change-detection: an unchanged describe pass must NOT rebuild/swap the table, so the
/// installed table (and its permanent per-provider dispatch pools + round-robin cursors, the
/// C2 no-evict invariant) survives the 5s cadence untouched in steady state. Only an actual
/// describe change rebuilds.
#[tokio::test]
async fn unchanged_describe_pass_preserves_the_installed_table() {
    let slots = Arc::new(Slots::new());
    let front = Arc::new(
        FrontDoor::new(slots, Arc::new(DevSessionVerifier::new()), demo_keys(), Vec::new())
            .into_dynamic_routing(),
    );
    // An always-up fetcher returning the SAME manifest every pass.
    let fetch: DescribeFetcher = Arc::new(|_provider: String, _addrs: Vec<String>| {
        Box::pin(async { Ok(opsapi::DescribeManifest { ops: vec![manifest_match_report()] }) })
    });
    let peers = vec![opsapi::PeerAddr { provider: "match".into(), addrs: vec!["127.0.0.1:9006".into()] }];
    let mut router = DescribeRouter::new(front.clone(), peers, fetch);

    router.refresh_once().await.expect("first build");
    let t1 = front.table();
    assert!(t1.find_by_method("match.report").is_some(), "first pass installs the route");

    // A second, identical pass: no change → the SAME table Arc must still be installed.
    router.refresh_once().await.expect("unchanged pass");
    let t2 = front.table();
    assert!(
        Arc::ptr_eq(&t1, &t2),
        "an unchanged describe pass must not rebuild/swap the table (pools stay permanent)"
    );
}

/// C2 no-evict at the refresh boundary: when ONE provider's describe changes, an UNCHANGED
/// provider must keep its EXACT warm dispatch pool (same `Arc` → same instances + round-robin
/// cursor) across the swap. Without the provider-granular carry, `install_table` would drop
/// every provider's pool whenever any peer changed — the eviction the primary path (a peer
/// appearing at t+5s) would trigger against every already-warm peer.
#[tokio::test]
async fn unchanged_provider_keeps_its_warm_pool_across_another_providers_change() {
    use std::sync::atomic::AtomicBool;

    fn manifest_op(provider: &str) -> opsapi::OpManifest {
        opsapi::OpManifest {
            method: format!("{provider}.op"),
            verb: "POST".into(),
            path: format!("/{provider}"),
            auth: AuthReq::None,
            success: 200,
            retry_mode: RetryMode::Never,
            args: vec![],
        }
    }

    let slots = Arc::new(Slots::new());
    let front = Arc::new(
        FrontDoor::new(slots, Arc::new(DevSessionVerifier::new()), demo_keys(), Vec::new())
            .into_dynamic_routing(),
    );

    // charX is always up; charY is DOWN on pass 1, UP (a change) on pass 2.
    let y_up = Arc::new(AtomicBool::new(false));
    let fetch: DescribeFetcher = {
        let y_up = y_up.clone();
        Arc::new(move |provider: String, _addrs: Vec<String>| {
            let y_up = y_up.clone();
            Box::pin(async move {
                match provider.as_str() {
                    "charX" => Ok(opsapi::DescribeManifest { ops: vec![manifest_op("charX")] }),
                    "charY" if y_up.load(Ordering::SeqCst) => {
                        Ok(opsapi::DescribeManifest { ops: vec![manifest_op("charY")] })
                    }
                    _ => Err(opsapi::Error::unavailable("peer down")),
                }
            })
        })
    };
    let peers = vec![
        opsapi::PeerAddr { provider: "charX".into(), addrs: vec!["127.0.0.1:1".into()] },
        opsapi::PeerAddr { provider: "charY".into(), addrs: vec!["127.0.0.1:2".into()] },
    ];
    let mut router = DescribeRouter::new(front.clone(), peers, fetch);

    // Pass 1: only charX is routed. Warm charX's dispatch pool with a distinguishable caller
    // (as a first dispatch to charX would have cached).
    router.refresh_once().await.expect("first pass");
    let installed = front.table();
    let x_pool: Arc<dyn Caller> = Arc::new(RecordingCaller::default());
    installed.remotes.lock().unwrap().insert("charX".into(), x_pool.clone());
    assert!(installed.find_by_method("charY.op").is_none(), "charY down → no route yet");

    // Pass 2: charY comes up (a change) → the table rebuilds and swaps.
    y_up.store(true, Ordering::SeqCst);
    router.refresh_once().await.expect("second pass rebuilds on charY's change");
    let after = front.table();

    // charX was UNCHANGED → its EXACT warm pool survives the swap (same Arc, cursor intact).
    let x_after = after.cached_remote("charX").expect("charX's pool must be carried across");
    assert!(
        Arc::ptr_eq(&x_pool, &x_after),
        "an unchanged provider keeps its exact warm pool/cursor across another provider's change"
    );
    // The change actually took effect: charY is now routed.
    assert!(after.find_by_method("charY.op").is_some(), "charY's route is installed by the re-fetch");
    // charY (newly-changed) has no carried pool — it dials lazily on its next request.
    assert!(after.cached_remote("charY").is_none(), "a changed provider re-dials lazily");
}

/// A `TableCell::Slots` front door ignores `install_table` — the monolith/standalone table is
/// immutable once built, so a stray dynamic swap cannot corrupt it.
#[test]
fn install_table_is_a_noop_on_a_slots_front_door() {
    let slots = Arc::new(Slots::new());
    let op = demo_opset();
    slots.contribute(opsapi::SLOT, op.operation);
    slots.contribute(opsapi::BINDING_SLOT, op.binding);
    slots.contribute(opsapi::LOCAL_SLOT, op.local);
    let front = front_door_with_keys(slots, demo_keys());

    // A swap attempt with an EMPTY table must not take effect on the slots-mode front door.
    front.install_table(Arc::new(
        RouteTable::build_from_parts(Vec::new(), Vec::new(), Vec::new(), Vec::new()).unwrap(),
    ));
    assert!(
        front.table().find_by_method("demo.echo").is_some(),
        "the slots-built table is immutable; install_table is a no-op here"
    );
}

// ---------------------------------------------------------------------------
// D2 describe routing: task ownership, bounded pass, provider-prefix guard
//
// TOPOLOGY. Everything below runs on the AT-RISK path — the managed/split describe
// gateway (`Gateway::with_describe_routing` → `FrontDoor::into_dynamic_routing`,
// `cmd/gateway-svc/src/lib.rs:56`). The monolith slot path never spawns the refresh
// loop and never mints a dispatch pool, so a monolith-shaped fixture would prove
// nothing here.
//
// WHY UNIT TESTS ARE THE ONLY PROOF for these branches. `tools/splitproof` DOES boot
// gateway-svc in describe-routing mode and pins the happy path ([D4-ROUTE]/[D4-*]),
// but it exercises none of the branches below: its peers are healthy (no per-peer
// timeout, no panicking fetch, no foreign-prefix manifest), its fleet is 11 providers
// — one fetch wave, the window never refills — and its ONE cooperative-shutdown
// assertion ([W2], `tools/splitproof/src/main.rs:379-392`) runs against the MONOLITH,
// whose slot path has no refresh task and no dispatch pools to tear down. So the
// lifecycle/teardown, timeout, panic, refill and prefix branches have no harness
// coverage at all; these tests are it.
// ---------------------------------------------------------------------------

/// A dynamic-routing front door — the describe/split topology every test in this section
/// runs on (the shape `Gateway::init` builds when `describe_routing` is set).
fn dynamic_front() -> Arc<FrontDoor> {
    Arc::new(
        FrontDoor::new(
            Arc::new(Slots::new()),
            Arc::new(DevSessionVerifier::new()),
            demo_keys(),
            Vec::new(),
        )
        .into_dynamic_routing(),
    )
}

/// One well-formed manifest op OWNED by `provider`: method `<provider>.<op>` on
/// `POST /<provider>/<op>` (so it satisfies the provider-prefix guard).
fn describe_op(provider: &str, op: &str) -> opsapi::OpManifest {
    describe_op_raw(&format!("{provider}.{op}"), &format!("/{provider}/{op}"))
}

/// A manifest op with an ARBITRARY method id — the seam the provider-prefix fixtures use
/// to advertise a method the fetched peer does not own.
fn describe_op_raw(method: &str, path: &str) -> opsapi::OpManifest {
    opsapi::OpManifest {
        method: method.into(),
        verb: "POST".into(),
        path: path.into(),
        auth: AuthReq::None,
        success: 200,
        retry_mode: RetryMode::Never,
        args: vec![],
    }
}

/// A production `remote::Pool` over a constant address list — the CONCRETE type the route
/// table mints and `RouteTable::pools` holds. Construction dials nothing; the instance set
/// is empty until the first `call`.
fn constant_pool(addrs: &[&str]) -> Arc<remote::Pool> {
    let addrs: Vec<String> = addrs.iter().map(|s| s.to_string()).collect();
    let list: remote::PeerListResolver = Arc::new(move || {
        let addrs = addrs.clone();
        Box::pin(async move { Ok(addrs.clone()) })
    });
    Arc::new(remote::Pool::new(list))
}

/// Drives one `call` on `pool` so its instance SET is resolved (the pool's `refresh` runs
/// inside `call`). The address is deliberately unparseable, so the dial fails synchronously
/// inside the dialer — no socket, no wall-clock dial deadline — while the instance survives.
/// A resolved instance set is what makes "this pool was stopped" OBSERVABLE: `Pool::stop`
/// `mem::take`s the instances, flipping `readyz` back to "no resolved instances yet".
async fn pool_with_resolved_instances() -> Arc<remote::Pool> {
    let pool = constant_pool(&["not-a-socket-addr"]);
    let err = pool
        .call("probe.op", None, b"{}", RetryMode::Never)
        .await
        .expect_err("an unparseable peer address cannot dial");
    let _ = err;
    assert!(
        !pool_readyz_error(&pool).contains("no resolved instances yet"),
        "fixture: the pool must have a resolved instance set before it is handed to a table"
    );
    pool
}

/// `Pool::readyz`'s error text — the public observable this section uses to tell a pool with
/// a live instance set from one whose instances `Pool::stop` has taken.
fn pool_readyz_error(pool: &remote::Pool) -> String {
    pool.readyz().err().unwrap_or_default()
}

/// The number of domain fortresses the managed gateway fetches describe from: every
/// `cmd/<name>-svc` ON DISK except the front door itself (`gateway-svc` hosts the gateway,
/// it is not one of its peers).
///
/// DERIVED, never a literal — the same drift-check discipline `tools/splitproof`'s
/// fleet preflight uses. A hardcoded `11` would keep `DESCRIBE_FETCH_CONCURRENCY >= 11` true
/// forever while a 12th, 17th, 20th svc quietly pushed the boot pass into a second fetch
/// wave: the invariant would break with nothing red. Reading the tree is exact here because
/// `CARGO_MANIFEST_DIR` is this crate's source path, baked in at compile time; if the tree is
/// gone the test FAILS loudly rather than skipping (a green SKIP would restore the same hole).
fn domain_svc_count() -> usize {
    let cmd = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../cmd");
    let entries = std::fs::read_dir(&cmd).unwrap_or_else(|e| {
        panic!(
            "cannot enumerate {} to derive the fleet size: {e}\n\
             (this path is baked in at COMPILE time — a missing dir usually means the test \
             binary was built from a different checkout sharing this CARGO_TARGET_DIR; \
             rebuild it before reading this as a product failure)",
            cmd.display()
        )
    });
    let count = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|name| name.ends_with("-svc") && name != "gateway-svc")
        .count();
    assert!(count > 0, "no cmd/*-svc roots found under {} — the derivation is broken", cmd.display());
    count
}

/// A guard MOVED into the refresh task through the fetcher closure. Its `Drop` runs only
/// when the spawned task's future is dropped — i.e. only when the task has actually ENDED.
/// This is the drop-flag the lifecycle proof rests on: "no further pass after stop" would be
/// satisfied by a still-leaked task that simply is not ticked, and would prove nothing.
struct TaskEnded(Arc<std::sync::atomic::AtomicBool>);

impl Drop for TaskEnded {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// (#1 LIFECYCLE) `Gateway::stop` JOINS the describe-refresh task — the leak 3311381 closed.
///
/// Before that commit `DescribeRouter::spawn` detached the loop with `tokio::spawn`, kept no
/// handle and no stop signal, and `Gateway` had no `stop` at all: the loop (holding the
/// `Arc<FrontDoor>` and its describe-fetcher pools) ran on past module teardown. The
/// assertion is the DROP FLAG, not a pass count: it flips only when the task's future — which
/// owns the `DescribeRouter`, which owns the sole `Arc` of the fetcher closure — is dropped,
/// which happens only when the task ends. A detached-and-leaked task leaves it `false`.
///
/// The paused clock turns the second half into a binary: a task that had to be FORCE-ABORTED
/// burns exactly `DESCRIBE_STOP_GRACE` of (virtual) time first, so `elapsed < grace` proves
/// the loop observed the signal and drained cooperatively. No wall clock is raced.
#[tokio::test(start_paused = true)]
async fn stop_joins_the_describe_refresh_task_instead_of_leaking_it() {
    use std::sync::atomic::AtomicBool;

    let ended = Arc::new(AtomicBool::new(false));
    // The fetcher closure is the ONLY owner of the guard, and the test keeps no clone of the
    // `Arc<dyn Fn>` — so the flag tracks the task's lifetime and nothing else. No peers, so
    // the closure is never called; it exists purely to carry the guard into the task.
    let fetch: DescribeFetcher = {
        let guard = TaskEnded(ended.clone());
        Arc::new(move |_provider: String, _addrs: Vec<String>| {
            let _ = &guard;
            Box::pin(async { Ok(opsapi::DescribeManifest { ops: Vec::new() }) })
        })
    };
    let router = DescribeRouter::new(dynamic_front(), Vec::new(), fetch);
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    let task = router.spawn(stop_rx);
    assert!(
        !ended.load(Ordering::SeqCst),
        "fixture: the refresh task must still be alive before stop"
    );

    // The module owns the task exactly as `Gateway::start` leaves it.
    let gw = Gateway::with_verifier(Arc::new(DevSessionVerifier::new()));
    *gw.stop_tx.lock().unwrap() = Some(stop_tx);
    *gw.task.lock().unwrap() = Some(task);

    let ctx = Context::new();
    let started = tokio::time::Instant::now();
    gw.stop(&ctx).await.expect("stop must not fail");
    let elapsed = started.elapsed();

    assert!(
        ended.load(Ordering::SeqCst),
        "stop must JOIN the refresh task (its future dropped), not leave it detached"
    );
    assert!(
        elapsed < DESCRIBE_STOP_GRACE,
        "the loop must observe the stop signal and drain, not burn the whole grace and get \
         aborted (took {elapsed:?} of the {DESCRIBE_STOP_GRACE:?} grace)"
    );
    // (#4) The ownership cells are emptied, so a second stop is a no-op rather than a panic.
    assert!(gw.task.lock().unwrap().is_none(), "stop takes the join handle");
    assert!(gw.stop_tx.lock().unwrap().is_none(), "stop takes the stop sender");
    gw.stop(&ctx).await.expect("a second stop must be a no-op, not a panic");
}

/// (#1b LIFECYCLE) A stop landing MID-PASS force-ABORTS the refresh task after
/// `DESCRIBE_STOP_GRACE` — and that is the NORMAL production path, not the exception.
///
/// The two landed commits compose into it: the loop observes the stop signal only BETWEEN
/// passes (at `ticker.tick()`), and 41c3344 made a pass bounded at `DESCRIBE_PEER_TIMEOUT`
/// (6s) — three times the 2s grace. So any stop that lands while a peer is slow takes THIS
/// branch (`modules/gateway/src/lib.rs:536-542`), while the cooperative branch its sibling
/// test pins is the lucky case. Untested, a `stop` that dropped the abort (or awaited the
/// join forever) would blow `MODULE_STOP_GRACE_MS` and leave the task detached — the exact
/// leak 3311381 exists to close, restored on the common path.
///
/// Fully deterministic on the paused clock: the peer's fetch never resolves, so once `stop`
/// is awaited the runtime's earliest deadline is the 2s grace (the fetch's own 6s bound is
/// later), and auto-advance fires it. `Arc::strong_count(&front) == 1` afterwards is the
/// proof the task's future — which owns the only other `Arc<FrontDoor>` — is gone; the abort
/// really completed rather than being fired and forgotten.
#[tokio::test(start_paused = true)]
async fn a_stop_landing_mid_pass_force_aborts_the_refresh_task() {
    let front = dynamic_front();
    let pass_started = Arc::new(tokio::sync::Notify::new());
    let fetch: DescribeFetcher = {
        let pass_started = pass_started.clone();
        Arc::new(move |_provider: String, _addrs: Vec<String>| {
            let pass_started = pass_started.clone();
            Box::pin(async move {
                // `notify_one` stores the permit, so the waiter cannot miss it.
                pass_started.notify_one();
                std::future::pending().await
            })
        })
    };
    let peers = vec![opsapi::PeerAddr {
        provider: "stall".into(),
        addrs: vec!["127.0.0.1:1".into()],
    }];
    // No synchronous first pass: the loop's own tick starts the pass this test stops inside.
    let router = DescribeRouter::new(front.clone(), peers, fetch);
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    let task = router.spawn(stop_rx);

    // Happens-before: the pass is provably IN FLIGHT (the fetcher ran and then parked forever).
    tokio::time::timeout(Duration::from_secs(60), pass_started.notified())
        .await
        .expect("the periodic tick must start a pass");

    let gw = Gateway::with_verifier(Arc::new(DevSessionVerifier::new()));
    *gw.stop_tx.lock().unwrap() = Some(stop_tx);
    *gw.task.lock().unwrap() = Some(task);

    let started = tokio::time::Instant::now();
    gw.stop(&Context::new())
        .await
        .expect("a mid-pass stop must still return Ok, not hang or error");
    let elapsed = started.elapsed();

    assert_eq!(
        elapsed, DESCRIBE_STOP_GRACE,
        "a mid-pass stop must wait exactly the grace and then ABORT — not return early \
         (nothing joined) and not run past the module's stop budget"
    );
    assert_eq!(
        Arc::strong_count(&front),
        1,
        "the aborted task's future must be dropped by the time stop returns — an abort that \
         is fired and not awaited leaves the task (and this Arc) alive"
    );
}

/// (#1c LIFECYCLE) The periodic loop's TICK arm actually runs a pass.
///
/// Every other test in this section drives `refresh_once` directly, and the lifecycle tests
/// stop the loop before a tick ever fires — so "the table re-fetches every
/// `DESCRIBE_REFRESH_INTERVAL`", the entire reason `spawn` exists (a peer DOWN at boot is
/// routed once it comes up, with no restart), was asserted nowhere in-process. A mutation
/// turning the `ticker.tick()` arm into a `break` survives the rest of the suite untouched.
///
/// The happens-before is the SECOND pass's fetch: a pass fetches before it installs, so
/// observing pass 2 begin proves pass 1 completed its swap. The paused clock's auto-advance
/// is what moves virtual time to each tick; the 60s guard is a later deadline, so it only
/// fires if no tick ever comes.
#[tokio::test(start_paused = true)]
async fn the_refresh_loop_runs_a_pass_on_every_tick() {
    let front = dynamic_front();
    let passes = Arc::new(AtomicUsize::new(0));
    let second_pass = Arc::new(tokio::sync::Notify::new());
    let fetch: DescribeFetcher = {
        let passes = passes.clone();
        let second_pass = second_pass.clone();
        Arc::new(move |provider: String, _addrs: Vec<String>| {
            let n = passes.fetch_add(1, Ordering::SeqCst) + 1;
            if n == 2 {
                second_pass.notify_one();
            }
            Box::pin(async move {
                Ok(opsapi::DescribeManifest { ops: vec![describe_op(&provider, "op")] })
            })
        })
    };
    let peers = vec![opsapi::PeerAddr {
        provider: "late".into(),
        addrs: vec!["127.0.0.1:1".into()],
    }];
    // Deliberately NO synchronous first pass — every fetch below comes from the tick arm.
    let router = DescribeRouter::new(front.clone(), peers, fetch);
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    let task = router.spawn(stop_rx);
    assert!(
        front.table().find_by_method("late.op").is_none(),
        "fixture: the dynamic table starts empty (fail-closed cold start)"
    );

    tokio::time::timeout(Duration::from_secs(60), second_pass.notified())
        .await
        .expect("the loop must keep ticking, not run once and stop");

    assert!(
        front.table().find_by_method("late.op").is_some(),
        "the tick-driven pass must INSTALL its table — this is how a peer that was down at \
         boot gets routed without a restart"
    );
    assert!(passes.load(Ordering::SeqCst) >= 2, "the loop ticks repeatedly, not once");

    let gw = Gateway::with_verifier(Arc::new(DevSessionVerifier::new()));
    *gw.stop_tx.lock().unwrap() = Some(stop_tx);
    *gw.task.lock().unwrap() = Some(task);
    gw.stop(&Context::new()).await.expect("stop must not fail");
}

/// (#2 LIFECYCLE) `adopt_remote` carries the CONCRETE `Arc<remote::Pool>` (the teardown
/// handle), not only the type-erased `Arc<dyn Caller>` used for dispatch.
///
/// The pre-existing carry-over test injects a fake `Caller` into `remotes` only, so the
/// `pools` half — the entire point of 3311381 — was unexercised: a regression that dropped
/// the `pools` insert from `adopt_remote`/`publish_caller` would leave that test green while
/// every pool surviving a refresh became invisible to `Gateway::stop`.
#[test]
fn adopt_remote_carries_the_concrete_pool_beside_the_erased_caller() {
    let map = fetched(vec![("charX", vec!["127.0.0.1:1"], vec![describe_op("charX", "op")])]);
    let old = build_describe_table(&map).expect("old table builds");
    let new = build_describe_table(&map).expect("new table builds");

    // Mint the pool through the SAME publisher the dispatch path uses (`insert_caller`).
    let pool = constant_pool(&["127.0.0.1:1"]);
    let caller = old.insert_caller("charX", pool.clone());

    assert!(new.adopt_remote("charX", &old), "an unchanged provider is adopted");
    let carried_caller = new.cached_remote("charX").expect("the dispatch entry is carried");
    assert!(
        Arc::ptr_eq(&carried_caller, &caller),
        "the EXACT live caller crosses the refresh (instances + round-robin cursor intact)"
    );
    let carried_pool = new
        .pools
        .lock()
        .unwrap()
        .get("charX")
        .cloned()
        .expect("the TEARDOWN handle must cross the refresh too, or Gateway::stop cannot \
                 reach a pool that survives rebuilds");
    assert!(
        Arc::ptr_eq(&carried_pool, &pool),
        "the carried pools entry must be the SAME pool object, not a re-minted one"
    );

    // A caller with no pool behind it (the unit-test fakes) is carried for dispatch only —
    // the `None` arm of `publish_caller`, which must not fabricate a teardown handle.
    old.publish_caller("charY", None, Arc::new(RecordingCaller::default()));
    assert!(new.adopt_remote("charY", &old), "a non-pool caller is still adopted");
    assert!(
        new.pools.lock().unwrap().get("charY").is_none(),
        "a caller with no pool contributes nothing to tear down"
    );
    // A provider the previous table never dialed is not adopted (it dials lazily instead).
    assert!(!new.adopt_remote("charZ", &old), "an unknown provider is not adopted");
}

/// (#3 LIFECYCLE) `Gateway::stop` stops the pool ADOPTED ACROSS a describe refresh — the
/// carry-over path, which is the one that was silently broken, not the freshly-minted one.
///
/// charX is unchanged across both passes (so its pool is adopted); charY appears on pass 2,
/// which is what forces the rebuild+swap. After `stop`: the installed table's `pools` map is
/// drained AND the adopted pool itself reports an empty instance set — `Pool::stop`'s
/// `mem::take` — which is the observable proof that THIS pool object is the one that was
/// stopped, not merely dropped from a map.
#[tokio::test]
async fn gateway_stop_stops_the_pool_adopted_across_a_describe_refresh() {
    use std::sync::atomic::AtomicBool;

    let front = dynamic_front();
    let y_up = Arc::new(AtomicBool::new(false));
    let fetch: DescribeFetcher = {
        let y_up = y_up.clone();
        Arc::new(move |provider: String, _addrs: Vec<String>| {
            let y_up = y_up.clone();
            Box::pin(async move {
                match provider.as_str() {
                    "charX" => Ok(opsapi::DescribeManifest { ops: vec![describe_op("charX", "op")] }),
                    "charY" if y_up.load(Ordering::SeqCst) => {
                        Ok(opsapi::DescribeManifest { ops: vec![describe_op("charY", "op")] })
                    }
                    _ => Err(opsapi::Error::unavailable("peer down")),
                }
            })
        })
    };
    let peers = vec![
        opsapi::PeerAddr { provider: "charX".into(), addrs: vec!["127.0.0.1:1".into()] },
        opsapi::PeerAddr { provider: "charY".into(), addrs: vec!["127.0.0.1:2".into()] },
    ];
    let mut router = DescribeRouter::new(front.clone(), peers, fetch);

    // Pass 1 installs charX and warms its dispatch pool the way a first request would.
    router.refresh_once().await.expect("first pass builds");
    let pool = pool_with_resolved_instances().await;
    front.table().insert_caller("charX", pool.clone());

    // Pass 2: charY appears → rebuild + swap, charX's pool is ADOPTED across it.
    y_up.store(true, Ordering::SeqCst);
    router.refresh_once().await.expect("second pass rebuilds");
    let after = front.table();
    assert!(after.find_by_method("charY.op").is_some(), "fixture: the swap really happened");
    let adopted = after.pools.lock().unwrap().get("charX").cloned().expect("pool adopted");
    assert!(Arc::ptr_eq(&adopted, &pool), "fixture: the adopted pool is the warmed one");

    let gw = Gateway::with_verifier(Arc::new(DevSessionVerifier::new()));
    let _ = gw.front_door.set(front.clone());
    gw.stop(&Context::new()).await.expect("stop must not fail");

    assert!(
        front.table().pools.lock().unwrap().is_empty(),
        "stop must drain the installed table's teardown handles"
    );
    assert!(
        pool_readyz_error(&pool).contains("no resolved instances yet"),
        "the ADOPTED pool must be the one Pool::stop ran on (instances taken); got {:?}",
        pool_readyz_error(&pool)
    );
}

/// (#4 LIFECYCLE) `stop` on a `Slots` front door that never served a request must not
/// MATERIALIZE the table, and must stay a no-op on a second call.
///
/// Proven by construction: the slots below carry a COLLIDING pair of operations, so
/// `FrontDoor::table()`'s lazy `get_or_init(... .expect(...))` would PANIC if `stop` ever
/// took that path. `stop_pools` reads the cell with `OnceLock::get` instead — the difference
/// between a clean teardown and a panic that unwinds `App::stop` and skips every remaining
/// module's teardown.
#[tokio::test]
async fn stop_never_materializes_an_unserved_slots_table_and_is_idempotent() {
    let slots = Arc::new(Slots::new());
    let first = demo_opset();
    let second = demo_opset();
    slots.contribute(opsapi::SLOT, first.operation);
    slots.contribute(opsapi::BINDING_SLOT, first.binding);
    slots.contribute(opsapi::SLOT, second.operation);
    slots.contribute(opsapi::BINDING_SLOT, second.binding);
    let front = front_door_with_keys(slots, demo_keys());
    // The decoy is real: building this table fails, and `table()` would `expect` on it.
    assert!(front.build_table().is_err(), "fixture: the slot contents must collide");

    let gw = Gateway::with_verifier(Arc::new(DevSessionVerifier::new()));
    let _ = gw.front_door.set(front.clone());
    let ctx = Context::new();
    gw.stop(&ctx).await.expect("stop on an unserved slots front door");
    gw.stop(&ctx).await.expect("a second stop must be a no-op");

    match &front.table {
        TableCell::Slots(cell) => assert!(
            cell.get().is_none(),
            "stop must read the table cell WITHOUT building it (the build here would panic)"
        ),
        TableCell::Dynamic(_) => panic!("fixture: this must be a slots front door"),
    }
}

/// (#5 LIFECYCLE) Dispatch-pool teardown is BOUNDED — it cannot outlive `POOL_STOP_BUDGET`.
///
/// The fixture makes the stops genuinely slow by construction: each pool points at a local
/// UDP socket that is bound but never answers, so the QUIC handshake runs to `edge`'s 5s
/// `DIAL_DEADLINE`, and `Reconnecting::close` — which `Pool::stop` awaits per instance — must
/// wait on the tokio mutex that dial holds. An UNBOUNDED teardown of the three pools runs ~5s
/// (measured); `stop_pools` must return at ~2s. That matters because `App::stop` wraps each
/// module in `MODULE_STOP_GRACE_MS` (5s) and DROPS the future on elapse, and a cancelled
/// `Pool::stop` is strictly worse than none (it has already `mem::take`n its instances, so
/// they sit in the cancelled frame, invisible even to `Drop`'s probe-abort net).
///
/// WHAT THIS TEST CANNOT SEE — the fan-out being CONCURRENT. The bound is a
/// `timeout(POOL_STOP_BUDGET, ..)` placed AROUND the whole drain, so a serial teardown that
/// kept that timeout would also return at ~2s and stay green here. Distinguishing them needs
/// pools whose stop takes a TUNABLE delay just over half the budget (N x delay > budget while
/// concurrent < budget), and that is not constructible from this crate: `remote::Pool`'s
/// factory-injectable constructor (`Pool::with_factory`, `core/remote/src/lib.rs:804`) is
/// private to `remote`, so the only stop delays available here are ~0 (no in-flight dial) or
/// ~`DIAL_DEADLINE` (one), never a middling one. Recorded as a known gap rather than papered
/// over with a name the assertions do not earn.
///
/// ENVIRONMENT: the lower-bound assertion is the anti-vacuity guard — if these loopback peers
/// ever stopped stalling the dial (a sandbox or firewall that fast-fails UDP to 127.0.0.1),
/// this test goes RED on that bound. That is a FIXTURE failure, not a product bug; read the
/// message before touching `stop_pools`.
#[tokio::test]
async fn dispatch_pool_teardown_is_bounded() {
    // Bound, never read: the OS keeps the port open and queues the handshake packets, so the
    // dial gets no response and no ICMP refusal — a stalled peer, entirely on loopback (no
    // external network, so this behaves identically in a sandbox).
    let mut black_holes = Vec::new();
    let mut entries = Vec::new();
    for _ in 0..3 {
        let sock = std::net::UdpSocket::bind(("127.0.0.1", 0)).expect("bind a black-hole peer");
        entries.push(sock.local_addr().expect("black-hole addr").to_string());
        black_holes.push(sock);
    }

    let table = build_describe_table(&fetched(vec![(
        "p0",
        vec!["127.0.0.1:1"],
        vec![describe_op("p0", "op")],
    )]))
    .expect("table builds");

    let mut calls = Vec::new();
    for (i, addr) in entries.iter().enumerate() {
        let pool = constant_pool(&[addr.as_str()]);
        // An in-flight call: it resolves the instance set, then holds the dial mutex for the
        // whole 5s deadline — which is what makes this pool's `stop` slow.
        let dialing = pool.clone();
        calls.push(tokio::spawn(async move {
            let _ = dialing.call("p.op", None, b"{}", RetryMode::Never).await;
        }));
        // Happens-before, not a sleep: wait until the pool has published its instance set.
        let resolved = tokio::time::timeout(Duration::from_secs(5), async {
            while pool_readyz_error(&pool).contains("no resolved instances yet") {
                tokio::task::yield_now().await;
            }
        })
        .await;
        resolved.expect("fixture: the pool must resolve its instance set");
        table.insert_caller(&format!("p{i}"), pool);
    }

    let started = tokio::time::Instant::now();
    table.stop_pools().await;
    let elapsed = started.elapsed();

    assert!(
        elapsed >= POOL_STOP_BUDGET,
        "fixture check: the teardown was not actually stalled ({elapsed:?}), so this test \
         would prove nothing about the bound — the black-hole peers must hang the dial"
    );
    assert!(
        elapsed < POOL_STOP_BUDGET + Duration::from_secs(1),
        "teardown must be bounded by POOL_STOP_BUDGET, not by the probe grace + dial \
         deadline it waits on — took {elapsed:?}"
    );
    assert!(
        table.pools.lock().unwrap().is_empty(),
        "stop_pools drains its handles, so a second call is a no-op"
    );
    for c in calls {
        c.abort();
    }
    drop(black_holes);
}

/// (#6 BOUNDED PASS) A peer that STALLS its describe is timed out into the keep-last branch
/// while every other peer refreshes — the 41c3344 bound.
///
/// Before it, `refresh_once` awaited each peer serially with no timeout of its own
/// (`remote::describe` adds none; the server only gives up at `edge`'s 30s
/// `EDGE_STREAM_GRACE`), so this pass — the same call `Gateway::start` awaits — hung.
/// Virtual clock only: the stalling fetch is `pending()` forever, so the pass ends exactly at
/// `DESCRIBE_PEER_TIMEOUT` of paused-clock time and no wall clock is raced.
#[tokio::test(start_paused = true)]
async fn a_stalled_peer_times_out_into_keep_last_while_the_others_refresh() {
    use std::sync::atomic::AtomicBool;

    let front = dynamic_front();
    let stalling = Arc::new(AtomicBool::new(false));
    let live_grew = Arc::new(AtomicBool::new(false));
    let fetch: DescribeFetcher = {
        let stalling = stalling.clone();
        let live_grew = live_grew.clone();
        Arc::new(move |provider: String, _addrs: Vec<String>| {
            let stalling = stalling.clone();
            let live_grew = live_grew.clone();
            match provider.as_str() {
                "stall" if stalling.load(Ordering::SeqCst) => Box::pin(std::future::pending()),
                "stall" => Box::pin(async {
                    Ok(opsapi::DescribeManifest { ops: vec![describe_op("stall", "op")] })
                }),
                _ => Box::pin(async move {
                    let mut ops = vec![describe_op("live", "op")];
                    if live_grew.load(Ordering::SeqCst) {
                        ops.push(describe_op("live", "extra"));
                    }
                    Ok(opsapi::DescribeManifest { ops })
                }),
            }
        })
    };
    let peers = vec![
        opsapi::PeerAddr { provider: "stall".into(), addrs: vec!["127.0.0.1:1".into()] },
        opsapi::PeerAddr { provider: "live".into(), addrs: vec!["127.0.0.1:2".into()] },
    ];
    let mut router = DescribeRouter::new(front.clone(), peers, fetch);

    router.refresh_once().await.expect("first pass builds");
    let before = front.table();
    assert!(before.find_by_method("stall.op").is_some(), "fixture: the stall peer was routed");

    // Pass 2: `stall` hangs forever, `live` changes (so the pass must still rebuild + swap).
    stalling.store(true, Ordering::SeqCst);
    live_grew.store(true, Ordering::SeqCst);
    let started = tokio::time::Instant::now();
    // The hang guard is a VIRTUAL timer with 10x headroom: with the per-peer bound in place
    // the inner 6s deadline always fires first, and without it this turns an unbounded pass
    // into a clean failure instead of a hung test run.
    tokio::time::timeout(Duration::from_secs(60), router.refresh_once())
        .await
        .expect("the pass must be bounded per peer, not by EDGE_STREAM_GRACE")
        .expect("a stalled peer is keep-last, never a failed pass");
    let elapsed = started.elapsed();

    assert!(
        elapsed >= DESCRIBE_PEER_TIMEOUT && elapsed < DESCRIBE_PEER_TIMEOUT + Duration::from_secs(1),
        "the pass must end at the per-peer bound, not at EDGE_STREAM_GRACE (took {elapsed:?})"
    );
    let after = front.table();
    assert!(
        !Arc::ptr_eq(&before, &after),
        "the live peer's change must still rebuild the table despite the stalled peer"
    );
    assert!(
        after.find_by_method("stall.op").is_some(),
        "a timed-out peer KEEPS its prior manifest — a timeout is not a route eviction"
    );
    assert!(after.find_by_method("live.op").is_some(), "the healthy peer's routes survive");
    assert!(after.find_by_method("live.extra").is_some(), "the healthy peer really refreshed");
}

/// (#7 BOUNDED PASS) A PANICKING fetch task is keep-last for its peer; the pass and the loop
/// survive and every other peer refreshes.
///
/// This is the `JoinSet` task-level `Err` arm added in 41c3344 — a branch that did not exist
/// while the pass was a sequential `for` loop (a panic there unwound the pass, and with it the
/// refresh task). Nothing else in the pass can produce a `JoinError`, so this fixture is the
/// only way to execute it.
#[tokio::test]
async fn a_panicking_describe_fetch_is_keep_last_and_the_pass_survives() {
    use std::sync::atomic::AtomicBool;

    let front = dynamic_front();
    let explode = Arc::new(AtomicBool::new(false));
    let live_grew = Arc::new(AtomicBool::new(false));
    let fetch: DescribeFetcher = {
        let explode = explode.clone();
        let live_grew = live_grew.clone();
        Arc::new(move |provider: String, _addrs: Vec<String>| {
            let explode = explode.clone();
            let live_grew = live_grew.clone();
            Box::pin(async move {
                if provider == "boom" {
                    if explode.load(Ordering::SeqCst) {
                        panic!("gateway test: the injected describe fetcher exploded");
                    }
                    return Ok(opsapi::DescribeManifest { ops: vec![describe_op("boom", "op")] });
                }
                let mut ops = vec![describe_op("live", "op")];
                if live_grew.load(Ordering::SeqCst) {
                    ops.push(describe_op("live", "extra"));
                }
                Ok(opsapi::DescribeManifest { ops })
            })
        })
    };
    let peers = vec![
        opsapi::PeerAddr { provider: "boom".into(), addrs: vec!["127.0.0.1:1".into()] },
        opsapi::PeerAddr { provider: "live".into(), addrs: vec!["127.0.0.1:2".into()] },
    ];
    let mut router = DescribeRouter::new(front.clone(), peers, fetch);

    router.refresh_once().await.expect("first pass builds");
    assert!(front.table().find_by_method("boom.op").is_some(), "fixture: boom was routed");

    explode.store(true, Ordering::SeqCst);
    live_grew.store(true, Ordering::SeqCst);
    router
        .refresh_once()
        .await
        .expect("a panicking fetch task must not fail the pass");

    let after = front.table();
    assert!(
        after.find_by_method("boom.op").is_some(),
        "a panicking peer keeps its last-known routes, it does not drop the table"
    );
    assert!(after.find_by_method("live.extra").is_some(), "the other peers still refresh");

    // And the pass is repeatable — the JoinSet arm did not poison the router.
    explode.store(false, Ordering::SeqCst);
    router.refresh_once().await.expect("the router recovers on the next pass");
}

/// (#8 BOUNDED PASS) The fetch window is CAPPED at `DESCRIBE_FETCH_CONCURRENCY` and REFILLS:
/// with more peers than the window, every peer is still fetched.
///
/// Both halves are proven by construction, not by timing. Every fetch parks on a gate the
/// TEST holds shut, so no fetch can complete and the window cannot refill while the test is
/// looking: the number of fetches that ever STARTED is then exactly the window size. A
/// smaller window never reaches 16 (the first wait fails); an absent/larger window spawns all
/// 21 at once (the equality fails — the whole burst is spawned in one synchronous loop
/// iteration before the first `join_next().await`, so the yields below are guaranteed to have
/// polled them). Opening the gate then requires the window to refill 5 times over for the
/// per-peer route assertions to hold.
#[tokio::test]
async fn a_pass_refills_the_fetch_window_and_never_exceeds_it() {
    const EXTRA: usize = 5;
    let total = DESCRIBE_FETCH_CONCURRENCY + EXTRA;

    // 0 permits: a fetch that reaches this cannot finish until the test opens the gate.
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let started = Arc::new(AtomicUsize::new(0));
    let fetch: DescribeFetcher = {
        let gate = gate.clone();
        let started = started.clone();
        Arc::new(move |provider: String, _addrs: Vec<String>| {
            let gate = gate.clone();
            let started = started.clone();
            Box::pin(async move {
                started.fetch_add(1, Ordering::SeqCst);
                gate.acquire().await.expect("the gate is never closed").forget();
                Ok(opsapi::DescribeManifest { ops: vec![describe_op(&provider, "op")] })
            })
        })
    };
    let peers: Vec<opsapi::PeerAddr> = (0..total)
        .map(|i| opsapi::PeerAddr {
            provider: format!("p{i:02}"),
            addrs: vec![format!("127.0.0.1:{}", 9000 + i)],
        })
        .collect();
    let front = dynamic_front();
    let mut router = DescribeRouter::new(front.clone(), peers, fetch);
    let pass = tokio::spawn(async move { router.refresh_once().await });

    // Happens-before, not a sleep: wait until the window is FULL (hang guard has wide
    // headroom — with a smaller window this wait is what fails).
    tokio::time::timeout(Duration::from_secs(20), async {
        while started.load(Ordering::SeqCst) < DESCRIBE_FETCH_CONCURRENCY {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the pass must reach DESCRIBE_FETCH_CONCURRENCY simultaneous fetches");
    // Every task the pass spawned is runnable (all of them are parked on the gate only after
    // being polled), so these yields drain the scheduler's queue.
    for _ in 0..256 {
        tokio::task::yield_now().await;
    }
    assert_eq!(
        started.load(Ordering::SeqCst),
        DESCRIBE_FETCH_CONCURRENCY,
        "no fetch can complete while the gate is shut, so no MORE than the window may have \
         started — an unbounded fan-out would have started all {total}"
    );

    // Open the gate: finishing a fetch is the only thing that can free a window slot, so the
    // remaining peers are proof that the window REFILLS.
    gate.add_permits(total);
    tokio::time::timeout(Duration::from_secs(20), pass)
        .await
        .expect("the pass must not hang")
        .expect("the pass task must not panic")
        .expect("the pass builds");

    let table = front.table();
    for i in 0..total {
        let method = format!("p{i:02}.op");
        assert!(
            table.find_by_method(&method).is_some(),
            "every peer must be fetched — the window has to refill after the first wave \
             (missing {method})"
        );
    }
}

/// (#9 BOUNDED PASS) Two `PeerAddr` entries naming ONE provider apply in PEER ORDER — the
/// LAST contribution wins — even when its fetch finishes FIRST.
///
/// The concurrent pass collects results by peer INDEX and applies them in `self.peers` order
/// afterwards; 41c3344 claims that preserves the serial "last contribution wins" semantics but
/// nothing proved it. The fixture inverts completion order against peer order (index 0 blocks
/// until index 1 has finished), so an implementation that applied results in COMPLETION order
/// would install the FIRST entry's manifest and fail here.
#[tokio::test]
async fn duplicate_provider_entries_apply_in_peer_order_not_completion_order() {
    let second_done = Arc::new(tokio::sync::Notify::new());
    let fetch: DescribeFetcher = {
        let second_done = second_done.clone();
        Arc::new(move |_provider: String, addrs: Vec<String>| {
            let second_done = second_done.clone();
            Box::pin(async move {
                if addrs[0].ends_with(":2") {
                    // `notify_one` STORES the permit, so the waiter below cannot miss it.
                    second_done.notify_one();
                    Ok(opsapi::DescribeManifest { ops: vec![describe_op("dup", "second")] })
                } else {
                    second_done.notified().await;
                    Ok(opsapi::DescribeManifest { ops: vec![describe_op("dup", "first")] })
                }
            })
        })
    };
    let peers = vec![
        opsapi::PeerAddr { provider: "dup".into(), addrs: vec!["127.0.0.1:1".into()] },
        opsapi::PeerAddr { provider: "dup".into(), addrs: vec!["127.0.0.1:2".into()] },
    ];
    let front = dynamic_front();
    let mut router = DescribeRouter::new(front.clone(), peers, fetch);

    tokio::time::timeout(Duration::from_secs(20), router.refresh_once())
        .await
        .expect("the pass must not hang")
        .expect("the pass builds");

    let table = front.table();
    assert!(
        table.find_by_method("dup.second").is_some(),
        "the LAST peer entry for a provider must win, regardless of which fetch finished first"
    );
    assert!(
        table.find_by_method("dup.first").is_none(),
        "the earlier entry must have been overwritten, not merged"
    );
    assert_eq!(
        table.peers.get("dup").map(Vec::as_slice),
        Some(["127.0.0.1:2".to_string()].as_slice()),
        "the winning entry's ADDRESS SET is the one the table dispatches over"
    );
}

/// (#10 BOUNDED PASS) `DESCRIBE_PEER_TIMEOUT` must clear a COLD DIAL, or the bound silently
/// excludes reachable peers instead of only stalled ones.
///
/// The floor is `edge`'s `DIAL_DEADLINE` (5s, `core/edge/src/client.rs:34` — `pub(crate)`,
/// so it is named by file:line rather than imported, the same convention its own
/// `client_tests::client_timing_invariants` pins it with). `Reconnecting::get` caches a
/// connection only on SUCCESS (`core/remote/src/lib.rs:331-338`), so a timeout below the dial
/// budget DISCARDS the partial handshake every pass: a peer whose QUIC+mTLS dial legitimately
/// takes ~2s would never enter the table and every op to it would 404 forever. The ceiling is
/// `edge`'s 30s `EDGE_STREAM_GRACE` — the server-side bound this constant exists to beat.
#[test]
fn describe_peer_timeout_clears_the_edge_dial_deadline() {
    assert!(
        DESCRIBE_PEER_TIMEOUT > Duration::from_secs(5),
        "DESCRIBE_PEER_TIMEOUT ({DESCRIBE_PEER_TIMEOUT:?}) must exceed edge's 5s DIAL_DEADLINE \
         (core/edge/src/client.rs:34) or a slow-but-REACHABLE peer is excluded on every pass"
    );
    assert!(
        DESCRIBE_PEER_TIMEOUT < Duration::from_secs(30),
        "DESCRIBE_PEER_TIMEOUT ({DESCRIBE_PEER_TIMEOUT:?}) must stay under edge's 30s \
         EDGE_STREAM_GRACE — beating that server-side bound is why it exists"
    );
    let fleet_providers = domain_svc_count();
    assert!(
        DESCRIBE_FETCH_CONCURRENCY >= fleet_providers,
        "the fetch window ({DESCRIBE_FETCH_CONCURRENCY}) must cover the {fleet_providers}-provider \
         fleet in ONE wave, or a boot pass costs two DESCRIBE_PEER_TIMEOUTs"
    );
}

/// (#11 PREFIX GUARD) A manifest advertising a method the fetched peer does not OWN fails the
/// build (c437153) — including the dotless/empty-op shapes a naive equality-only guard misses.
///
/// Before that commit every advertised `OpManifest` became a route unconditionally, so
/// `inventory` could mint a `characters.sneaky` route whose dispatch then resolved
/// `provider_of` to a DIFFERENT peer — a route minted from an unvalidated claim. All four
/// negative cases below returned `Ok` against pre-c437153 code, so this test fails on it.
#[test]
fn describe_table_bails_on_a_foreign_or_malformed_provider_prefix() {
    // A well-formed manifest is the control: the guard must not reject the normal shape.
    let good = fetched(vec![(
        "inventory",
        vec!["127.0.0.1:9001"],
        vec![describe_op("inventory", "list")],
    )]);
    assert!(build_describe_table(&good).is_ok(), "a peer's OWN op must still build");

    // (a) A FOREIGN prefix — the headline case. Verb/path do not collide with anything.
    let foreign = fetched(vec![(
        "inventory",
        vec!["127.0.0.1:9001"],
        vec![describe_op_raw("characters.sneaky", "/characters/sneaky")],
    )]);
    let err = build_describe_table(&foreign)
        .err()
        .expect("a foreign provider prefix must bail")
        .to_string();
    assert!(err.contains("inventory"), "the bail must name the fetched peer: {err}");
    assert!(err.contains("characters.sneaky"), "the bail must name the method: {err}");

    // (b) DOTLESS — `provider_of` returns the WHOLE method when there is no `.`, so it
    // compares EQUAL to the provider: the disjunct an equality-only guard lets through, and
    // the resulting route has no routable op suffix at all.
    let dotless = fetched(vec![(
        "inventory",
        vec!["127.0.0.1:9001"],
        vec![describe_op_raw("inventory", "/inventory")],
    )]);
    assert!(
        build_describe_table(&dotless).is_err(),
        "a dotless method compares equal to its provider and must still be rejected"
    );

    // (c) EMPTY op name — `"inventory."` also passes the equality check.
    let empty_op = fetched(vec![(
        "inventory",
        vec!["127.0.0.1:9001"],
        vec![describe_op_raw("inventory.", "/inventory/")],
    )]);
    assert!(
        build_describe_table(&empty_op).is_err(),
        "an empty op name leaves no routable method and must be rejected"
    );

    // (d) EMPTY provider key — it would otherwise match the empty prefix of `".op"`.
    let empty_provider = fetched(vec![("", vec!["127.0.0.1:9001"], vec![describe_op_raw(".op", "/op")])]);
    assert!(
        build_describe_table(&empty_provider).is_err(),
        "an empty provider key must be rejected outright"
    );
}

/// (#13 PREFIX GUARD) A foreign-prefix pass FREEZES the installed table and the router
/// RECOVERS on the next good pass — both halves of the documented fail-closed behaviour.
///
/// Pass 2's `Err` must not swap, drop, or half-install anything: the very same table `Arc`
/// stays installed and keeps serving. Pass 3 (a good, CHANGED manifest) must rebuild — the
/// freeze is not a latch, and a router that stayed stuck would silently serve a stale table
/// forever behind a green `/readyz`.
#[tokio::test]
async fn a_foreign_prefix_pass_freezes_the_installed_table_and_recovers() {
    use std::sync::atomic::AtomicUsize as Pass;

    let front = dynamic_front();
    let pass = Arc::new(Pass::new(0));
    let fetch: DescribeFetcher = {
        let pass = pass.clone();
        Arc::new(move |_provider: String, _addrs: Vec<String>| {
            let n = pass.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move {
                let ops = match n {
                    0 => vec![describe_op("inventory", "list")],
                    // Pass 2: a method this peer does not own.
                    1 => vec![
                        describe_op("inventory", "list"),
                        describe_op_raw("characters.sneaky", "/characters/sneaky"),
                    ],
                    // Pass 3: good again, and CHANGED, so a healthy router must rebuild.
                    _ => vec![describe_op("inventory", "list"), describe_op("inventory", "grant")],
                };
                Ok(opsapi::DescribeManifest { ops })
            })
        })
    };
    let peers = vec![opsapi::PeerAddr {
        provider: "inventory".into(),
        addrs: vec!["127.0.0.1:9001".into()],
    }];
    let mut router = DescribeRouter::new(front.clone(), peers, fetch);

    router.refresh_once().await.expect("pass 1 builds");
    let good = front.table();
    assert!(good.find_by_method("inventory.list").is_some(), "fixture: pass 1 installed routes");

    let err = router
        .refresh_once()
        .await
        .expect_err("pass 2 must FAIL the build, not install a half-table")
        .to_string();
    assert!(err.contains("characters.sneaky"), "the bail names the offending method: {err}");
    assert!(
        Arc::ptr_eq(&good, &front.table()),
        "a failed pass must leave the EXACT installed table in place (no swap, no drop)"
    );

    router.refresh_once().await.expect("pass 3 recovers");
    let recovered = front.table();
    assert!(
        !Arc::ptr_eq(&good, &recovered),
        "the freeze must not latch — a later good pass has to rebuild and swap"
    );
    assert!(recovered.find_by_method("inventory.grant").is_some(), "the new op is routed");
    assert!(recovered.find_by_method("inventory.list").is_some(), "the old op survives");
    assert!(
        recovered.find_by_method("characters.sneaky").is_none(),
        "the rejected method must never appear in an installed table"
    );
}
