//! Fleet manifest — the single deciding place for WHAT the game-backend fleet
//! is: process names, ports, boot order, and per-process env. This is a
//! faithful, config-as-code PORT of `tools/processctl/src/fleet.rs`'s
//! `game_backend_fleet`/`game_backend_monolith` (Development flavor) —
//! copied, not imported (weles is zero-sharing: it may never depend on a
//! workspace crate). Nothing else in `weles` may hardcode a port or env
//! name; every other module reads the manifest.
//!
//! The `Vec` returned by [`split_fleet`] IS the boot order — dependencies
//! are expressed implicitly by position, matching `fleet.rs`'s
//! `dependencies` ordering constraint without needing a separate graph here.
//! Precisely: every [`AddrKind::Edge`] entry in a service's
//! [`ServiceDef::peers`] appears strictly EARLIER in the Vec. An
//! [`AddrKind::Http`] entry carries NO such constraint — it is a passthrough
//! ORIGIN, dialed per request rather than at boot. Pinned by
//! `boot_order_respects_edge_peer_dependencies`, derived from the `peers`
//! field rather than hand-listed beside it.
//!
//! **Recorded semantic change (M1 Step 4):** gateway-svc is now
//! [`ServiceDef::managed`] — it asks the agent where its peers are instead of
//! being told by env — so it declares NO `peers`, and with that the fleet lost
//! six Edge entries and its ONLY two `AddrKind::Http` entries. Two
//! consequences, neither hidden: the boot-order rule no longer constrains
//! gateway-svc from its own declaration (its position in the Vec is unchanged,
//! so the booted order is not), and the Http-vs-Edge asymmetry has no live
//! example left in the real fleet. The tests that pinned both ON GATEWAY'S DATA
//! (`an_http_peer_carries_no_boot_order_constraint`, the real-fleet half of
//! `the_two_kinds_read_different_port_fields`, the gateway env golden, the
//! `checked == 17` count) are RED pending the plan's Step 7, which owns
//! re-pointing them and the `weles-fleet-parity` exclusion.
//!
//! Deliberate semantic delta vs the fleet.rs Development flavor: weles's
//! composed env is fully deterministic — the `overrideable_env` seam
//! (`tools/processctl/src/fleet.rs:568-584`, which lets ambient
//! `SCHEDULER_ENABLED`/`ACCOUNTS_DEV_AUTH`/`ADMIN_COOKIE_SECURE`/… override
//! the manifest) was consciously NOT ported, per the config-as-code
//! decision: what a service gets is exactly what this file says, plus the
//! fixed [`SERVICE_ENV_ALLOWLIST`] passthrough.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Parent-process env vars a spawned service may inherit. Mirrors
/// `tools/processctl/src/fleet.rs::SERVICE_ENV_ALLOWLIST` exactly — topology
/// wiring and bind addresses are never inherited, only ambient
/// interpreter/toolchain plumbing.
pub const SERVICE_ENV_ALLOWLIST: &[&str] = &[
    "COMSPEC",
    "HOME",
    "PATH",
    "PATHEXT",
    "RUST_BACKTRACE",
    "RUST_LOG",
    "SYSTEMROOT",
    "TEMP",
    "TMP",
    "USERPROFILE",
    "WINDIR",
];

/// The loopback port weles's own agent HTTP endpoint ([`crate::agentapi`])
/// binds. This file is the ONE place in weles allowed to write a port (see the
/// module doc), which is why the agent's port lives here rather than beside the
/// server that binds it: a runtime-minted port could not be handed to a service
/// through the `'static` [`ServiceDef::env_extra`], and a second port-writing
/// site would be a second authority for "where does the fleet listen".
///
/// Deliberately clear of every port claimed anywhere in this repo: the fleet's
/// HTTP range (8080..=8091, plus headroom for new services), the edge range
/// (9000..=9009) and the player plane (9100..=9101), the metrics-shaped 9090,
/// and — the one that bit — **8099, which `tools/verifyctl`'s C# fixture server
/// binds** (`stages/csharp.rs`, `docs/reference/csharp-client.md`). That is not
/// a live race (both hold `run/rollout.lock`), but sharing it means a leftover
/// fixture makes `weles up` die naming the wrong culprit, and vice versa.
///
/// Two tests pin this, because neither alone can: this crate's
/// `agent_port_collides_with_no_fleet_port` derives the FLEET's ports from the
/// manifest — but weles can only ever see its own fleet, which is the one place
/// this port was never going to collide. The cross-tool half lives in
/// verifyctl's `weles-async-island` stage, which can see both constants.
pub const AGENT_PORT: u16 = 8300;

