//! Middleware tests: burst-then-429 through a real router (Go's
//! `TestRateLimit_AllowsBurstThenBlocks`), per-IP isolation, and skip-infra (Go's
//! `TestSkipInfra`). Requests carry a `ConnectInfo` extension, as `app::run` supplies.

use super::*;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request as HttpRequest, StatusCode};
use axum::routing::get;
use axum::Router;
use std::net::SocketAddr;
use std::sync::Arc;
use tower::ServiceExt; // for `oneshot`

use crate::IpLimiter;

/// A request to `path` with `peer` injected as the connection info (as
/// `into_make_service_with_connect_info` does in production).
fn req(path: &str, peer: &str) -> HttpRequest<Body> {
    let mut r = HttpRequest::builder()
        .uri(path)
        .body(Body::empty())
        .unwrap();
    let addr: SocketAddr = peer.parse().unwrap();
    r.extensions_mut().insert(ConnectInfo(addr));
    r
}

/// A router with a domain route + an infra route, wrapped by the rate limiter (no
/// trusted proxies, so the connection peer is authoritative).
fn app(limiter: Arc<IpLimiter>) -> Router {
    let base = Router::new()
        .route("/x", get(|| async { "ok" }))
        .route("/metrics", get(|| async { "metrics" }));
    mount(base, limiter, Arc::new(vec![]))
}

async fn status(app: &Router, path: &str, peer: &str) -> StatusCode {
    app.clone().oneshot(req(path, peer)).await.unwrap().status()
}

#[tokio::test]
async fn allows_burst_then_429_with_retry_after() {
    // rate 0, burst 3: exactly 3 pass from one IP, then 429 + Retry-After: 1.
    let app = app(IpLimiter::new(0.0, 3));
    for i in 0..3 {
        assert_eq!(status(&app, "/x", "9.9.9.9:1111").await, StatusCode::OK, "req {i}");
    }
    let resp = app.clone().oneshot(req("/x", "9.9.9.9:1111")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(resp.headers().get("retry-after").unwrap(), "1");
}

#[tokio::test]
async fn separate_ips_have_separate_buckets() {
    let app = app(IpLimiter::new(0.0, 1));
    assert_eq!(status(&app, "/x", "1.1.1.1:1").await, StatusCode::OK);
    assert_eq!(status(&app, "/x", "1.1.1.1:1").await, StatusCode::TOO_MANY_REQUESTS);
    // A different peer keys a different bucket.
    assert_eq!(status(&app, "/x", "2.2.2.2:1").await, StatusCode::OK);
}

#[tokio::test]
async fn skip_infra_never_limited() {
    // A limiter with zero capacity blocks every non-infra path; /metrics is exempt.
    let app = app(IpLimiter::new(0.0, 0));
    assert_eq!(status(&app, "/metrics", "9.9.9.9:1").await, StatusCode::OK);
    assert_eq!(status(&app, "/x", "9.9.9.9:1").await, StatusCode::TOO_MANY_REQUESTS);
}
