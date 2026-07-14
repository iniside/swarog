//! `opscatalog` — the build-time generated catalog of every gateway-dispatched
//! (`#[http]`-bound) wire operation.
//!
//! ## Authority
//! [`OPERATIONS`] (in the generated [`mod generated`] via `include!`) is derived by
//! `tools/opscatalog-gen` from every `api/*/api` crate's generated
//! `<snake>_rpc::route_bindings()` — the SAME impl-free `opsapi::Operation` values the
//! gateway builds its route table from and `contract-golden` pins. There is NO
//! hand-maintained method list: a consumer (e.g. the apikeys role-policy editor's
//! checkbox source) reads this table, so its option set is exactly the served op surface.
//!
//! ## Freshness
//! The committed `src/generated.rs` is diffed against a fresh regeneration by the
//! `codegen-freshness` verify stage; any drift FAILs. Regenerate (the re-bless) with
//! `cargo run -p opscatalog-gen` — it overwrites this crate's `src/generated.rs`.
//!
//! ## Scope
//! ONLY api-gated operations belong here: methods carrying a `#[http(...)]` binding, which
//! the gateway dispatches from an HTTP route. Internal wire-only methods (e.g.
//! `apikeys.lookupKey`, `admin.adminData`) yield no `route_bindings()` entry and are
//! deliberately absent — they are not part of the player/operator-facing op vocabulary.

/// One gateway-dispatched operation, as a static row in [`OPERATIONS`]. Mirrors the
/// diffable fields of `opsapi::Operation` (method/verb/path/auth), flattened to
/// `&'static str` so this crate depends on nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OpInfo {
    /// The rpc method / wire name, e.g. `"leaderboard.topScores"`.
    pub method: &'static str,
    /// HTTP verb the gateway binds, e.g. `"GET"`.
    pub verb: &'static str,
    /// HTTP path pattern, e.g. `"/leaderboard"`.
    pub path: &'static str,
    /// Identity the gateway establishes before dispatch: `"none"` or `"player"`.
    pub auth: &'static str,
}

// The generated `pub const OPERATIONS: &[OpInfo]` table. `OpInfo` is in scope here, so the
// generated file stays a self-contained array literal.
include!("generated.rs");