/// The env key a [`ServiceDef::managed`] process is handed the agent's URL
/// through. `cmd/gateway-svc`'s main reads exactly this name (its own copy —
/// zero-sharing), and the VALUE is derived from [`AGENT_PORT`] by [`agent_url`],
/// never written beside it.
const ORCHESTRATOR_URL_ENV: &str = "ORCHESTRATOR_URL";

/// Where a managed service reaches the agent. Derived from [`AGENT_PORT`] — the
/// one authority for the agent's port — rather than spelled as a literal in
/// [`ServiceDef::env_extra`], which would be the same fact twice and free to
/// drift the day the port moves. It is also why this cannot BE an `env_extra`
/// entry: those are `&'static str` pairs, and this value is built from a number.
///
/// The host is `127.0.0.1` by the same construction [`service_addr`] relies on:
/// [`crate::agentapi::AgentServer::bind`] binds loopback, so the URL handed to a
/// service is the address that endpoint actually took.
fn agent_url() -> String {
    format!("http://127.0.0.1:{AGENT_PORT}")
}

/// Which of a provider's two port fields a peer address is formatted from.
///
/// This is a FIELD on every [`ServiceDef::peers`] entry, never inferred from
/// the env key's spelling: `ADDR`-suffix guessing would make the env KEY the
/// authority for where a service lives, which is the exact inversion the
/// `peers` seam exists to kill. `accounts` is dialed as BOTH kinds
/// (`ACCOUNTS_EDGE_ADDR` → 9003, `ACCOUNTS_HTTP_ADDR` → 8084), so the two
/// classes are not a property of the provider either.
///
/// This is ALSO the `kind` on the agent's `resolve` wire ([`crate::agentapi`]):
/// the serde derive here is what keeps the spelling (`"edge"`/`"http"`) an
/// attribute of this one enum rather than a `match` on strings beside it. A
/// wire-only twin would be exactly the second discriminator this type exists to
/// prevent, and it would be free to drift.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AddrKind {
    /// The provider's internal mTLS QUIC edge — [`ServiceDef::edge_port`].
    Edge,
    /// The provider's HTTP surface (passthrough origin) —
    /// [`ServiceDef::http_port`].
    Http,
}

/// A single fleet process: its identity, ports, whether it owns a Postgres
/// pool, the peers it dials, and the env pairs unique to it (dev-mode
/// opt-ins, own-process config).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServiceDef {
    pub name: &'static str,
    pub pkg: &'static str,
    /// The SHORT domain name this process is dialed by — the one name the
    /// wire and the service registry already use (`remote::Stub::new(
    /// "characters", …)`, which archcheck rule 17 text-scans; the
    /// `modules/<name>` / `api/<name>` directory). [`ServiceDef::peers`] and
    /// the future `resolve` verb both key on THIS, so the manifest, the
    /// resolve map and the wire share one naming authority — rather than a
    /// `strip_suffix("-svc")` rule reconstructing it from `name`, which would
    /// make a string convention the third authority (the same inversion
    /// [`AddrKind`]-as-a-field exists to avoid).
    ///
    /// `None` where no single short name is truthful: the monolith hosts
    /// EVERY domain in one process, so it is nameable as none of them. That
    /// is data, not an accident — it is why the monolith is structurally
    /// unresolvable as a peer.
    pub provider: Option<&'static str>,
    pub http_port: u16,
    pub edge_port: Option<u16>,
    pub player_port: Option<u16>,
    pub has_db: bool,
    /// `DATABASE_POOL_MAX_CONNECTIONS` for this process. Ignored when
    /// `has_db` is false (gateway-svc: pure-transport, no pool).
    pub pool_max: u32,
    /// Peer addresses this process is handed, as `(env key, provider, kind)`.
    /// The provider is another entry's [`ServiceDef::provider`] short name;
    /// the address is DERIVED in [`compose_env`] from that entry's port
    /// field, so the port declaration is the one authority for "where is X"
    /// and a port change propagates to every consumer by construction.
    ///
    /// Never write an address literal here or in [`ServiceDef::env_extra`] —
    /// that is the two-authorities drift this field replaced.
    pub peers: &'static [(&'static str, &'static str, AddrKind)],
    /// Whether this process ASKS the agent where its peers are instead of being
    /// told by env: [`compose_env`] hands a managed service
    /// [`ORCHESTRATOR_URL_ENV`] (derived from [`AGENT_PORT`]), and it resolves
    /// its own addresses over [`crate::agentapi`]'s `resolve`.
    ///
    /// A managed service therefore declares NO [`ServiceDef::peers`], and the
    /// two are mutually exclusive BY TEST (`managed_services_declare_no_peers`),
    /// not by convention: composing peer env for a process that resolves would
    /// put two authorities in one environment — and since the consumer
    /// (`cmd/gateway-svc`) reads exactly one source per mode and never both, the
    /// dead half would drift unnoticed until someone believed it.
    ///
    /// This is data about the process, not a topology branch: a managed
    /// service's addresses come from the SAME booting fleet
    /// ([`PeerAddrs::from_fleet`]) that [`compose_env_with_fleet`] would have
    /// composed them from, so the two sources cannot disagree — only the moment
    /// the service learns them differs.
    pub managed: bool,
    /// Literal, address-free env: dev-mode opt-ins and this process's own
    /// config (`TLS_MODE`, `PLAYER_EDGE_ADDR` — its own bind, not a peer's).
    ///
    /// Keys here are DISJOINT from [`ServiceDef::peers`] keys: `env_extra` is
    /// applied last and would silently override a derived address (pinned by
    /// `no_env_extra_key_shadows_a_derived_peer_key`).
    pub env_extra: &'static [(&'static str, &'static str)],
}

