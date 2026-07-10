//! `server` — the MONOLITH entrypoint (port of Go's `cmd/server`). It hosts EVERY
//! module in ONE process, with no internal edge server: every cross-module dependency
//! resolves locally through the registry (inventory's `require::<dyn Ownership>` takes
//! the in-process branch), so nothing crosses the internal mTLS QUIC boundary. The
//! split entrypoints (`characters-svc`, `inventory-svc`) each import only their own
//! modules; this binary is the opposite end — the full set. Per the
//! `never-monolith-only-features` memory, the monolith ALSO fronts players over the
//! QUIC player plane (all ops dispatch Local) — the same feature both topologies serve.

use std::sync::{Arc, Mutex};

use lifecycle::ProcessWiring;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().init();

    // One shared player-facing QUIC server for this process; `Gateway::with_player_edge`
    // installs the front's dispatch handler onto it during `init`, and `app::run`
    // `listen`s the same handle after Build — the monolith serves players over QUIC too.
    let player = Arc::new(Mutex::new(edge::PlayerServer::new()));

    // All modules but `webui`, hosted locally via `server::modules` (Step 10) — see
    // that lib's doc comment for why the demo SPA is pushed here instead of inside the
    // lib (keeps `demos/webui` reachable ONLY through cmd/server's OWN main, never
    // through `tools/checkmodules`, which links this crate as a library). The
    // durable-events plane is app-owned process infrastructure (`core/app::run`
    // constructs, migrates, starts and stops it) — it is never listed here; its Stop
    // ordering (delivery halts before any module tears down) is structural in
    // `app::run`, not a list-order convention.
    let mut mods = server::modules(&ProcessWiring::new(), Some(player.clone()));
    mods.push(Box::new(webui::WebUi::new())); // dev demo SPA at GET /; monolith-only (the one sanctioned fortress-svc exception)

    // No internal edge server: every provider is in-process in the monolith, so no
    // cross-module call ever crosses the mTLS edge. The player QUIC front IS wired
    // (all ops resolve Local — see `select_kind` in `modules/gateway`).
    app::run(app::Config::from_env(), mods, None, Some(player)).await
}
