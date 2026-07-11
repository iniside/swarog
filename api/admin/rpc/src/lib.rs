//! `adminrpc` — the GENERATED transport glue for the admin fan-out (the edge-
//! dependent half of `adminapi::AdminData`'s `#[rpc]` codegen). The `admin_data_rpc`
//! module is expanded from `adminapi`'s metadata-callback macro through
//! [`rpc_macro::generate_glue`] and contains the `Client` (implements
//! `adminapi::AdminData` over an [`opsapi::Caller`]), `register_server` (installs the
//! `admin.adminData` edge handler), and `provide_remote`.
//!
//! On top of the generated glue this crate adds:
//!   - [`register_admin`] — the server-side helper a provider registers on its edge;
//!     each provider's OWN `<name>rpc` re-exports it, so the provider MODULE never
//!     imports a foreign rpc crate (archcheck's module-consumer rule stays satisfied).
//!   - [`admin_remote_factory`] — the client-side [`remote::RemoteFactory`] a
//!     `remote::Stub` applies to contribute a REMOTE [`adminapi::Item`] whose
//!     `remote_fetch` hops the edge to the peer's `admin.adminData`. This is the
//!     deferred M2 admin fan-out (Go's `Stub.adminFetcher`), fed here as a boxed
//!     closure so `core/remote` stays `api/`-free.

use std::sync::Arc;

// The glue's method signatures re-resolve at THIS invocation site (the metadata
// travels as tokens), so `adminapi`'s domain types (`ItemData`) + the error type must
// be in scope here exactly as they are in `adminapi`'s trait.
use adminapi::*;
use opsapi::Error;

adminapi::admin_admin_data_meta!(rpc_macro::generate_glue);

/// Installs the `admin.adminData` edge handler for `svc` on `server` — the server
/// side of the fan-out. A provider registers this on its internal edge (via the
/// topology-blind `edge::EDGE_SLOT`) so a remote admin process can pull its page.
/// Providers reach it through their OWN `<name>rpc` re-export, never by importing
/// `adminrpc` directly.
pub fn register_admin(server: &mut edge::Server, svc: Arc<dyn AdminData>) {
    admin_data_rpc::register_server(server, svc);
}

/// Builds the client-side [`remote::RemoteFactory`] that contributes a REMOTE admin
/// [`adminapi::Item`] for `provider`. Applied by the owning `remote::Stub` in
/// `register`: it captures the stub's edge-backed [`opsapi::Caller`] and contributes
/// an `Item { id: provider, remote_fetch }` to [`adminapi::SLOT`], so this provider
/// still appears in a remote admin's sidebar — its Section/Label/Content fetched
/// lazily over the QUIC edge (no bespoke HTTP endpoint).
///
/// The fetch maps a peer with no admin surface (the edge's typed unknown-method
/// error, surfaced as [`opsapi::Status::NotFound`]) to [`adminapi::ItemError::Absent`]
/// so the admin drops the item silently; every other error propagates so the admin
/// shows an "unavailable" card (Go's `fetchAdmin`).
pub fn admin_remote_factory(provider: &str) -> remote::RemoteFactory {
    let provider = provider.to_string();
    Box::new(move |ctx, caller| {
        let provider = provider.clone();
        let fetch: adminapi::RemoteFetchFn = Arc::new(move |_params: adminapi::Params| {
            let caller = caller.clone();
            Box::pin(fetch_remote_admin(caller))
        });
        ctx.contribute(
            adminapi::SLOT,
            adminapi::Item {
                id: provider.clone(),
                section: String::new(),
                label: String::new(),
                render: None,
                remote_fetch: Some(fetch),
            },
        );
    })
}

/// One remote admin-data fetch over `caller`, mapping the outcome onto the admin
/// portal's tri-state: data, [`adminapi::ItemError::Absent`] (drop the item —
/// [`opsapi::Status::NotFound`], which is how `edge::Error::UnknownMethod` surfaces
/// through the transport when the peer has not registered an admin surface), or
/// [`adminapi::ItemError::Other`] (error card — peer down, timeout, any other
/// failure). Replaces Go's `strings.Contains(err.Error(), "unknown method")` sniff
/// with the typed status; the aliasing caveat (a domain NotFound would also read as
/// Absent) is accepted — `admin_data` has no domain not-found.
async fn fetch_remote_admin(
    caller: Arc<dyn opsapi::Caller>,
) -> Result<adminapi::ItemData, adminapi::ItemError> {
    let client = admin_data_rpc::Client::new(caller);
    match client.admin_data().await {
        Ok(data) => Ok(data),
        Err(e) if e.status == opsapi::Status::NotFound => Err(adminapi::ItemError::Absent),
        Err(e) => Err(adminapi::ItemError::Other(anyhow::anyhow!("{e}"))),
    }
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
