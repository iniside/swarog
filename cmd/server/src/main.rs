//! `server` — the MONOLITH entrypoint (port of Go's `cmd/server`). It hosts EVERY
//! module in ONE process, with no internal edge server: every cross-module dependency
//! resolves locally through the registry (inventory's `require::<dyn Ownership>` takes
//! the in-process branch), so nothing crosses the internal mTLS QUIC boundary. The
//! split entrypoints (`characters-svc`, `inventory-svc`) each import only their own
//! modules; this binary is the opposite end — the full set. Per the
//! `never-monolith-only-features` memory, the monolith ALSO fronts players over the
//! QUIC player plane (all ops dispatch Local) — the same feature both topologies serve.

use std::sync::{Arc, Mutex};

use lifecycle::Module;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().init();

    // One shared player-facing QUIC server for this process; `Gateway::with_player_edge`
    // installs the front's dispatch handler onto it during `init`, and `app::run`
    // `listen`s the same handle after Build — the monolith serves players over QUIC too.
    let player = Arc::new(Mutex::new(edge::PlayerServer::new()));

    // All modules, hosted locally. The durable-events plane is app-owned process
    // infrastructure (`core/app::run` constructs, migrates, starts and stops it) — it
    // is never listed here; its Stop ordering (delivery halts before any module tears
    // down) is structural in `app::run`, not a list-order convention.
    //
    // `tools/checkmodules::monolith_modules()` mirrors this list (minus `metrics`,
    // which has no requires/topics/schema) for the topiccheck/requirecheck harnesses —
    // keep it in sync when adding a module here.
    let mods: Vec<Box<dyn Module>> = vec![
        Box::new(metrics::Metrics::new()), // core-infra: mounts GET /metrics + contributes the record layer
        Box::new(config::Config::new()),         // DB-backed config: schema "config", provides "config.reader"
        Box::new(characters::Characters::new()), // player characters; owns schema "characters"
        Box::new(inventory::Inventory::new()),   // owner-scoped inventories; depends on characters + config
        Box::new(accounts::Accounts::new()),     // player identity: sessions + dev/epic auth; owns schema "accounts"
        Box::new(admin::Admin::new()),           // GameOps portal at /admin; renders LOCAL contributions (all providers in-process)
        Box::new(audit::Audit::new()),           // append-only event ledger; owns schema "audit", records durable events in-process
        Box::new(scheduler::Scheduler::new()),   // data-driven durable event source; owns schema "scheduler", emits scheduler.fired
        Box::new(rating::Rating::new()),         // in-memory MMR; provides "rating.mmr_reader", reacts to match.finished (+15/-15)
        Box::new(match_module::MatchModule::new()), // records matches (schema "match"); reads rating sync, emits match.finished durably
        Box::new(leaderboard::LeaderboardModule::new()), // win tally; owns schema "leaderboard", reacts to match.finished, serves GET /leaderboard
        Box::new(apikeys::ApiKeys::new()),       // API-key policy store: schema "apikeys", provides "apikeys.keys" for the gateway's key check
        Box::new(webui::WebUi::new()), // dev demo SPA at GET /; monolith-only (the one sanctioned fortress-svc exception)
        Box::new(gateway::Gateway::new().with_player_edge(player.clone())), // HTTP + player QUIC front, auth-once (real accounts sessions)
    ];

    // No internal edge server: every provider is in-process in the monolith, so no
    // cross-module call ever crosses the mTLS edge. The player QUIC front IS wired
    // (all ops resolve Local — see `select_kind` in `modules/gateway`).
    app::run(app::Config::from_env(), mods, None, Some(player)).await
}
