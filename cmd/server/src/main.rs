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

/// Parses `CREDENTIAL_ADMISSION_TIMEOUT_MS` — the front door's whole-credential-
/// admission deadline (api-key check + session verify, both public planes). Env is
/// read HERE in the composition root (the gateway module never reads env).
fn admission_budget_from_env() -> anyhow::Result<Option<std::time::Duration>> {
    admission_budget_from_value(std::env::var("CREDENTIAL_ADMISSION_TIMEOUT_MS").ok().as_deref())
}

/// The testable parser body. Each front main (`cmd/server` here and `cmd/gateway-svc`)
/// keeps its OWN copy of this fn per the repo's env-in-main convention — there is no
/// shared config crate for `cmd/*` roots.
///
/// Unset/blank/unparseable → `Ok(None)`: the module's 5000ms default applies (the same
/// lenient trim/parse shape `core/app`'s grace knobs use). An EXPLICIT `0` FAILS
/// STARTUP LOUDLY: unlike the sibling knobs where `0` means "disable", this deadline
/// guards an always-on security surface — a zero budget would time out every admission
/// instantly (every credentialed request 503s, a silently bricked front door), and
/// mapping `0` to "no bound" would reintroduce the unbounded-hang defect, so neither
/// meaning is acceptable to infer silently.
fn admission_budget_from_value(raw: Option<&str>) -> anyhow::Result<Option<std::time::Duration>> {
    let Some(v) = raw.map(str::trim).filter(|v| !v.is_empty()) else {
        return Ok(None);
    };
    match v.parse::<u64>() {
        Ok(0) => anyhow::bail!(
            "CREDENTIAL_ADMISSION_TIMEOUT_MS=0 is invalid: 0 would time out every \
             admission instantly (every credentialed request would 503); running \
             without a bound is not supported — unset the var for the 5000ms default"
        ),
        Ok(ms) => Ok(Some(std::time::Duration::from_millis(ms))),
        Err(_) => Ok(None),
    }
}

#[cfg(test)]
mod admission_budget_tests;

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
    // The monolith fronts the same two public planes as gateway-svc, so its
    // credential-admission budget is configured the same way — env parsed here in
    // main (where runtime handles are built), carried as plain data on the wiring.
    let mut wiring = ProcessWiring::new();
    if let Some(budget) = admission_budget_from_env()? {
        wiring = wiring.with_admission_budget(budget);
    }
    let mut mods = server::modules(&wiring, Some(player.clone()));
    mods.push(Box::new(webui::WebUi::new())); // dev demo SPA at GET /; monolith-only (the one sanctioned fortress-svc exception)

    // No internal edge server: every provider is in-process in the monolith, so no
    // cross-module call ever crosses the mTLS edge. The player QUIC front IS wired
    // (all ops resolve Local — see `select_kind` in `modules/gateway`).
    app::run(app::Config::from_env(), mods, None, Some(player)).await
}