/// Runtime values only known at supervisor start (never hardcoded here):
/// the local Postgres DSN and the mTLS CA material path.
pub struct RuntimeInputs {
    pub database_url: String,
    pub ca_cert: PathBuf,
    pub ca_key: PathBuf,
}

/// Per-DB-process pooled-connection cap in the split. Mirrors
/// `tools/processctl/src/fleet.rs::SPLIT_SERVICE_POOL_MAX`.
const SPLIT_SERVICE_POOL_MAX: u32 = 3;

/// Pooled-connection cap for the monolith. Mirrors
/// `tools/processctl/src/fleet.rs::MONOLITH_POOL_MAX`.
const MONOLITH_POOL_MAX: u32 = 20;

/// The 12-process split fleet, in boot order. Each dependency (a peer a
/// service dials over the internal mTLS edge) appears strictly earlier in
/// this list than its dependent, matching
/// `tools/processctl/src/fleet.rs::game_backend_fleet`'s `dependencies`
/// constraint by construction.
pub fn split_fleet() -> Vec<ServiceDef> {
    vec![
        ServiceDef {
            name: "accounts-svc",
            pkg: "accounts-svc",
            provider: Some("accounts"),
            http_port: 8084,
            edge_port: Some(9003),
            player_port: None,
            has_db: true,
            pool_max: SPLIT_SERVICE_POOL_MAX,
            managed: false,
            peers: &[],
            env_extra: &[("ACCOUNTS_DEV_AUTH", "1")],
        },
        ServiceDef {
            name: "apikeys-svc",
            pkg: "apikeys-svc",
            provider: Some("apikeys"),
            http_port: 8091,
            edge_port: Some(9009),
            player_port: None,
            has_db: true,
            pool_max: SPLIT_SERVICE_POOL_MAX,
            managed: false,
            peers: &[],
            env_extra: &[("APIKEYS_DEV_SEED", "1")],
        },
        ServiceDef {
            name: "audit-svc",
            pkg: "audit-svc",
            provider: Some("audit"),
            http_port: 8086,
            edge_port: Some(9004),
            player_port: None,
            has_db: true,
            pool_max: SPLIT_SERVICE_POOL_MAX,
            managed: false,
            peers: &[],
            env_extra: &[],
        },
        ServiceDef {
            name: "scheduler-svc",
            pkg: "scheduler-svc",
            provider: Some("scheduler"),
            http_port: 8087,
            edge_port: Some(9005),
            player_port: None,
            has_db: true,
            pool_max: SPLIT_SERVICE_POOL_MAX,
            // Deliberately NO SCHEDULER_ENABLED here — this manifest is the
            // Development flavor of tools/processctl/src/fleet.rs, which
            // only sets SCHEDULER_ENABLED under FleetFlavor::Proof.
            managed: false,
            peers: &[],
            env_extra: &[],
        },
        ServiceDef {
            name: "rating-svc",
            pkg: "rating-svc",
            provider: Some("rating"),
            http_port: 8089,
            edge_port: Some(9007),
            player_port: None,
            has_db: true,
            pool_max: SPLIT_SERVICE_POOL_MAX,
            managed: false,
            peers: &[],
            env_extra: &[],
        },
        ServiceDef {
            name: "leaderboard-svc",
            pkg: "leaderboard-svc",
            provider: Some("leaderboard"),
            http_port: 8090,
            edge_port: Some(9008),
            player_port: None,
            has_db: true,
            pool_max: SPLIT_SERVICE_POOL_MAX,
            managed: false,
            peers: &[],
            env_extra: &[],
        },
        ServiceDef {
            name: "match-svc",
            pkg: "match-svc",
            provider: Some("match"),
            http_port: 8088,
            edge_port: Some(9006),
            player_port: None,
            has_db: true,
            pool_max: SPLIT_SERVICE_POOL_MAX,
            managed: false,
            peers: &[("RATING_EDGE_ADDR", "rating", AddrKind::Edge)],
            env_extra: &[],
        },
        ServiceDef {
            name: "config-svc",
            pkg: "config-svc",
            provider: Some("config"),
            http_port: 8083,
            edge_port: Some(9002),
            player_port: None,
            has_db: true,
            pool_max: SPLIT_SERVICE_POOL_MAX,
            managed: false,
            peers: &[],
            env_extra: &[],
        },
        ServiceDef {
            name: "characters-svc",
            pkg: "characters-svc",
            provider: Some("characters"),
            http_port: 8080,
            edge_port: Some(9000),
            player_port: None,
            has_db: true,
            pool_max: SPLIT_SERVICE_POOL_MAX,
            managed: false,
            peers: &[("CONFIG_EDGE_ADDR", "config", AddrKind::Edge)],
            env_extra: &[],
        },
        ServiceDef {
            name: "inventory-svc",
            pkg: "inventory-svc",
            provider: Some("inventory"),
            http_port: 8081,
            edge_port: Some(9001),
            player_port: None,
            has_db: true,
            pool_max: SPLIT_SERVICE_POOL_MAX,
            managed: false,
            peers: &[
                ("CHARACTERS_EDGE_ADDR", "characters", AddrKind::Edge),
                ("CONFIG_EDGE_ADDR", "config", AddrKind::Edge),
            ],
            env_extra: &[("INVENTORY_DEV_GRANT", "1")],
        },
        ServiceDef {
            name: "gateway-svc",
            pkg: "gateway-svc",
            provider: Some("gateway"),
            http_port: 8082,
            edge_port: None,
            player_port: Some(9100),
            has_db: false,
            pool_max: 0,
            // M1's first managed process, and the natural first: it calls
            // peers, and nobody calls it (it serves no edge), so it can start
            // resolving without forcing another service to move. Instead of
            // the eight address keys it used to carry — six `*_EDGE_ADDR` plus
            // the two passthrough ORIGINS (`ADMIN_HTTP_ADDR`,
            // `ACCOUNTS_HTTP_ADDR`: admin-svc has no edge at all, and
            // accounts-svc is dialed as both kinds) — it is handed
            // ORCHESTRATOR_URL and asks the agent for each of the eight.
            managed: true,
            peers: &[],
            // PLAYER_EDGE_ADDR is this process's OWN player-plane bind, not a
            // peer's address — it stays a literal.
            env_extra: &[("PLAYER_EDGE_ADDR", ":9100"), ("TLS_MODE", "off")],
        },
        ServiceDef {
            name: "admin-svc",
            pkg: "admin-svc",
            provider: Some("admin"),
            http_port: 8085,
            edge_port: None,
            player_port: None,
            has_db: true,
            pool_max: SPLIT_SERVICE_POOL_MAX,
            managed: false,
            peers: &[
                ("CHARACTERS_EDGE_ADDR", "characters", AddrKind::Edge),
                ("INVENTORY_EDGE_ADDR", "inventory", AddrKind::Edge),
                ("CONFIG_EDGE_ADDR", "config", AddrKind::Edge),
                ("ACCOUNTS_EDGE_ADDR", "accounts", AddrKind::Edge),
                ("AUDIT_EDGE_ADDR", "audit", AddrKind::Edge),
                ("SCHEDULER_EDGE_ADDR", "scheduler", AddrKind::Edge),
                ("APIKEYS_EDGE_ADDR", "apikeys", AddrKind::Edge),
            ],
            env_extra: &[
                ("ADMIN_COOKIE_SECURE", "0"),
                ("TRUSTED_PROXY_CIDRS", "127.0.0.1/32"),
            ],
        },
    ]
}

