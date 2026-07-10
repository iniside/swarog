//! `configrpc` — the config domain's GENERATED transport glue plus the `CachedConfig`
//! remoting adapter (Step 5). It is the edge-dependent half of the `#[rpc]` codegen
//! for `configapi::ConfigSnapshot`, split out of the pure `configapi` contract.
//!
//! The `config_snapshot_rpc` module below is expanded from `configapi`'s
//! metadata-callback macro through [`rpc_macro::generate_glue`] and contains the
//! `Client` (the split-topology edge client implementing [`configapi::ConfigSnapshot`]
//! over an [`opsapi::Caller`]), `register_server` (installs the edge handler), and
//! `provide_remote`. It also `pub use`s the api crate's pure module.
//!
//! ## The `CachedConfig` remoting adapter
//! [`CachedConfig`] implements the SYNC [`configapi::Config`] reader trait over an
//! in-process `RwLock<HashMap>` cache, fed by the generated snapshot `Client`. In a
//! split process, [`remote_factories`] provides it under the SAME `config.reader`
//! registry key the config module uses locally, so a consumer's
//! `require::<dyn Config>` resolves without knowing the topology.
//!
//! ## Lifecycle (see [`remote_factories`])
//! The factory runs in [`remote::Stub`]'s phase-1 `register`: it builds the cache,
//! provides it, registers an authoritative-refresh callback on the `config_changed`
//! invalidation channel (Step 7 — replaces the old durable `"config-cache"`
//! subscription), and contributes a provider-tagged [`remote::RemoteBoot`] boot hook.
//! The `Stub` (whose `start` phase does exactly this) drains
//! [`remote::BOOT_SLOT`] in `start` and runs the boot hook once — a single `snapshot()`
//! that fails LOUD if config-svc is down (config is a hard dependency). The
//! invalidation plane's first refresh runs later still (after every module start), so
//! the boot guarantee does NOT degrade: "config-svc down at boot" stays a loud startup
//! failure via `RemoteBoot`; the invalidation callback only keeps the cache fresh
//! thereafter. No domain module is involved: the cache lives and boots entirely inside
//! the config `Stub`'s lifecycle.
//!
//! Rule 5: reached ONLY by the `config` module (its own glue, sanctioned), `remote`,
//! and `cmd/*` — never by a domain consumer (they import `configapi`, rule 4).

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

/// The invalidation channel config's write trigger `pg_notify`s and every config cache
/// LISTENs on. Shared vocabulary with `modules/config`'s `NOTIFY_CHANNEL`.
const CONFIG_CHANGED_CHANNEL: &str = "config_changed";

// The glue's method signatures re-resolve at THIS invocation site (the metadata
// travels as tokens), so the api crate's domain types + the error type must be in
// scope here exactly as they are in `configapi`'s lib.rs. `ConfigSnapshot` is
// wire-only (no `Identity` parameter), so `opsapi::Identity` is not imported.
use configapi::*;
use opsapi::Error;

configapi::config_config_snapshot_meta!(rpc_macro::generate_glue);

/// The admin fan-out's server-side registration, re-exported from `adminrpc` so the
/// `config` module registers `admin.adminData` through its OWN glue crate (never a
/// foreign rpc import — archcheck-clean).
pub use adminrpc::register_admin;

/// The atomically-swapped cache contents: the settings map plus the monotonic
/// `config.revision` it was read at. One `RwLock` over the pair makes the refresh's
/// revision gate race-free (a reader never sees a half-applied swap).
struct CacheState {
    /// `-1` before the first refresh; real revisions are ≥ 0, so the first snapshot
    /// always applies.
    revision: i64,
    map: HashMap<(String, String), String>,
}

/// A snapshot-backed [`configapi::Config`] reader for a split process: it holds a
/// generated [`config_snapshot_rpc::Client`] to config-svc and an in-process cache of
/// the last full snapshot. All getters read the cache (degrading to `default` on a
/// miss); the cache is boot-filled and refreshed by [`CachedConfig::refresh`].
pub struct CachedConfig {
    snapshot: Arc<dyn configapi::ConfigSnapshot>,
    cache: RwLock<CacheState>,
}

impl CachedConfig {
    /// Builds an empty cache over a snapshot client. The cache stays empty (every
    /// getter returns its `default`) until the first [`refresh`](CachedConfig::refresh)
    /// — which the config `Stub` runs at `start` before any consumer reads.
    pub fn new(snapshot: Arc<dyn configapi::ConfigSnapshot>) -> CachedConfig {
        CachedConfig {
            snapshot,
            cache: RwLock::new(CacheState {
                revision: -1,
                map: HashMap::new(),
            }),
        }
    }

