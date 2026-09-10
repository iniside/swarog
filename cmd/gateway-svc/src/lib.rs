//! Library half of `gateway-svc`'s composition root (Step 10): the real module list,
//! extracted so `tools/checkmodules` can build the SAME set the process boots
//! without hand-mirroring it. `main.rs` resolves each peer edge address and
//! passthrough origin from env (unchanged defaults) into a [`ProcessWiring`]; the
//! checker harness builds an empty `ProcessWiring` and gets the same peer defaults
//! back from [`ProcessWiring::peer_set_or`] (a one-element default set) —
//! `register`/`init` do no I/O, so a dummy peer address is safe.
//!
//! `player` is the ONE runtime handle this lib is not allowed to construct itself
//! (Step 10's gateway-svc carve-out): `main.rs` owns the `Arc<Mutex<edge::PlayerServer>>`
//! socket handle and decides whether to install it via `with_player_edge` — a
//! checker passes `None` so it never touches a real QUIC listener. Gateway hosts no
//! durable subscription either way, so the player-edge presence is invisible to
//! `topiccheck`/`requirecheck`'s recorded event/require graph.

use std::sync::{Arc, Mutex};

use lifecycle::{Module, ProcessWiring};

/// Builds a stub's [`remote::PeerSource`] for an EDGE `provider`. The boot snapshot the
/// gateway route table reads is `wiring.peer_set_or` (the whole instance SET) in BOTH
/// modes. In MANAGED mode `edge_list_resolver` is `Some`, so the stub's capability caller
/// is a `remote::Pool` that round-robins across the live instances and re-resolves the
/// LIST on its own cadence (C2 — a moved peer OR a scale event is picked up with no
/// restart). In STANDALONE mode it is `None`: a single-address env value, `fixed`
/// (`Reconnecting`), byte-identical boot — no pool, no re-resolve.
fn edge_peer(
    wiring: &ProcessWiring,
    edge_list_resolver: Option<&dyn Fn(&'static str) -> remote::PeerListResolver>,
    provider: &'static str,
    default: &str,
) -> remote::PeerSource {
    let boot = wiring.peer_set_or(provider, &[default]);
    match edge_list_resolver {
        Some(make) => remote::PeerSource::pooled(boot, make(provider)),
        // Standalone/checker: a one-element set → a single fixed Reconnecting conn.
        None => remote::PeerSource::fixed(boot.into_iter().next().unwrap_or_default()),
    }
}

pub fn modules(
    wiring: &ProcessWiring,
    player: Option<Arc<Mutex<edge::PlayerServer>>>,
    // `Some` only in managed boot (`main.rs` owns `addrs::edge_list_resolver`, bound to the
    // agent URL): each edge stub's capability caller is then a round-robin `remote::Pool`
    // re-resolving the live instance LIST. `None` in standalone and in the `checkmodules`
    // harness — a single fixed conn, no pool, no re-resolve.
    edge_list_resolver: Option<&dyn Fn(&'static str) -> remote::PeerListResolver>,
    // The `/push` WebSocket bounds `main.rs` parsed from env. `None` in the
    // `checkmodules` harness — the module then applies its own defaults, and no checker
    // ever models the developer's ambient shell.
    push_limits: Option<gateway::PushLimits>,
) -> Vec<Box<dyn Module>> {
    // D2 routing-as-data: this front door builds its op route table from each peer's runtime
    // `__describe` manifest (re-fetched periodically), NOT from a compile-time `<name>rpc`
    // route import. So the stubs below contribute only their PEER_SLOT address set (+ the
    // accounts/apikeys sync CAPABILITY clients via `provide_factories`, never routes), and the
    // gateway module drives the describe fetch. A new `#[http]` op on any svc lights up here
    // with zero changes to this process.
    let mut gw = gateway::Gateway::new().with_describe_routing();
    if let Some(p) = player {
        gw = gw.with_player_edge(p);
    }
    for (prefix, origin) in wiring.passthrough() {
        gw = gw.with_passthrough(prefix, origin);
    }
    // The credential-admission budget (`CREDENTIAL_ADMISSION_TIMEOUT_MS`) is parsed in
    // `main.rs` like the passthrough origins; unset leaves the module's 5s default.
    if let Some(budget) = wiring.admission_budget() {
        gw = gw.with_admission_budget(budget);
    }
    if let Some(limits) = push_limits {
        gw = gw.with_push_limits(limits);
    }

    vec![
        Box::new(metrics::Metrics::new()), // core-infra: mounts GET /metrics + contributes the record layer
        Box::new(gw),
        // `remote` is generic (Step 4): this composition root injects each provider's swap
        // closures explicitly, so `remote` never names a provider. Under D2, the pure-HTTP
        // providers (characters/inventory/match/leaderboard) are `Stub::describe_peer` — NO
        // factories (their routes arrive via `__describe`), just the PEER_SLOT address set the
        // describe fetch + dispatch read. (`describe_peer` is the intentional peer-only
        // constructor — legal with zero factories, unlike `Stub::new`, whose zero-factory bail
        // stays a loud guard for an accidental forgotten `remote_factories()`.) accounts/apikeys
        // keep `Stub::new` with `provide_factories` — they DO provide a sync capability CLIENT
        // (Sessions/Keys, NO routes, so they can't collide with the describe pass that supplies
        // accounts's `#[http]` routes).
        Box::new(remote::Stub::describe_peer(
            "characters",
            edge_peer(wiring, edge_list_resolver, "characters", "127.0.0.1:9000"),
        )),
        Box::new(remote::Stub::describe_peer(
            "inventory",
            edge_peer(wiring, edge_list_resolver, "inventory", "127.0.0.1:9001"),
        )),
        Box::new(remote::Stub::new(
            "accounts",
            edge_peer(wiring, edge_list_resolver, "accounts", "127.0.0.1:9003"),
            accountsrpc::provide_factories(),
        )),
        Box::new(remote::Stub::new(
            "apikeys",
            edge_peer(wiring, edge_list_resolver, "apikeys", "127.0.0.1:9009"),
            apikeysrpc::provide_factories(),
        )),
        Box::new(remote::Stub::describe_peer(
            "match",
            edge_peer(wiring, edge_list_resolver, "match", "127.0.0.1:9006"),
        )),
        Box::new(remote::Stub::describe_peer(
            "leaderboard",
            edge_peer(wiring, edge_list_resolver, "leaderboard", "127.0.0.1:9008"),
        )),
        // wallet is pure-HTTP from this front door's point of view: it exposes
        // `GET /wallet/me` + `GET /wallet/currencies` (routes arriving via `__describe`)
        // and the gateway consumes NO wallet capability of its own — nothing here
        // `require`s `dyn Wallet`/`dyn Player`. So `describe_peer` (peer-only, zero
        // factories), never `Stub::new`, whose zero-factory bail exists to catch a
        // forgotten `provide_factories()` on a stub that WAS meant to provide one.
        Box::new(remote::Stub::describe_peer(
            "wallet",
            edge_peer(wiring, edge_list_resolver, "wallet", "127.0.0.1:9010"),
        )),
        // notifications is pure-HTTP from this front door's point of view (list/
        // mark_read/delete, routes arriving via `__describe`); the gateway consumes
        // no notifications capability of its own. `describe_peer`, never `Stub::new`
        // — `notificationsrpc` deliberately exposes no `remote_factories()`, and
        // `Stub::new` with an empty factory list `anyhow::bail!`s in `register`.
        Box::new(remote::Stub::describe_peer(
            "notifications",
            edge_peer(wiring, edge_list_resolver, "notifications", "127.0.0.1:9011"),
        )),
        // friends is pure-HTTP from this front door's point of view (request/accept/
        // decline/remove/list/pending, routes arriving via `__describe`); the gateway
        // consumes no friends capability of its own. `describe_peer`, never `Stub::new`
        // — `friendsrpc` deliberately exposes no `remote_factories()`, and `Stub::new`
        // with an empty factory list `anyhow::bail!`s in `register`.
        Box::new(remote::Stub::describe_peer(
            "friends",
            edge_peer(wiring, edge_list_resolver, "friends", "127.0.0.1:9014"),
        )),
        // groups is pure-HTTP from this front door's point of view (its player-facing
        // membership/invite ops, routes arriving via `__describe`); the gateway consumes
        // no groups capability of its own. `describe_peer`, never `Stub::new` — `groupsrpc`
        // deliberately exposes no `remote_factories()`, and `Stub::new` with an empty
        // factory list `anyhow::bail!`s in `register`.
        Box::new(remote::Stub::describe_peer(
            "groups",
            edge_peer(wiring, edge_list_resolver, "groups", "127.0.0.1:9015"),
        )),
    ]
}
