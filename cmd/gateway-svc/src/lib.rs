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

pub fn modules(
    wiring: &ProcessWiring,
    player: Option<Arc<Mutex<edge::PlayerServer>>>,
) -> Vec<Box<dyn Module>> {
    let mut gw = gateway::Gateway::new();
    if let Some(p) = player {
        gw = gw.with_player_edge(p);
    }
    for (prefix, origin) in wiring.passthrough() {
        gw = gw.with_passthrough(prefix, origin);
    }

    vec![
        Box::new(metrics::Metrics::new()), // core-infra: mounts GET /metrics + contributes the record layer
        Box::new(gw),
        // `remote` is generic (Step 4): this composition root injects each provider's
        // swap closures explicitly, so `remote` never names a provider.
        Box::new(remote::Stub::new(
            "characters",
            &wiring.peer_or("characters", "127.0.0.1:9000"),
            charactersrpc::remote_factories(),
        )),
        Box::new(remote::Stub::new(
            "inventory",
            &wiring.peer_or("inventory", "127.0.0.1:9001"),
            inventoryrpc::remote_factories(),
        )),
        Box::new(remote::Stub::new(
            "accounts",
            &wiring.peer_or("accounts", "127.0.0.1:9003"),
            accountsrpc::remote_factories(),
        )),
        Box::new(remote::Stub::new(
            "apikeys",
            &wiring.peer_or("apikeys", "127.0.0.1:9009"),
            apikeysrpc::remote_factories(),
        )),
        Box::new(remote::Stub::new(
            "match",
            &wiring.peer_or("match", "127.0.0.1:9006"),
            matchrpc::remote_factories(),
        )),
        Box::new(remote::Stub::new(
            "leaderboard",
            &wiring.peer_or("leaderboard", "127.0.0.1:9008"),
            leaderboardrpc::remote_factories(),
        )),
    ]
}