    /// Pulls the FULL snapshot from config-svc and replaces the cache atomically IF the
    /// snapshot's revision is newer than the one held — so a stale or duplicate NOTIFY
    /// (or the redundant invalidation first-refresh after the boot-fill) is a no-op.
    /// Used both for the one boot-fill (`Stub::start`) and for every `config_changed`
    /// invalidation refresh — re-reading the whole snapshot is simplest and always
    /// consistent (the snapshot is the authoritative store read). An `Err` (peer down)
    /// propagates so the boot-fill fails loud and a refresh is logged + retried.
    pub async fn refresh(&self) -> Result<(), Error> {
        let snapshot = self.snapshot.snapshot().await?;
        let mut map = HashMap::with_capacity(snapshot.settings.len());
        for s in snapshot.settings {
            map.insert((s.namespace, s.key), s.value);
        }
        let mut guard = self.cache.write().unwrap();
        if snapshot.revision <= guard.revision {
            return Ok(());
        }
        guard.revision = snapshot.revision;
        guard.map = map;
        Ok(())
    }
}

/// The sync reader over the cache — byte-for-byte the same getter semantics as the
/// config module's own `Service` impl, so a consumer cannot tell a `CachedConfig`
/// (split) from the real service (monolith).
impl configapi::Config for CachedConfig {
    fn get_string(&self, ns: &str, key: &str, default: &str) -> String {
        self.get(ns, key).unwrap_or_else(|| default.to_string())
    }

    fn get_bool(&self, ns: &str, key: &str, default: bool) -> bool {
        match self.get(ns, key) {
            Some(v) => v == "1" || v.eq_ignore_ascii_case("true") || v.eq_ignore_ascii_case("on"),
            None => default,
        }
    }

    fn get_int(&self, ns: &str, key: &str, default: i64) -> i64 {
        match self.get(ns, key) {
            Some(v) => v.parse::<i64>().unwrap_or(default),
            None => default,
        }
    }

    fn get(&self, ns: &str, key: &str) -> Option<String> {
        self.cache
            .read()
            .unwrap()
            .map
            .get(&(ns.to_string(), key.to_string()))
            .cloned()
    }
}

/// The config provider's client-registration closures for a process where config
/// lives in a PEER (config-svc). Consumed by [`remote::Stub`]: the composition root
/// (`cmd/*`) passes `configrpc::remote_factories()` into `Stub::new`.
///
/// The single factory, run in the config `Stub`'s phase-1 `register`:
///   1. builds a [`CachedConfig`] over the generated snapshot `Client`,
///   2. `provide`s it under the SAME `config.reader` key the local config module uses,
///      so a co-hosted consumer's `require::<dyn Config>` resolves to the cache,
///   3. registers an authoritative-refresh callback on the `config_changed`
///      invalidation channel (Step 7 — replaces the old durable `"config-cache"`
///      subscription): every committed NOTIFY (from config-svc's write trigger, on the
///      shared DB) re-reads the snapshot over the edge and swaps the cache if newer,
///   4. contributes a provider-tagged [`remote::RemoteBoot`] whose boot the `Stub`
///      runs in `start` (one `snapshot()` boot-fill; fails loud if config-svc is down).
///
/// Config exposes NO front-door `#[http]` op (its only capability is wire-only
/// `ConfigSnapshot`), so — unlike characters/inventory — this contributes no route
/// bindings.
pub fn remote_factories() -> Vec<remote::RemoteFactory> {
    vec![Box::new(|ctx, caller| {
        let client: Arc<dyn configapi::ConfigSnapshot> =
            Arc::new(config_snapshot_rpc::Client::new(caller));
        let cached = Arc::new(CachedConfig::new(client));

        // (2) registry swap: the sync reader other modules require.
        ctx.registry().provide::<dyn configapi::Config>(
            registry::key("config", "reader"),
            cached.clone() as Arc<dyn configapi::Config>,
        );

        // (3) broadcast-invalidation REFRESH (Step 7): a committed `config_changed`
        // NOTIFY re-pulls the snapshot over the edge and applies it if newer. This is a
        // freshness callback, NOT a durable subscription — no checkpoint, so every
        // replica of this process refreshes independently (consumer-group semantics
        // would refresh only one). Wiring-only: the closure first runs at the
        // invalidation plane's start (after module start). A transport failure surfaces
        // as an error the plane logs, counts, and retries on the next NOTIFY/poll.
        let refresh = cached.clone();
        ctx.invalidation()
            .register(CONFIG_CHANGED_CHANNEL, "config-cache", move || {
                let refresh = refresh.clone();
                async move { refresh.refresh().await.map_err(anyhow::Error::from) }
            });

        // (4) boot-fill hook, run once by the Stub in `start` (fail loud if peer down) —
        // the boot guarantee, run BEFORE the invalidation plane's first refresh.
        let boot = cached.clone();
        ctx.contribute(
            remote::BOOT_SLOT,
            remote::RemoteBoot::new("config", move || {
                let boot = boot.clone();
                Box::pin(async move { boot.refresh().await.map_err(anyhow::Error::from) })
            }),
        );
    })]
}

#[cfg(test)]
mod tests;
