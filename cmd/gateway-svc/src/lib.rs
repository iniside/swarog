//! Library half of `gateway-svc`'s composition root (Step 10): the real module list,
//! extracted so `tools/checkmodules` can build the SAME set the process boots
//! without hand-mirroring it. `main.rs` resolves each peer edge address and
//! passthrough origin from env (unchanged defaults) into a [`ProcessWiring`]; the
//! checker harness builds an empty `ProcessWiring` and gets the same peer defaults
//! back from [`ProcessWiring::peer_or`] — `register`/`init` do no I/O, so a dummy
//! peer address is safe.
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
/// gateway route table reads is `wiring.peer_or` in BOTH modes (resolved once at start).
/// In MANAGED mode `edge_resolver` is `Some`, so the stub's reconnecting caller also
/// re-resolves on every dial (A5 — a moved peer is picked up with no restart); in
/// STANDALONE mode it is `None`, giving a constant resolver and byte-identical boot.
fn edge_peer(
    wiring: &ProcessWiring,
    edge_resolver: Option<&dyn Fn(&'static str) -> remote::PeerResolver>,
    provider: &'static str,
    default: &str,
) -> remote::PeerSource {
    let boot = wiring.peer_or(provider, default);
    match edge_resolver {
        Some(make) => remote::PeerSource::resolving(boot, make(provider)),
        None => remote::PeerSource::fixed(boot),
    }
}

pub fn modules(
    wiring: &ProcessWiring,
    player: Option<Arc<Mutex<edge::PlayerServer>>>,
    // `Some` only in managed boot (`main.rs` owns `addrs::edge_resolver`, bound to the
    // agent URL): each edge stub then re-resolves its peer on reconnect. `None` in
    // standalone and in the `checkmodules` harness — constant resolvers, no re-resolve.
    edge_resolver: Option<&dyn Fn(&'static str) -> remote::PeerResolver>,
) -> Vec<Box<dyn Module>> {
    let mut gw = gateway::Gateway::new();
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

    vec![
        Box::new(metrics::Metrics::new()), // core-infra: mounts GET /metrics + contributes the record layer
        Box::new(gw),
        // `remote` is generic (Step 4): this composition root injects each provider's
        // swap closures explicitly, so `remote` never names a provider.
        Box::new(remote::Stub::new(
            "characters",
            edge_peer(wiring, edge_resolver, "characters", "127.0.0.1:9000"),
            charactersrpc::remote_factories(),
        )),
        Box::new(remote::Stub::new(
            "inventory",
            edge_peer(wiring, edge_resolver, "inventory", "127.0.0.1:9001"),
            inventoryrpc::remote_factories(),
        )),
        Box::new(remote::Stub::new(
            "accounts",
            edge_peer(wiring, edge_resolver, "accounts", "127.0.0.1:9003"),
            accountsrpc::remote_factories(),
        )),
        Box::new(remote::Stub::new(
            "apikeys",
            edge_peer(wiring, edge_resolver, "apikeys", "127.0.0.1:9009"),
            apikeysrpc::remote_factories(),
        )),
        Box::new(remote::Stub::new(
            "match",
            edge_peer(wiring, edge_resolver, "match", "127.0.0.1:9006"),
            matchrpc::remote_factories(),
        )),
        Box::new(remote::Stub::new(
            "leaderboard",
            edge_peer(wiring, edge_resolver, "leaderboard", "127.0.0.1:9008"),
            leaderboardrpc::remote_factories(),
        )),
    ]
}
