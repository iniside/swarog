//! Route-shape tests for the `webui` module: `GET /` serves the embedded page, and
//! anything else (no fallback mounted here) 404s — the exact-path-only contract the
//! plan calls for.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt; // for `oneshot`

use crate::{test_router, INDEX_HTML};

#[tokio::test]
async fn root_serves_the_demo_page() {
    let router = test_router();
    let req = Request::builder().uri("/").body(Body::empty()).unwrap();
    let resp = router.oneshot(req).await.unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let content_type = resp
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert!(content_type.starts_with("text/html"));

    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(bytes, INDEX_HTML.as_bytes());
}

#[tokio::test]
async fn unmatched_path_is_not_found() {
    let router = test_router();
    let req = Request::builder()
        .uri("/nonexistent")
        .body(Body::empty())
        .unwrap();
    let resp = router.oneshot(req).await.unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}