/// The single-process monolith topology (`cmd/server`, package `server`).
pub fn monolith() -> ServiceDef {
    ServiceDef {
        name: "server",
        pkg: "server",
        provider: None,
        http_port: 8080,
        edge_port: None,
        player_port: Some(9100),
        has_db: true,
        pool_max: MONOLITH_POOL_MAX,
        // One process hosts every module: there are no peers to dial, so the
        // monolith is trivially free of derived addresses.
        managed: false,
        peers: &[],
        env_extra: &[
            ("PLAYER_EDGE_ADDR", ":9100"),
            ("APIKEYS_DEV_SEED", "1"),
            ("ACCOUNTS_DEV_AUTH", "1"),
            ("INVENTORY_DEV_GRANT", "1"),
            ("TLS_MODE", "off"),
            ("ADMIN_COOKIE_SECURE", "0"),
            ("TRUSTED_PROXY_CIDRS", "127.0.0.1/32"),
        ],
    }
}

/// THE address formatter: `def`'s own address of `kind`, or `None` where `def`
/// has no such address (`edge_port: None` — admin-svc serves no internal edge).
///
/// Every caller that answers "where does this service listen" goes through
/// here, taking the def IN HAND rather than a name to look up again: a
/// `format!("127.0.0.1:{port}")` anywhere else is a second authority, and a
/// re-lookup by name is a chance to format the WRONG def's port (see
/// [`PeerAddrs::from_fleet`], which must not re-find what it already holds).
///
/// Every fleet process binds loopback (`PORT`/`EDGE_ADDR` are `:<port>`), so
/// the host is `127.0.0.1` by construction, not per-service data.
fn service_addr(def: &ServiceDef, kind: AddrKind) -> Option<String> {
    let port = match kind {
        AddrKind::Edge => def.edge_port?,
        AddrKind::Http => def.http_port,
    };
    Some(format!("127.0.0.1:{port}"))
}

