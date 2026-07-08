//! `inventoryrpc` — the inventory domain's GENERATED transport glue (port of Go's
//! `api/inventory/inventoryrpc`): the edge-dependent half of the `#[rpc]` codegen,
//! split out of the pure `inventoryapi` contract in the Step-2 fortress refactor.
//!
//! The `holdings_rpc` module below is expanded from `inventoryapi`'s
//! metadata-callback macro through [`rpc_macro::generate_glue`] and contains the
//! `Client` (the split-topology edge client implementing [`Holdings`] over an
//! [`opsapi::Caller`]), `register_server` (installs one `edge::IdentityHandler`
//! adapter per method), and `provide_remote` (provides the `Client` under
//! `inventory.holdings`). It also `pub use`s the api crate's pure module, so
//! `inventoryrpc::holdings_rpc::*` is a drop-in superset of the old fused module.
//!
//! Rule 5: this crate is reached ONLY by the `inventory` module itself (a module
//! importing its OWN glue is sanctioned), `remote`, and `cmd/*` binaries — never by
//! a domain consumer (they import `inventoryapi`, rule 4).

use std::sync::Arc;

// The glue's method signatures re-resolve at THIS invocation site (the metadata
// travels as tokens), so the api crate's domain types + the identity/error types
// must be in scope here exactly as they are in `inventoryapi`'s lib.rs.
use inventoryapi::*;
use opsapi::{Error, Identity};

inventoryapi::inventory_holdings_meta!(rpc_macro::generate_glue);

/// One provider-swap action (the Step-4 generic-`remote` seam): applied to the
/// process [`lifecycle::Context`] and the stub's edge-backed [`opsapi::Caller`], it
/// provides a generated capability `Client` and/or contributes front-door route
/// bindings — exactly what `remote`'s per-provider match arm used to hand-write.
pub type RemoteFactory = Box<dyn Fn(&lifecycle::Context, Arc<dyn opsapi::Caller>) + Send + Sync>;

/// The inventory provider's client-registration closures for a process where the
/// provider lives in a PEER process (consumed by `remote::Stub`; Step 4 will pass
/// them in from `cmd/*`). Inventory is a LEAF — no peer `require`s an inventory
/// capability — so this contributes the holdings `route_bindings()` ONLY (front-door
/// routing; no `LOCAL_SLOT`, the gateway dispatches remotely) and deliberately makes
/// no capability provide: a dead provide is noise, add one when a consumer appears.
pub fn remote_factories() -> Vec<RemoteFactory> {
    vec![Box::new(|ctx, _caller| {
        for rb in holdings_rpc::route_bindings() {
            ctx.contribute(opsapi::SLOT, rb.operation);
            ctx.contribute(opsapi::BINDING_SLOT, rb.binding);
        }
    })]
}
