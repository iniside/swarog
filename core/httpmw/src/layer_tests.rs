//! [`HttpLayer`] mechanics: one-shot application across clones (the contrib registry hands
//! clones back, but a `FnOnce` wrap must run at most once) and that a contributed layer
//! actually wraps the router it is applied to.

use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// The wrap runs at most once even though a clone (as the slot hands out) also calls
/// `apply` — the shared `Option<FnOnce>` is `take`n by the first caller.
#[test]
fn http_layer_applies_at_most_once_across_clones() {
    let calls = Arc::new(AtomicUsize::new(0));
    let counted = calls.clone();
    let layer = HttpLayer::new(move |r: Router| {
        counted.fetch_add(1, Ordering::SeqCst);
        r
    });
    let clone = layer.clone();

    let router = layer.apply(Router::new());
    // A second call on a CLONE is a no-op: the closure was already taken.
    let _ = clone.apply(router);

    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

/// The closure receives the router and its transformation is observable in the result: a
/// layer that adds a route makes that route reachable.
#[tokio::test]
async fn http_layer_wraps_the_router_it_is_applied_to() {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::routing::get;
    use tower::ServiceExt; // for `oneshot`

    let layer = HttpLayer::new(|r: Router| r.route("/added", get(|| async { "ok" })));
    let router = layer.apply(Router::new());

    let resp = router
        .oneshot(Request::builder().uri("/added").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}
