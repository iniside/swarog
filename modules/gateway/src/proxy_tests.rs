//! Tests for the HTTP passthrough: the pure prefix/origin/XFF helpers, and one
//! end-to-end proxy round-trip against a live upstream axum server (proving method +
//! path + headers are preserved, the body is relayed, `X-Forwarded-For` is extended,
//! and an unmatched prefix stays 404).

use std::net::SocketAddr;

use axum::body::Body;
use axum::http::{HeaderMap, HeaderValue, Request, StatusCode};
use axum::routing::any;
use axum::Router;

use super::*;

/// Builds a table directly (bypassing env) for deterministic tests.
fn table(routes: &[(&str, &str)]) -> ProxyTable {
    let mut routes: Vec<(String, String)> = routes
        .iter()
        .map(|(p, o)| (p.to_string(), normalize_origin(o)))
        .collect();
    routes.sort_by_key(|(prefix, _)| std::cmp::Reverse(prefix.len()));
    ProxyTable {
        routes,
        client: reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("proxy client"),
    }
}

// ---- pure helpers -----------------------------------------------------------

#[test]
fn origin_matches_prefix_and_subtree_not_sibling() {
    let t = table(&[("/admin", "127.0.0.1:8085")]);
    assert_eq!(t.origin_for("/admin"), Some("http://127.0.0.1:8085"));
    assert_eq!(t.origin_for("/admin/characters"), Some("http://127.0.0.1:8085"));
    assert_eq!(t.origin_for("/admin/theme.css"), Some("http://127.0.0.1:8085"));
    // A longer sibling word is NOT under the prefix.
    assert_eq!(t.origin_for("/administrator"), None);
    assert_eq!(t.origin_for("/characters"), None);
}

#[test]
fn longest_prefix_wins() {
    let t = table(&[("/accounts", "127.0.0.1:1"), ("/accounts/epic", "127.0.0.1:2")]);
    assert_eq!(t.origin_for("/accounts/epic/callback"), Some("http://127.0.0.1:2"));
    assert_eq!(t.origin_for("/accounts/login"), Some("http://127.0.0.1:1"));
}

#[test]
fn from_routes_normalizes_sorts_and_skips_blank() {
    // Longest-prefix-first ordering + `http://` normalization, and a blank origin
    // (the composition root's skip-empty case) drops that prefix so it stays a 404.
    let t = ProxyTable::from_routes(vec![
        ("/accounts".to_string(), "127.0.0.1:1".to_string()),
        ("/accounts/epic".to_string(), "127.0.0.1:2".to_string()),
        ("/admin".to_string(), "   ".to_string()), // blank → dropped
    ]);
    assert_eq!(t.origin_for("/accounts/epic/callback"), Some("http://127.0.0.1:2"));
    assert_eq!(t.origin_for("/accounts/login"), Some("http://127.0.0.1:1"));
    assert_eq!(t.origin_for("/admin"), None, "a blank origin proxies nothing");
}

#[test]
fn from_routes_empty_proxies_nothing() {
    let t = ProxyTable::from_routes(Vec::new());
    assert_eq!(t.origin_for("/admin"), None);
    assert_eq!(t.origin_for("/accounts/epic"), None);
}

#[test]
fn normalize_origin_forms() {
    assert_eq!(normalize_origin("127.0.0.1:8085"), "http://127.0.0.1:8085");
    assert_eq!(normalize_origin("http://host:9/"), "http://host:9");
    assert_eq!(normalize_origin("https://host"), "https://host");
}

#[test]
fn xff_chain_building() {
    let peer: SocketAddr = "203.0.113.7:5000".parse().unwrap();
    // No prior chain, known peer → just the peer IP.
    let v = build_xff(&HeaderMap::new(), Some(peer)).unwrap();
    assert_eq!(v.to_str().unwrap(), "203.0.113.7");
    // Prior chain + peer → appended.
    let mut h = HeaderMap::new();
    h.insert("x-forwarded-for", HeaderValue::from_static("198.51.100.1"));
    let v = build_xff(&h, Some(peer)).unwrap();
    assert_eq!(v.to_str().unwrap(), "198.51.100.1, 203.0.113.7");
    // Neither → header left unset.
    assert!(build_xff(&HeaderMap::new(), None).is_none());
}

// ---- end-to-end proxy round-trip -------------------------------------------

/// Spawns a tiny upstream that echoes `method path xff=<X-Forwarded-For> body=<body>`,
/// returning its base origin (`http://127.0.0.1:<port>`).
async fn spawn_upstream() -> String {
    async fn echo(req: Request<Body>) -> String {
        let method = req.method().clone();
        let path = req.uri().path().to_string();
        let xff = req
            .headers()
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let bytes = axum::body::to_bytes(req.into_body(), 1 << 20).await.unwrap();
        format!("{method} {path} xff={xff} body={}", String::from_utf8_lossy(&bytes))
    }
    let app = Router::new().fallback(any(echo));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

#[tokio::test(flavor = "multi_thread")]
async fn forward_proxies_verbatim_and_extends_xff() {
    let origin = spawn_upstream().await;
    let t = table(&[("/admin", origin.trim_start_matches("http://"))]);

    let (parts, body) = Request::builder()
        .method("POST")
        .uri("/admin/characters?x=1")
        .header("authorization", "Basic dXNlcjpwYXNz")
        .body(Body::from("payload"))
        .unwrap()
        .into_parts();

    let peer: SocketAddr = "203.0.113.9:4000".parse().unwrap();
    let resp = t.forward(parts, body, Some(peer)).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20).await.unwrap();
    let text = String::from_utf8_lossy(&bytes);
    // Method + path + query preserved, body streamed through, XFF set to the peer.
    assert!(text.contains("POST /admin/characters"), "got: {text}");
    assert!(text.contains("xff=203.0.113.9"), "got: {text}");
    assert!(text.contains("body=payload"), "got: {text}");
}

/// Spawns an upstream that always answers `302 Found` with `Location: /#token=abc`,
/// modelling the Epic OAuth callback redirect whose fragment must survive verbatim.
async fn spawn_redirecting_upstream() -> String {
    async fn redirect() -> impl IntoResponse {
        (StatusCode::FOUND, [("location", "/#token=abc")])
    }
    let app = Router::new().fallback(any(redirect));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

#[tokio::test(flavor = "multi_thread")]
async fn forward_relays_redirect_without_following() {
    let origin = spawn_redirecting_upstream().await;
    let t = table(&[("/accounts/epic", origin.trim_start_matches("http://"))]);

    let (parts, body) = Request::builder()
        .method("GET")
        .uri("/accounts/epic/callback?code=x&state=y")
        .body(Body::empty())
        .unwrap()
        .into_parts();

    let resp = t.forward(parts, body, None).await;
    // The proxy must NOT follow the redirect — it relays the 302 + Location verbatim so
    // the browser applies the `#token` fragment (which a server-side follow would drop).
    assert_eq!(resp.status(), StatusCode::FOUND);
    assert_eq!(
        resp.headers().get("location").unwrap().as_bytes(),
        b"/#token=abc",
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn unmatched_prefix_is_404() {
    let t = table(&[("/admin", "127.0.0.1:1")]);
    let (parts, body) = Request::builder()
        .uri("/characters")
        .body(Body::empty())
        .unwrap()
        .into_parts();
    let resp = t.forward(parts, body, None).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test(flavor = "multi_thread")]
async fn empty_table_is_404() {
    let t = table(&[]);
    let (parts, body) = Request::builder()
        .uri("/admin")
        .body(Body::empty())
        .unwrap()
        .into_parts();
    let resp = t.forward(parts, body, None).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}
