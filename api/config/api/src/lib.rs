//! `configapi` — the capability trait `config` exposes to other modules (the
//! interface-crate demanded by rule 4). Go's `config` consumers `Require` the
//! `"config"` service and downcast it to their OWN 1-method structural interface;
//! Rust's nominal traits can't do that, so the provider and every consumer share
//! THIS trait. Inventory (Step 9) will
//! `require::<dyn Config>(&registry::key("config", "reader"))`.
//!
//! Note the split: only the READ subset lives here. `set` stays on the concrete
//! `config` service (its own admin uses it), NOT on this trait — a reader depends
//! on a capability it needs (getters), nothing more.
//!
//! ## Remoting (Step 5): the `ConfigSnapshot` wire trait
//!
//! The sync `Config` trait above CANNOT ride the `#[rpc]` edge: it is synchronous,
//! non-`Result`, and callers rely on it being a cheap cached read. So remoting works
//! via a snapshot + durable-invalidation client. This crate additionally declares the
//! WIRE-ONLY `#[rpc]` trait [`ConfigSnapshot`] (`async fn snapshot() ->
//! Result<Vec<Setting>, Error>`, no `#[http]`): the config module implements it over
//! its store and exposes it on the internal edge, and the `configrpc` glue crate
//! wraps its generated `Client` in a `CachedConfig` adapter that implements the sync
//! `Config` trait over an in-process cache (boot-filled by one `snapshot()`, refreshed
//! on `config.changed`). Consumers keep calling the same sync `Config` trait; only the
//! registry swap differs between topologies.

use async_trait::async_trait;
use opsapi::Error;
use rpc_macro::rpc;
use serde::{Deserialize, Serialize};

/// One config setting on the wire — an element of a [`Snapshot`] reply. Field names
/// are the wire contract; evolve additively (constraint #6). The config module maps
/// its private row type to/from this at the edge boundary.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Setting {
    pub namespace: String,
    pub key: String,
    pub value: String,
}

/// A full config snapshot: every setting plus the monotonic `config.revision` the
/// store was at when they were read — both produced by ONE SQL statement (Step 7), so
/// the revision names exactly this set of settings. A `CachedConfig` applies a snapshot
/// only when its `revision` is newer than the one it already holds, so a stale or
/// duplicate refresh is a no-op.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub revision: i64,
    pub settings: Vec<Setting>,
}

/// The wire-only remoting capability (Step 5): returns a full [`Snapshot`] so a peer's
/// `CachedConfig` can (re)build its in-process cache. It is WIRE-ONLY — no leading
/// `Identity` (config is unauthenticated infrastructure) and no `#[http]` (not a
/// gateway route; it rides the internal mTLS edge like `characters.ownerOf`). The
/// transport-free surface is generated into `config_snapshot_rpc` here; the
/// edge-dependent `Client`/`register_server` live in the `configrpc` glue crate.
#[rpc(prefix = "config")]
#[async_trait]
pub trait ConfigSnapshot: Send + Sync {
    #[retry_safe]
    async fn snapshot(&self) -> Result<Snapshot, Error>;
}

/// The read-mostly config capability: namespaced `key=value` getters with a
/// code-default fallback, backed by an in-memory cache kept fresh by config's
/// listener. All getters degrade to `default` on a cache miss.
pub trait Config: Send + Sync {
    /// The cached value, or `default` on a miss.
    fn get_string(&self, ns: &str, key: &str, default: &str) -> String;

    /// Truthiness mirrors the repo's `envBool` (`"1"`/`"true"`/`"on"`, case-
    /// insensitive); a miss returns `default`.
    fn get_bool(&self, ns: &str, key: &str, default: bool) -> bool;

    /// Parses the cached value as an integer; a miss OR a parse error returns
    /// `default`.
    fn get_int(&self, ns: &str, key: &str, default: i64) -> i64;

    /// The raw cache lookup: `Some(value)` when present, else `None`.
    fn get(&self, ns: &str, key: &str) -> Option<String>;
}
