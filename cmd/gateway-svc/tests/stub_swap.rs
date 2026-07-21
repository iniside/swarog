//! Integration coverage for the generic `remote::Stub` wired with the REAL provider
//! glue factories (`<name>rpc::remote_factories()`). These moved out of `core/remote`
//! in Step 4: `remote` is now a leaf that imports no `api/` crate, so the swap that
//! depends on `charactersapi`/`inventoryapi` + the glue lives here, in a `cmd/*` crate
//! that legitimately depends on both glue crates (rule 5). The generic factory-running
//! behavior is unit-tested with local fakes in `core/remote/src/tests.rs`; this proves
//! the real factories provide the right capability keys and route bindings.

use lifecycle::{Context, Module};

/// The `"characters"` stub, built with `charactersrpc::remote_factories()`, provides
/// BOTH capability keys the local impl would — downcastable to the exact trait objects
/// a consumer / the gateway `require`s — with no QUIC dial (register is lazy).
#[test]
fn characters_stub_provides_capability_keys() {
    let ctx = Context::new(); // DB-less: register only touches the registry
    let stub = remote::Stub::new("characters", "127.0.0.1:9000", charactersrpc::remote_factories());
    assert_eq!(stub.name(), "characters", "name is the PROVIDER name for validate_requires");
    assert!(stub.requires().is_empty());
    stub.register(&ctx).unwrap();

    assert!(
        ctx.registry()
            .try_require::<dyn charactersapi::Ownership>(&registry::key("characters", "ownership"))
            .is_some(),
        "characters.ownership must resolve to an Arc<dyn Ownership> (inventory's authz dep)"
    );
    assert!(
        ctx.registry()
            .try_require::<dyn charactersapi::Player>(&registry::key("characters", "player"))
            .is_some(),
        "characters.player must resolve to an Arc<dyn Player> (front-door dep)"
    );
}

/// The `"characters"` stub contributes the player ops' `Operation`+`OpBinding` to the
/// two route-table slots (matching the generated `route_bindings()`), and NOTHING to
/// `LOCAL_SLOT` — so the gateway resolves them Remote, over the edge.
#[test]
fn characters_stub_contributes_route_bindings_but_no_local() {
    let ctx = Context::new();
    remote::Stub::new("characters", "127.0.0.1:9000", charactersrpc::remote_factories())
        .register(&ctx)
        .unwrap();

    let expected: Vec<String> = charactersapi::player_rpc::route_bindings()
        .into_iter()
        .map(|rb| rb.operation.method)
        .collect();
    assert!(!expected.is_empty(), "player has #[http] ops to contribute");

    let ops: Vec<opsapi::Operation> = ctx.contributions(opsapi::SLOT);
    let bindings: Vec<opsapi::OpBinding> = ctx.contributions(opsapi::BINDING_SLOT);
    let locals: Vec<opsapi::LocalOp> = ctx.contributions(opsapi::LOCAL_SLOT);

    assert_eq!(
        ops.iter().map(|o| o.method.clone()).collect::<Vec<_>>(),
        expected,
        "SLOT carries exactly the player route Operations"
    );
    assert_eq!(
        bindings.iter().map(|b| b.method.clone()).collect::<Vec<_>>(),
        expected,
        "BINDING_SLOT carries the matching OpBindings"
    );
    assert!(locals.is_empty(), "no LocalOp — the stub has no in-process invoker");
}

/// The `"inventory"` stub, built with `inventoryrpc::remote_factories()`, contributes
/// route bindings ONLY: SLOT/BINDING_SLOT carry the holdings ops, LOCAL_SLOT is empty,
/// and — because inventory is a leaf — the registry has NO inventory capability provide.
#[test]
fn inventory_stub_is_routes_only_no_capability() {
    let ctx = Context::new();
    remote::Stub::new("inventory", "127.0.0.1:9001", inventoryrpc::remote_factories())
        .register(&ctx)
        .unwrap();

    let expected: Vec<String> = inventoryapi::holdings_rpc::route_bindings()
        .into_iter()
        .map(|rb| rb.operation.method)
        .collect();
    assert!(!expected.is_empty(), "holdings has #[http] ops to contribute");

    let ops: Vec<opsapi::Operation> = ctx.contributions(opsapi::SLOT);
    let bindings: Vec<opsapi::OpBinding> = ctx.contributions(opsapi::BINDING_SLOT);
    let locals: Vec<opsapi::LocalOp> = ctx.contributions(opsapi::LOCAL_SLOT);

    assert_eq!(
        ops.iter().map(|o| o.method.clone()).collect::<Vec<_>>(),
        expected,
        "SLOT carries exactly the holdings route Operations"
    );
    assert_eq!(bindings.len(), expected.len(), "BINDING_SLOT matches SLOT");
    assert!(locals.is_empty(), "no LocalOp for a routes-only stub");
}

/// After `init`, a stub contributes its peer edge address to `opsapi::PEER_SLOT`, so a
/// co-hosted gateway front door resolves this provider's Remote ops to that address
/// WITHOUT reading env — the topology the composition root injected via `Stub::new`.
/// This is the seam that replaced the module's `{PROVIDER}_EDGE_ADDR` env lookup.
#[test]
fn stub_contributes_peer_addr_for_remote_dispatch() {
    let ctx = Context::new();
    let stub = remote::Stub::new("characters", "127.0.0.1:9000", charactersrpc::remote_factories());
    stub.register(&ctx).unwrap();
    stub.init(&ctx).unwrap();

    let peers: Vec<opsapi::PeerAddr> = ctx.contributions(opsapi::PEER_SLOT);
    let found = peers
        .iter()
        .find(|p| p.provider == "characters")
        .expect("characters peer address contributed to PEER_SLOT");
    assert_eq!(
        found.addrs,
        vec!["127.0.0.1:9000".to_string()],
        "the address set the gateway dials Remote (one element in the single-address phase)"
    );
}
