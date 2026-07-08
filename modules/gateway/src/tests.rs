use super::*;
use axum::http::Request as HttpRequest;
use opsapi::{DecodeFn, EncodeFn, LocalOp, OpSet, Status};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tower::ServiceExt; // for `oneshot`

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
    assert_eq!(v.verify("dev-alice").await.as_deref(), Some("alice"));
    assert_eq!(v.verify("dev-").await, None); // empty suffix rejected
    assert_eq!(v.verify("alice").await, None); // no prefix rejected
    assert_eq!(v.verify("").await, None);
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
/// either plane (the axum fallback or the player handler) through it.
fn demo_front_door() -> Arc<FrontDoor> {
    let slots = Arc::new(Slots::new());
    let op = demo_opset();
    slots.contribute(opsapi::SLOT, op.operation);
    slots.contribute(opsapi::BINDING_SLOT, op.binding);
    slots.contribute(opsapi::LOCAL_SLOT, op.local);
    Arc::new(FrontDoor::new(slots, Arc::new(DevSessionVerifier::new())))
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
    let router = demo_router();
    let req = HttpRequest::builder()
        .method("POST")
        .uri("/demo/42")
        .body(Body::from("1"))
        .unwrap();
    let resp = router.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
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
    let front = Arc::new(FrontDoor::new(slots, Arc::new(DevSessionVerifier::new())));
    let router = front.router();

    let req = HttpRequest::builder().method("GET").uri("/demo/7").body(Body::empty()).unwrap();
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
    let table = RouteTable::build(&slots);

    let route = table.find_by_method("demo.echo").expect("bound op is found");
    assert_eq!(route.op.method, "demo.echo");
    assert_eq!(route.op.auth, AuthReq::Player);

    // A wire-only internal method was never contributed → invisible (the
    // allow-list gate the player plane relies on).
    assert!(table.find_by_method("characters.ownerOf").is_none());
}

// ---- the player handler: the pinned {status, err} grammar on every outcome ----

/// Drives the player handler exactly as the `edge::PlayerServer` would, returning
/// the response payload as a string. The handler is Ok on EVERY domain outcome
/// (the pinned grammar), so unwrapping here is itself an assertion.
async fn call_player(
    front: &Arc<FrontDoor>,
    method: &str,
    token: Option<&str>,
    payload: &[u8],
) -> String {
    let h = front.player_handler();
    let bytes = h(method.to_string(), token.map(str::to_string), payload.to_vec())
        .await
        .expect("domain outcomes never surface as transport Err");
    String::from_utf8(bytes).unwrap()
}

#[tokio::test]
async fn player_missing_token_on_auth_op_is_unauthorized_envelope() {
    let front = demo_front_door();
    let body = call_player(&front, "demo.echo", None, br#"{"n":1}"#).await;
    // Exact macro grammar: field `err`, Status as bare variant name.
    assert_eq!(body, r#"{"status":"Unauthorized","err":"unauthorized"}"#);
}

#[tokio::test]
async fn player_bad_token_is_unauthorized_envelope() {
    let front = demo_front_door();
    let body = call_player(&front, "demo.echo", Some("nope-x"), br#"{"n":1}"#).await;
    assert_eq!(body, r#"{"status":"Unauthorized","err":"unauthorized"}"#);
}

#[tokio::test]
async fn player_unknown_method_is_not_found_envelope() {
    let front = demo_front_door();
    // `characters.ownerOf` is the canonical wire-only internal: a peer edge
    // serves it, but it is absent from the route table → not player-reachable.
    let body = call_player(&front, "characters.ownerOf", Some("dev-alice"), b"{}").await;
    assert_eq!(body, r#"{"status":"NotFound","err":"unknown operation"}"#);
}

#[tokio::test]
async fn player_malformed_json_is_invalid_at_the_front() {
    let front = demo_front_door();
    let body = call_player(&front, "demo.echo", Some("dev-alice"), b"{not json").await;
    assert_eq!(body, r#"{"status":"Invalid","err":"malformed request payload"}"#);
}

#[tokio::test]
async fn player_happy_path_returns_wire_response_verbatim() {
    let front = demo_front_door();
    // No OpBinding::decode on this plane: the payload IS the wire request.
    let body = call_player(&front, "demo.echo", Some("dev-alice"), br#"{"n":1}"#).await;
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
    let front = Arc::new(FrontDoor::new(slots, Arc::new(DevSessionVerifier::new())));

    // No token at all — must dispatch, not 401.
    let body = call_player(&front, "demo.public", None, b"{}").await;
    assert_eq!(body, r#"{"status":"Ok","anon":true}"#);
}

#[tokio::test]
async fn player_backend_error_is_reserialized_as_status_err_envelope() {
    // A backend failure (an Err(opsapi::Error), not a status-carrying payload)
    // must still come back in the pinned {status, err} grammar. Drive it with an
    // op that has no local invoker AND no <PROVIDER>_EDGE_ADDR: dispatch fails
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
    // NO LOCAL_SLOT contribution → Remote; GHOSTPROV_EDGE_ADDR is unset.
    let remote_front = Arc::new(FrontDoor::new(slots, Arc::new(DevSessionVerifier::new())));
    let body = call_player(&remote_front, "ghostprov.op", None, b"{}").await;
    assert!(body.starts_with(r#"{"status":"Unavailable","err":""#), "{body}");
    assert!(body.contains("GHOSTPROV_EDGE_ADDR"), "{body}");
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
    let table = RouteTable::build(&Slots::new());
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

    // Second request goes back through remote_caller (re-dial). With no
    // FAKEPROV_EDGE_ADDR the re-dial path itself errors — and crucially the DEAD
    // caller was NOT reused (its call count is unchanged).
    let err = table.dispatch(&op, Identity::none(), b"{}".to_vec()).await.unwrap_err();
    assert!(err.msg.contains("FAKEPROV_EDGE_ADDR"), "{}", err.msg);
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