/// Formats one peer address for a CONSUMER that names a provider: the lookup by
/// name is the point here — a consumer knows only the short name it declared.
///
/// PANICS, naming the offender, on an unknown provider or `Edge` against a
/// service with no edge — both are programmer errors committed while adding a
/// service to this file, and the manifest follows the repo's "duplicate
/// registration PANICs" convention: a wiring mistake is a loud boot failure,
/// never a silently wrong address that surfaces as a peer that isn't there.
fn peer_addr(fleet: &[ServiceDef], consumer: &str, provider: &str, kind: AddrKind) -> String {
    let def = fleet.iter().find(|svc| svc.provider == Some(provider)).unwrap_or_else(|| {
        panic!(
            "fleet manifest: {consumer} declares peer {provider:?}, which no service in \
             this fleet provides"
        )
    });
    service_addr(def, kind).unwrap_or_else(|| {
        // `AddrKind::{kind:?}` names the Rust variant, not the wire spelling:
        // this is a programmer error in this file, read by whoever is editing
        // it. (`edge_kind_against_real_admin_svc_panics` pins this wording.)
        panic!(
            "fleet manifest: {consumer} declares peer {provider:?} as AddrKind::{kind:?}, \
             but {provider} has edge_port: None (it serves no internal edge)"
        )
    })
}

/// Every address the agent can answer a `resolve` with, for ONE booting
/// topology: `(provider, kind) -> addresses`.
///
/// # Why this is derived, and derived HERE
///
/// It is built from the SAME [`ServiceDef`] slice the supervisor hands
/// [`compose_env_with_fleet`], and each address is formatted by the SAME
/// [`service_addr`] — so "where is characters" has one authority whether a
/// service is told by env or asks over the wire. A map assembled anywhere else
/// (a second `format!("127.0.0.1:{}")`, a lookup keyed off `name`) would be a
/// second authority whose first drift from the composed env is invisible.
///
/// # Why the monolith is empty, without a topology `if`
///
/// The map is keyed on [`ServiceDef::provider`], which is `None` for the
/// monolith: one process hosting all 12 domains is nameable as none of them. So
/// `PeerAddrs::from_fleet(&[monolith()])` is EMPTY and every `resolve` under the
/// monolith 404s — a property of the data, not a branch. That is the correct
/// answer, not a degradation: a monolith has no peers to resolve
/// (`weles-design.md`, "the monolith satisfies this trivially"). A map built
/// from `split_fleet()` regardless of topology would instead hand out addresses
/// for twelve processes that are not running.
///
/// # Shape
///
/// [`PeerAddrs::lookup`] returns a LIST because the design's `resolve` returns
/// *all live instances* (round-robin LB is client-side, and out of M1's scope).
/// In M1 a provider has exactly one instance, so every non-empty answer has
/// exactly one element — but the shape LB will need is here from the start
/// rather than broken into later.
#[derive(Clone, Debug, Default)]
pub struct PeerAddrs {
    /// `(provider, kind, addr)`. A Vec, not a map: multiple instances of one
    /// provider are the eventual shape, and at twelve services a scan is not a
    /// data structure worth having an opinion about.
    entries: Vec<(&'static str, AddrKind, String)>,
}

impl PeerAddrs {
    /// Derives the map from the fleet that is actually booting.
    ///
    /// Each entry is formatted from the def IN HAND via [`service_addr`], never
    /// by looking its provider name back up in `fleet`. The round-trip would be
    /// a first-match lookup, so the day two defs share a provider (M2's
    /// replicas — the day this map's list shape starts to matter) every entry
    /// would format from the FIRST def's port: two instances rendered as one
    /// address twice, which looks exactly like a healthy 2-element answer and
    /// would send half the LB's traffic to a port that isn't there.
    pub fn from_fleet(fleet: &[ServiceDef]) -> Self {
        let mut entries = Vec::new();
        for svc in fleet {
            // No short name ⇒ not resolvable as a peer. See the type doc: this
            // is the monolith, and it is data, not a special case.
            let Some(provider) = svc.provider else { continue };
            // Only the kinds this service actually HAS: `service_addr` answers
            // `None` for Edge against `edge_port: None` (admin-svc), so no Edge
            // entry is ever created for it and the lookup 404s instead of
            // falling back to the HTTP port.
            for kind in [AddrKind::Edge, AddrKind::Http] {
                if let Some(addr) = service_addr(svc, kind) {
                    entries.push((provider, kind, addr));
                }
            }
        }
        Self { entries }
    }

