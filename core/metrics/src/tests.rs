//! HTTP-metrics tests: drive real requests through an axum router wrapped by [`mount`]
//! and assert against the rendered `/metrics` exposition. The private registry is a
//! process-global built once, so these tests share it — every assertion is therefore
//! written as "this series is present" (never an exact count / clean-slate), and uses a
//! route pattern (`/widgets/:id`) no other test touches.

use super::*;
use axum::body::Body;
use axum::http::{Request as HttpRequest, StatusCode};
use axum::routing::get;
use axum::Router;
use tower::ServiceExt; // for `oneshot`

/// A router that mirrors the real app surface: a parameterized domain route, a healthz
/// probe, plus the `/metrics` scrape route + recording layer the [`Metrics`] module wires
/// (mounted route + `httpmw::LAYER_SLOT` layer). Assembled directly here so the codec
/// tests exercise `scrape`/`record` without a `lifecycle::Context`.
fn app() -> Router {
    Router::new()
        .route("/widgets/:id", get(|| async { "ok" }))
        .route("/healthz", get(|| async { "ok" }))
        .route("/metrics", get(scrape))
        .layer(middleware::from_fn(record))
}

/// Sends one request through the router and returns the response body as a String.
async fn get_body(router: Router, uri: &str) -> (StatusCode, String) {
    let resp = router
        .oneshot(
            HttpRequest::builder()
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, String::from_utf8(bytes.to_vec()).unwrap())
}

/// A real request through a matched route is counted under the MATCHED PATTERN, not the
/// raw URL — the cardinality guard. `/widgets/42` must record `path="/widgets/:id"` and
/// never `path="/widgets/42"`.
#[tokio::test]
async fn counter_uses_matched_path_label_not_raw_url() {
    let (status, _) = get_body(app(), "/widgets/42").await;
    assert_eq!(status, StatusCode::OK);

    let (_, scrape) = get_body(app(), "/metrics").await;
    assert!(
        scrape.contains("http_requests_total"),
        "expected the counter series, got:\n{scrape}"
    );
    assert!(
        scrape.contains(r#"path="/widgets/:id""#),
        "expected the MATCHED pattern label, got:\n{scrape}"
    );
    assert!(
        !scrape.contains(r#"path="/widgets/42""#),
        "raw URL must never become a label (cardinality guard), got:\n{scrape}"
    );
}

/// `/metrics` renders the private registry in the Prometheus text format, including both
/// declared collectors.
#[tokio::test]
async fn metrics_endpoint_renders_exposition() {
    // Record at least one request so the counter family has a child line to render.
    let _ = get_body(app(), "/widgets/7").await;

    let (status, body) = get_body(app(), "/metrics").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("http_requests_total"), "got:\n{body}");
    assert!(
        body.contains("http_request_duration_seconds"),
        "got:\n{body}"
    );
}

/// Infra endpoints are exempt from recording: hitting `/healthz` must never produce a
/// `/healthz` series in the scrape. (Because the exemption is unconditional, no request
/// in any test could ever create one — so an absent label proves the skip regardless of
/// the shared-registry state.)
#[tokio::test]
async fn healthz_is_not_recorded() {
    let (status, _) = get_body(app(), "/healthz").await;
    assert_eq!(status, StatusCode::OK);

    let (_, scrape) = get_body(app(), "/metrics").await;
    assert!(
        !scrape.contains(r#"path="/healthz""#),
        "infra endpoint /healthz must not be recorded, got:\n{scrape}"
    );
    assert!(
        !scrape.contains(r#"path="/metrics""#),
        "the scrape itself must not be recorded, got:\n{scrape}"
    );
}

/// The module wiring: `init` mounts `GET /metrics` on the shared router AND contributes
/// exactly one `httpmw::HttpLayer` to `LAYER_SLOT`. Applying that layer over the mounted
/// router yields a live `/metrics` scrape — the same composition `app::run` performs.
#[tokio::test]
async fn module_init_mounts_scrape_and_contributes_one_layer() {
    let ctx = Context::new();
    Metrics::new().init(&ctx).unwrap();

    // Exactly one layer contributed to the slot the app drains.
    let layers = ctx.contributions::<httpmw::HttpLayer>(httpmw::LAYER_SLOT);
    assert_eq!(layers.len(), 1, "init must contribute exactly one HTTP layer");

    // Compose as app::run does: take the mounted router, then apply the layer over it.
    let router = layers[0].apply(ctx.take_router());
    let (status, body) = get_body(router, "/metrics").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("http_requests_total"), "got:\n{body}");
}
