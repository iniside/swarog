//! The at-risk path the D2 in-process gateway tests MISSED: gateway-svc's REAL module set
//! (`gateway_svc::modules`) must actually BOOT — `register` (phase 1) then `init` (phase 2) —
//! without the zero-factory `Stub::register` bail killing it. The D2 wiring first built the
//! four route-provider stubs with `Stub::new(p, a, Vec::new())`; `Stub::register` bailed on the
//! empty factory list, so gateway-svc never started in the real split (build passed — build ≠
//! run; the `-p gateway` unit tests passed — they use in-process fakes, not `modules()`'s real
//! register path). This test drives the real two-phase build and asserts every `#[http]`
//! provider lands its `PEER_SLOT` entry, so the describe fetch reaches it.

use lifecycle::{Context, Module, ProcessWiring};

/// Build the real module set (standalone wiring: empty `ProcessWiring` → default peer
/// addresses, no edge-list resolver, no player edge), run register-all then init-all (the
/// lifecycle two-phase order), and assert:
///   1. every module's `register` SUCCEEDS (the zero-factory bail no longer fires on the
///      peer-only describe stubs), and
///   2. every `#[http]` provider contributed a `PEER_SLOT` entry — the entry the D2 describe
///      fetch iterates to reach that peer's `__describe`. The provider set is DERIVED from
///      `opscatalog::OPERATIONS` (itself generated from `api/*/api` and freshness-gated), so a
///      new provider joins this assertion by existing, not by being remembered here.
#[test]
fn gateway_svc_module_set_boots_and_every_http_provider_lands_in_peer_slot() {
    let wiring = ProcessWiring::new();
    let mods: Vec<Box<dyn Module>> = gateway_svc::modules(&wiring, None, None);
    let ctx = Context::new();

    // Phase 1 — register all. This is where `Stub::new(p, a, vec![])` used to bail and abort
    // gateway-svc boot; the peer-only `Stub::describe_peer` stubs must register cleanly.
    for m in &mods {
        m.register(&ctx)
            .unwrap_or_else(|e| panic!("register of module {:?} failed: {e:#}", m.name()));
    }
    // Phase 2 — init all (PEER_SLOT contribution + the gateway's verifier resolution, which
    // resolves the Sessions/Keys capabilities the accounts/apikeys stubs provided in phase 1).
    for m in &mods {
        m.init(&ctx)
            .unwrap_or_else(|e| panic!("init of module {:?} failed: {e:#}", m.name()));
    }

    let peers: Vec<opsapi::PeerAddr> = ctx.contributions(opsapi::PEER_SLOT);
    let providers: std::collections::BTreeSet<&str> =
        peers.iter().map(|p| p.provider.as_str()).collect();
    // PEER_SLOT is a superset of the `#[http]` set — capability-only stubs (apikeys, config)
    // contribute an entry without owning a route — so this is containment over a DERIVED set,
    // never equality against a literal.
    let http_providers: std::collections::BTreeSet<&str> = opscatalog::OPERATIONS
        .iter()
        .filter_map(|op| op.method.split_once('.').map(|(provider, _)| provider))
        .collect();
    assert!(
        !http_providers.is_empty(),
        "opscatalog::OPERATIONS yielded no providers; the derivation is broken and this \
         assertion would pass vacuously"
    );
    for http_provider in &http_providers {
        assert!(
            providers.contains(http_provider),
            "the #[http] provider {http_provider:?} must contribute a PEER_SLOT entry so the \
             describe fetch reaches it; got {providers:?}"
        );
    }
}