    /// Every address of `kind` for `provider`. Never falls back to the other
    /// kind.
    ///
    /// EMPTY means "no such (provider, kind) in this topology" — an unknown
    /// provider, or a provider with no address of that kind — which is the
    /// caller's 404. It does NOT mean "known, but nothing live": that state
    /// does not exist yet (this map is derived from the manifest, not from
    /// liveness) and when M2 introduces it, it belongs in the wire's `addrs`
    /// list as `[]`, not here. See [`crate::agentapi`] for that boundary.
    pub fn lookup(&self, provider: &str, kind: AddrKind) -> Vec<String> {
        self.entries
            .iter()
            .filter(|(name, entry_kind, _)| *name == provider && *entry_kind == kind)
            .map(|(_, _, addr)| addr.clone())
            .collect()
    }
}

/// [`compose_env`]'s core, resolving `svc`'s peers against an EXPLICIT fleet:
/// peer addresses are a property of the topology being booted, so the caller
/// that chose the topology passes the fleet it chose rather than this function
/// re-deriving (and possibly disagreeing with) it. `supervisor::run_up` picks
/// the defs by `Topology` and threads them here; the future `resolve` map is
/// built from the same slice, so env and `resolve` cannot diverge.
///
/// Taking a slice is also what makes the derivation exercisable with synthetic
/// data (same shape as [`fleet_pg_budget`]) — in particular the
/// previously-broken branch "a provider's port change reaches its consumers'
/// env", which a `'static` real fleet cannot express.
pub(crate) fn compose_env_with_fleet(
    svc: &ServiceDef,
    inputs: &RuntimeInputs,
    fleet: &[ServiceDef],
) -> BTreeMap<OsString, OsString> {
    let mut env: BTreeMap<OsString, OsString> = BTreeMap::new();

    for key in SERVICE_ENV_ALLOWLIST {
        if let Some(value) = std::env::var_os(key) {
            env.insert(OsString::from(*key), value);
        }
    }

    env.insert(OsString::from("PORT"), OsString::from(format!(":{}", svc.http_port)));
    if let Some(port) = svc.edge_port {
        env.insert(OsString::from("EDGE_ADDR"), OsString::from(format!(":{port}")));
    }

    if svc.has_db {
        env.insert(OsString::from("DATABASE_URL"), OsString::from(inputs.database_url.clone()));
        env.insert(
            OsString::from("DATABASE_POOL_MAX_CONNECTIONS"),
            OsString::from(svc.pool_max.to_string()),
        );
        env.insert(OsString::from("EDGE_CA_CERT"), inputs.ca_cert.clone().into_os_string());
        env.insert(OsString::from("EDGE_CA_KEY"), inputs.ca_key.clone().into_os_string());
    } else if svc.name == "gateway-svc" {
        // Pure-transport front door: no pool, but it still dials every peer
        // over the internal mTLS edge, so it needs the CA material despite
        // has_db == false. Verified against tools/processctl/src/fleet.rs
        // game_backend_fleet's `gateway_env` (inserts EDGE_CA_CERT/EDGE_CA_KEY
        // directly, never routes gateway through the DB-only `base()` helper
        // that also injects DATABASE_POOL_MAX_CONNECTIONS).
        env.insert(OsString::from("EDGE_CA_CERT"), inputs.ca_cert.clone().into_os_string());
        env.insert(OsString::from("EDGE_CA_KEY"), inputs.ca_key.clone().into_os_string());
    }

    for (key, provider, kind) in svc.peers {
        env.insert(
            OsString::from(*key),
            OsString::from(peer_addr(fleet, svc.name, provider, *kind)),
        );
    }

    // A managed process is told WHO to ask, never WHERE its peers are: the
    // addresses it would have been handed above it resolves for itself over the
    // agent's `resolve`, which answers from `PeerAddrs::from_fleet(fleet)` — the
    // same slice, formatted by the same `service_addr`. That shared derivation
    // is why env and `resolve` cannot disagree about the fleet; only the moment
    // the service learns an address differs.
    if svc.managed {
        env.insert(OsString::from(ORCHESTRATOR_URL_ENV), OsString::from(agent_url()));
    }

    for (key, value) in svc.env_extra {
        env.insert(OsString::from(*key), OsString::from(*value));
    }

    env
}

/// Builds the full spawn environment for `svc`: parent-env allowlist, then
/// `PORT`/`EDGE_ADDR`, then (if DB-backed, or gateway-svc which dials mTLS
/// edges without owning a pool) `DATABASE_URL`/`DATABASE_POOL_MAX_CONNECTIONS`/
/// `EDGE_CA_CERT`/`EDGE_CA_KEY`, then the `peers` addresses DERIVED from each
/// provider's own port field, then `env_extra` last (so a service's own wiring
/// always wins over anything synthesized above it).
///
/// Convenience for callers holding a def but not its fleet (the goldens,
/// verifyctl's parity stage): `svc`'s peers resolve against the manifest
/// `svc` ITSELF belongs to, found by identity — never against `split_fleet()`
/// by assumption. A monolith-shaped def is therefore resolved against the
/// monolith, where a split-only provider is absent and fails loudly instead of
/// silently picking up a split address for a process that isn't running.
///
/// The supervisor does NOT use this: it knows the topology it chose and calls
/// [`compose_env_with_fleet`] with that fleet.
///
/// PANICS if `svc` belongs to neither real manifest — a synthetic def has no
/// discoverable fleet, so it must go through [`compose_env_with_fleet`].
pub fn compose_env(svc: &ServiceDef, inputs: &RuntimeInputs) -> BTreeMap<OsString, OsString> {
    compose_env_with_fleet(svc, inputs, &home_fleet(svc))
}

/// The real manifest `svc` is a member of, by `name`.
fn home_fleet(svc: &ServiceDef) -> Vec<ServiceDef> {
    let split = split_fleet();
    if split.iter().any(|peer| peer.name == svc.name) {
        return split;
    }
    let mono = monolith();
    if svc.name == mono.name {
        return vec![mono];
    }
    panic!(
        "fleet manifest: {:?} belongs to neither split_fleet() nor monolith(); a synthetic \
         def must resolve its peers through compose_env_with_fleet with an explicit fleet",
        svc.name
    )
}

/// Fleet-manifest errors. Kept local to `weles` (zero-sharing: never reuses
/// `tools/processctl`'s `FleetError`).
#[derive(Debug)]
pub enum ManifestError {
    /// `cmd/*-svc` on disk disagrees with the canonical [`split_fleet`]
    /// names, in either direction. Lists EVERY drifted entry, not just the
    /// first — a didn't-forget tool dies pre-work with a per-entry log.
    DiskDrift { missing_on_disk: Vec<String>, missing_in_manifest: Vec<String> },
    ReadDir { path: PathBuf, source: std::io::Error },
    ReadEntry { path: PathBuf, source: std::io::Error },
    PoolBudgetExceeded { total: u32, budget: u32, breakdown: String },
}

impl std::fmt::Display for ManifestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ManifestError::DiskDrift { missing_on_disk, missing_in_manifest } => {
                writeln!(f, "fleet manifest drift against cmd/*-svc on disk:")?;
                for name in missing_on_disk {
                    writeln!(f, "  on disk but not in manifest: {name}")?;
                }
                for name in missing_in_manifest {
                    writeln!(f, "  in manifest but not on disk: {name}")?;
                }
                Ok(())
            }
            ManifestError::ReadDir { path, source } => {
                write!(f, "read service directory {}: {source}", path.display())
            }
            ManifestError::ReadEntry { path, source } => {
                write!(f, "read entry in service directory {}: {source}", path.display())
            }
            ManifestError::PoolBudgetExceeded { total, budget, breakdown } => {
                write!(
                    f,
                    "fleet Postgres session reservation {total} exceeds budget {budget}\n{breakdown}"
                )
            }
        }
    }
}

