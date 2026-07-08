//! The front door's HTTP reverse-proxy passthrough (port of Go's
//! `gateway/httpproxy.go`). Some public surfaces are HTML/browser flows, NOT typed
//! operations: the admin portal (`/admin`) and the Epic web OAuth redirect
//! (`/accounts/epic/*`). They are served by a DIFFERENT process, so the front door
//! reverse-proxies them to that origin instead of dispatching them as ops.
//!
//! A request that matches no operation (the fallback's 404 path) is offered to this
//! [`ProxyTable`]: if its path falls under a configured prefix it is proxied verbatim
//! to the origin (method, path+query, and headers preserved minus hop-by-hop; body
//! streamed; `X-Forwarded-For` extended); otherwise it stays a 404. The prefix→origin
//! table is read from env, so a process with neither var set proxies nothing (every
//! unmatched route is still 404 — the exact prior behaviour).

use std::net::SocketAddr;

use axum::body::Body;
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};

/// Hop-by-hop headers (RFC 7230 §6.1) that a proxy must NOT forward, plus `host`
/// (reqwest sets it from the target origin). Matched case-insensitively (header names
/// are already lowercased by `http`).
const HOP_BY_HOP: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
    "host",
];

/// The prefix→origin reverse-proxy table + the shared HTTP client. Built once from env
/// in the front door; empty when no passthrough is configured.
pub struct ProxyTable {
    /// `(bare_prefix, origin_base)` pairs, e.g. `("/admin", "http://127.0.0.1:8085")`,
    /// sorted longest-prefix-first so `/accounts/epic` wins over a hypothetical
    /// `/accounts`.
    routes: Vec<(String, String)>,
    client: reqwest::Client,
}

impl ProxyTable {
    /// Reads the passthrough table from env: `ADMIN_HTTP_ADDR` serves `/admin`,
    /// `ACCOUNTS_HTTP_ADDR` serves `/accounts/epic`. An unset/blank var drops that
    /// prefix (so the route stays a 404). The caller decides the prefixes; the origin
    /// is a bare `host:port` (an `http://` scheme is added) or a full URL.
    pub fn from_env() -> ProxyTable {
        let mut routes = Vec::new();
        for (prefix, env_key) in [
            ("/admin", "ADMIN_HTTP_ADDR"),
            ("/accounts/epic", "ACCOUNTS_HTTP_ADDR"),
        ] {
            if let Ok(addr) = std::env::var(env_key) {
                let addr = addr.trim();
                if !addr.is_empty() {
                    routes.push((prefix.to_string(), normalize_origin(addr)));
                }
            }
        }
        routes.sort_by_key(|(prefix, _)| std::cmp::Reverse(prefix.len()));
        ProxyTable {
            routes,
            client: reqwest::Client::new(),
        }
    }

    /// The origin serving `path`, if any. A prefix matches the exact path or a subtree
    /// (`/admin` and `/admin/…`) but NOT a longer sibling word (`/administrator`) —
    /// the same semantics as Go registering both `prefix` and `prefix+"/"`.
    fn origin_for(&self, path: &str) -> Option<&str> {
        self.routes.iter().find_map(|(prefix, origin)| {
            let subtree = format!("{prefix}/");
            (path == prefix || path.starts_with(&subtree)).then_some(origin.as_str())
        })
    }

    /// The metrics route-pattern LABEL for a proxied `path`: the matched prefix as a
    /// wildcard subtree (e.g. `/admin` → `"/admin/*"`), so every proxied request records
    /// under one bounded series instead of the raw (attacker-controlled) URL. `None` when
    /// no prefix matches — the request stays a 404 and records under `"unmatched"`. Uses
    /// the SAME prefix-match semantics as [`origin_for`].
    pub fn pattern_for(&self, path: &str) -> Option<String> {
        self.routes.iter().find_map(|(prefix, _)| {
            let subtree = format!("{prefix}/");
            (path == prefix || path.starts_with(&subtree)).then(|| format!("{prefix}/*"))
        })
    }

