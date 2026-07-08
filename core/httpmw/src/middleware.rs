//! The axum rate-limiting middleware (port of Go's `RateLimit`). Applied by the boot
//! layer over the WHOLE surface via [`mount`], mirroring `metrics::mount`.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{ConnectInfo, Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::Router;
use ipnet::IpNet;

use crate::client_ip::client_ip;
use crate::limiter::IpLimiter;
use crate::skip_infra;

/// The middleware's shared state: the limiter and the trusted-proxy set for the
/// client-IP walk. Cheap to clone (two `Arc`s).
#[derive(Clone)]
struct RateLimitState {
    limiter: Arc<IpLimiter>,
    trusted: Arc<Vec<IpNet>>,
}

/// Wraps `router` in the per-IP rate limiter, returning the layered router (mirrors
/// `metrics::mount`). The boot layer applies this UNDER the metrics layer, so a `429`
/// the limiter issues is still recorded (Go's `metrics(ratelimit(mux))`). Infra
/// endpoints are skipped; the client IP is resolved trust-aware from the connection
/// peer + `X-Forwarded-For`/`X-Real-IP`.
pub fn mount(router: Router, limiter: Arc<IpLimiter>, trusted: Arc<Vec<IpNet>>) -> Router {
    router.layer(middleware::from_fn_with_state(
        RateLimitState { limiter, trusted },
        rate_limit,
    ))
}

/// Per-request rate-limit check: infra endpoints pass untouched; otherwise the client
/// IP is resolved and its bucket consulted, and an exhausted bucket yields `429` with a
/// plain body + `Retry-After: 1` (Go's `RateLimit` handler).
async fn rate_limit(State(state): State<RateLimitState>, req: Request, next: Next) -> Response {
    if skip_infra(req.uri().path()) {
        return next.run(req).await;
    }

    // The connection peer (inserted by `into_make_service_with_connect_info` in
    // `app::run`) is the ground truth. Absent it — only in a test that forgot the
    // extension — we cannot key per-IP, so fail OPEN rather than throttle everyone into
    // one bucket.
    let Some(&ConnectInfo(peer)) = req.extensions().get::<ConnectInfo<SocketAddr>>() else {
        return next.run(req).await;
    };

    let xff = header_str(&req, "x-forwarded-for");
    let x_real_ip = header_str(&req, "x-real-ip");
    let ip = client_ip(peer.ip(), xff.as_deref(), x_real_ip.as_deref(), &state.trusted);

    if state.limiter.allow(ip) {
        next.run(req).await
    } else {
        (
            StatusCode::TOO_MANY_REQUESTS,
            [(header::RETRY_AFTER, "1")],
            "rate limit exceeded",
        )
            .into_response()
    }
}

/// Reads a request header as an owned `String`, or `None` when absent/non-ASCII.
fn header_str(req: &Request, name: &str) -> Option<String> {
    req.headers()
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
}

#[cfg(test)]
#[path = "middleware_tests.rs"]
mod middleware_tests;