impl std::error::Error for ManifestError {}

/// Diffs the canonical [`split_fleet`] names against the `*-svc` directories
/// under `cmd_dir`. Fails loudly, listing every drifted entry, in EITHER
/// direction (a service added to the manifest without its `cmd/*-svc` root,
/// or a `cmd/*-svc` root nobody wired into the manifest).
pub fn validate_disk(cmd_dir: &Path) -> Result<(), ManifestError> {
    let entries = std::fs::read_dir(cmd_dir)
        .map_err(|source| ManifestError::ReadDir { path: cmd_dir.to_path_buf(), source })?;
    let mut on_disk = Vec::new();
    for entry in entries {
        let entry = entry
            .map_err(|source| ManifestError::ReadEntry { path: cmd_dir.to_path_buf(), source })?;
        let file_type = entry
            .file_type()
            .map_err(|source| ManifestError::ReadEntry { path: entry.path(), source })?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if file_type.is_dir() && name.ends_with("-svc") {
            on_disk.push(name);
        }
    }
    validate_names(on_disk)
}

fn validate_names(names: impl IntoIterator<Item = String>) -> Result<(), ManifestError> {
    use std::collections::BTreeSet;
    let on_disk: BTreeSet<String> = names.into_iter().collect();
    let canonical: BTreeSet<String> =
        split_fleet().into_iter().map(|svc| svc.name.to_string()).collect();

    let missing_on_disk: Vec<String> = canonical.difference(&on_disk).cloned().collect();
    let missing_in_manifest: Vec<String> = on_disk.difference(&canonical).cloned().collect();

    if missing_on_disk.is_empty() && missing_in_manifest.is_empty() {
        Ok(())
    } else {
        Err(ManifestError::DiskDrift { missing_on_disk, missing_in_manifest })
    }
}