    /// Proxies an unmatched request to its origin, or returns 404 when no prefix
    /// matches (the prior fallback behaviour). Streams the body both ways, preserves
    /// the method + headers (minus hop-by-hop), and extends `X-Forwarded-For` with the
    /// direct peer. An upstream dial/transport failure is a 502.
    pub async fn forward(&self, parts: Parts, body: Body, peer: Option<SocketAddr>) -> Response {
        let path = parts.uri.path();
        let Some(origin) = self.origin_for(path) else {
            return (StatusCode::NOT_FOUND, "not found").into_response();
        };

        // origin + path + query, preserved verbatim (Go's target URL has no base path).
        let tail = parts
            .uri
            .path_and_query()
            .map(|pq| pq.as_str())
            .unwrap_or(path);
        let url = format!("{origin}{tail}");

        let mut headers = parts.headers.clone();
        for h in HOP_BY_HOP {
            headers.remove(*h);
        }
        if let Some(xff) = build_xff(&parts.headers, peer) {
            headers.insert(HeaderName::from_static("x-forwarded-for"), xff);
        }

        let resp = self
            .client
            .request(parts.method.clone(), &url)
            .headers(headers)
            // Stream the request body to the origin without buffering: axum's
            // `into_data_stream` yields the body frames as a `Stream<Bytes>` (axum
            // `Body` itself is `!Sync`, which `wrap` rejects, so wrap the stream).
            .body(reqwest::Body::wrap_stream(body.into_data_stream()))
            .send()
            .await;

        match resp {
            Ok(upstream) => relay_response(upstream),
            Err(e) => {
                tracing::warn!(url = %url, err = %e, "gateway passthrough upstream failed");
                (StatusCode::BAD_GATEWAY, "upstream unavailable").into_response()
            }
        }
    }
}

/// Rebuilds the client-facing response from the upstream one: status + headers (minus
/// hop-by-hop) + the streamed body. Status/headers are `http` types shared by axum and
/// reqwest (both on `http` 1.x), so they copy directly.
fn relay_response(upstream: reqwest::Response) -> Response {
    let mut builder = Response::builder().status(upstream.status());
    for (name, value) in upstream.headers() {
        if HOP_BY_HOP.contains(&name.as_str()) {
            continue;
        }
        builder = builder.header(name, value);
    }
    builder
        .body(Body::from_stream(upstream.bytes_stream()))
        .unwrap_or_else(|_| {
            (StatusCode::BAD_GATEWAY, "bad upstream response").into_response()
        })
}

/// Computes the `X-Forwarded-For` value to send upstream: the existing chain (if the
/// client already sent one — we are a downstream proxy) with the direct peer's IP
/// appended. Returns `None` only when there is neither an existing chain nor a known
/// peer (e.g. a unit test with no connection info) — then the header is left unset.
fn build_xff(headers: &HeaderMap, peer: Option<SocketAddr>) -> Option<HeaderValue> {
    let existing = headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let peer_ip = peer.map(|p| p.ip().to_string());
    let chain = match (existing, peer_ip) {
        (Some(chain), Some(ip)) => format!("{chain}, {ip}"),
        (Some(chain), None) => chain,
        (None, Some(ip)) => ip,
        (None, None) => return None,
    };
    HeaderValue::from_str(&chain).ok()
}

/// Prepends `http://` to a bare `host:port` origin; leaves a full URL (with scheme)
/// untouched. Trailing slashes are trimmed so `origin + path` never doubles up.
fn normalize_origin(addr: &str) -> String {
    let base = if addr.contains("://") {
        addr.to_string()
    } else {
        format!("http://{addr}")
    };
    base.trim_end_matches('/').to_string()
}

#[cfg(test)]
#[path = "proxy_tests.rs"]
mod tests;
