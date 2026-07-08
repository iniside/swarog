//! `webui` — a UI-ONLY dev demo module (port of Go's `modules/webui`). It serves a
//! single embedded single-page app at the EXACT path `GET /`: dev login/register,
//! then "Link Epic", exercising the accounts HTTP surface
//! (`/accounts/register|login|me`, `/accounts/epic/start|callback`) from a browser
//! so the account-linking flow is visible without a separate client.
//!
//! No state, no `Requires`, no schema, no events — just one static route mounted on
//! the shared router (`ctx.mount`, the same seam `accounts::epic_oauth` uses). This
//! is the ONE sanctioned exception to the fortress-svc rule: a dev demo SPA has no
//! independent deployment story, so it is registered in `cmd/server` (the monolith)
//! only — there is no `cmd/webui-svc`.

use axum::http::header;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;
use lifecycle::{Context, Module};

#[cfg(test)]
mod tests;

/// The embedded demo page (copied verbatim from Go's `modules/webui/index.html`).
/// Its `fetch()` calls target `/accounts/register`, `/accounts/login`,
/// `/accounts/me`, and `/accounts/epic/start` — all live, byte-identical routes on
/// today's Rust `accountsapi::Auth` HTTP surface, so no path/body-key adjustment
/// was needed.
const INDEX_HTML: &str = include_str!("index.html");

async fn index() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], INDEX_HTML)
}

/// A dev demo SPA module: mounts exactly one route, `GET /`. Anything else under
/// `/` is unmatched here and falls through to the gateway's fallback (or a plain
/// 404 if no fallback is mounted) — `axum::routing::get("/")` matches the root path
/// only, never a prefix.
#[derive(Default)]
pub struct WebUi;

impl WebUi {
    pub fn new() -> Self {
        WebUi
    }
}

#[async_trait::async_trait]
impl Module for WebUi {
    fn name(&self) -> &str {
        "webui"
    }

    fn init(&self, ctx: &Context) -> anyhow::Result<()> {
        ctx.mount(router());
        Ok(())
    }
}

fn router() -> Router {
    Router::new().route("/", get(index))
}

/// Exposed for the module's own test module.
#[cfg(test)]
pub(crate) fn test_router() -> Router {
    router()
}