/// Per-DB-process dedicated Postgres sessions held OUTSIDE the pool: both
/// durable-event-plane and invalidation-plane are constructed in any process
/// with a DB (DB ⇒ plane — see CLAUDE.md), so every DB-backed process
/// reserves the delivery workers + the wake-up listener + the invalidation
/// listener. Mirrors `tools/processctl/src/fleet.rs::PLANE_DEDICATED_SESSIONS`
/// (`AE_WORKERS`(2) + `AE_WAKEUP_SESSIONS`(1) + `INVALIDATION_LISTEN_SESSIONS`(1)).
const PLANE_DEDICATED_SESSIONS: u32 = 4;

/// scheduler-svc's one dedicated per-fire connection, beyond the two planes.
/// Mirrors `tools/processctl/src/fleet.rs::SCHEDULER_FIRE_SESSIONS`.
const SCHEDULER_FIRE_SESSIONS: u32 = 1;

/// Sessions reserved for dev tooling running ALONGSIDE the fleet (splitproof's
/// own sqlx pool, devctl/adminctl seeding, eventctl, asyncevents poison-burst
/// headroom, slack). Mirrors `tools/processctl/src/fleet.rs::HARNESS_RESERVE`
/// (itemized there; not re-derived here).
const HARNESS_RESERVE: u32 = 10;

/// Usable Postgres sessions the whole fleet + monolith must fit within.
/// Mirrors `tools/processctl/src/fleet.rs::PG_SESSION_BUDGET`
/// (`max_connections`(100) - `superuser_reserved_connections`(3) -
/// `HARNESS_RESERVE`).
pub const PG_SESSION_BUDGET: u32 = 97 - HARNESS_RESERVE;

/// Per-process (pool_max, dedicated) reservation for a manifest entry. Split
/// out as its own function so the arithmetic is unit-testable independent of
/// the real fleet data (see `manifest_tests`'s synthetic-numbers case proving
/// the dedicated term matters, not just the pool sum).
pub fn service_pg_budget(svc: &ServiceDef) -> (u32, u32) {
    if !svc.has_db {
        // Pure-transport front door: no pool, no plane.
        return (0, 0);
    }
    let mut dedicated = PLANE_DEDICATED_SESSIONS;
    // The scheduler's dedicated per-fire connection is charged wherever the
    // scheduler module runs: its own svc in the split, the "server" package
    // in the monolith (one process hosts every module).
    if svc.name == "scheduler-svc" || svc.pkg == "server" {
        dedicated += SCHEDULER_FIRE_SESSIONS;
    }
    (svc.pool_max, dedicated)
}

/// The one budget authority: sums pool_max + dedicated across `services`
/// and fails with the itemized breakdown if the total exceeds
/// [`PG_SESSION_BUDGET`]. Full fleet.rs arithmetic — NOT a pool-only
/// shortcut: the dedicated term (plane workers + listeners, scheduler's
/// fire connection) is charged too. Takes a slice so the failing branch is
/// exercisable with a synthetic fleet in tests (the public wrapper feeds
/// the real manifests).
fn fleet_pg_budget(services: &[ServiceDef]) -> Result<(), ManifestError> {
    let mut breakdown = String::new();
    let mut total = 0u32;
    for svc in services {
        let (pool, dedicated) = service_pg_budget(svc);
        total += pool + dedicated;
        breakdown.push_str(&format!(
            "  {name}: pool={pool} dedicated={dedicated}\n",
            name = svc.name
        ));
    }
    if total > PG_SESSION_BUDGET {
        return Err(ManifestError::PoolBudgetExceeded {
            total,
            budget: PG_SESSION_BUDGET,
            breakdown,
        });
    }
    Ok(())
}

/// Validates BOTH real manifests against [`PG_SESSION_BUDGET`]: the split
/// fleet as one deployment, the monolith as another (each topology runs
/// alone against the shared local Postgres).
pub fn validate_pg_budget() -> Result<(), ManifestError> {
    fleet_pg_budget(&split_fleet())?;
    fleet_pg_budget(&[monolith()])
}

#[cfg(test)]
#[path = "manifest_tests.rs"]
mod manifest_tests;
