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
//! [`Addrs::Told`] list appears strictly EARLIER in the Vec. An
//! [`AddrKind::Http`] entry carries NO such constraint — it is a passthrough
//! ORIGIN, dialed per request rather than at boot. Pinned by
//! `boot_order_respects_edge_peer_dependencies`, derived from the `peers`
//! field rather than hand-listed beside it.
//!
//! **Recorded semantic change (M1 Step 4):** gateway-svc is now
//! [`Addrs::Asks`] — it asks the agent where its peers are instead of being
//! told by env — so it declares no peer list, and with that the fleet lost
//! six Edge entries and its ONLY two `AddrKind::Http` entries. Two
//! consequences, neither hidden: the boot-order rule no longer constrains
//! gateway-svc from its own declaration (its position in the Vec is unchanged,
//! so the booted order is not), and the Http-vs-Edge asymmetry has no live
//! example left in the real fleet. Step 7 re-pointed the tests that pinned both
//! ON GATEWAY'S DATA: the boot-order count is 11 (not 17 — gateway's six edge
//! declarations legitimately left the field, and restoring the number would
//! assert a fact that is no longer true), the Http asymmetry is now proven on
//! synthetic data with a guard that fires if an Http peer ever returns to the
//! real fleet, and the dual-kind provider (`accounts`) is proven through
//! [`PeerAddrs`] — where the gateway now actually reads it.
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

/// The minimal, always-on floor of parent-process env vars every spawned
/// process inherits — the ambient interpreter/toolchain plumbing a binary needs
/// to exec at all. It carries NO domain meaning: topology wiring and bind
/// addresses are never inherited here.
///
/// Anything beyond this floor reaches a service two domain-blind ways: its
/// per-service `env` table, or a per-fleet PASSTHROUGH list naming env KEYS
/// weles forwards from its OWN environment (threaded into
/// [`compose_env_with_fleet`] and [`crate::prep::run_prepare`]). weles knows the
/// key NAME, never its meaning.
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
    "TMPDIR",
    "USERPROFILE",
    "WINDIR",
];

/// The loopback port weles's own agent HTTP endpoint ([`crate::agentapi`])
/// binds. This file is the ONE place in weles allowed to write a port (see the
/// module doc), which is why the agent's port lives here rather than beside the
/// server that binds it: a runtime-minted port belongs to weles's derivation,
/// not the operator's [`ServiceDef::env`] table, and a second port-writing
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

/// The env key an [`Addrs::Asks`] process is handed the agent's URL
/// through. `cmd/gateway-svc`'s main reads exactly this name (its own copy —
/// zero-sharing), and the VALUE is derived from [`AGENT_PORT`] by [`agent_url`],
/// never written beside it.
///
/// `pub` because verifyctl's `weles-fleet-parity` stage must name the key that
/// carries the delegation in order to EXCLUDE it from its diff against
/// processctl (which has no managed mode). That exclusion is derived from this
/// const and [`agent_url`] rather than re-spelling the string, so the day the
/// key is renamed the exclusion follows it instead of quietly widening to a name
/// nothing composes any more.
pub const ORCHESTRATOR_URL_ENV: &str = "ORCHESTRATOR_URL";

/// Where a managed service reaches the agent. Derived from [`AGENT_PORT`] — the
/// one authority for the agent's port — rather than spelled as a literal in a
/// service's [`ServiceDef::env`] table, which would be the same fact twice and
/// free to drift the day the port moves.
///
/// The host is `127.0.0.1` by the same construction [`service_addr`] relies on:
/// [`crate::agentapi::AgentServer::bind`] binds loopback, so the URL handed to a
/// service is the address that endpoint actually took.
///
/// `pub` for the same reason as [`ORCHESTRATOR_URL_ENV`]: verifyctl's parity
/// stage excludes the exact PAIR (key AND value) this composes, not the key
/// alone — so an `ORCHESTRATOR_URL` carrying anything other than this URL is
/// still a FAIL there.
pub fn agent_url() -> String {
    format!("http://127.0.0.1:{AGENT_PORT}")
}

/// Which of a provider's two port fields a peer address is formatted from.
///
/// This is a FIELD on every [`Addrs::Told`] entry, never inferred from
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

/// HOW a process learns where its peers are — the two ways, as ONE field,
/// because they are one decision and a process makes it once.
///
/// This is deliberately not `peers: &[…]` beside `managed: bool`: that pair can
/// spell `managed: true` WITH peers, and [`compose_env_with_fleet`] would then
/// hand a process both authorities — the addresses it resolves for itself AND an env
/// copy it never reads. An unread value is the worst kind: it drifts silently
/// until someone believes it. Here that state cannot be written down, so no test
/// has to forbid it (an earlier `managed_services_declare_no_peers` did exactly
/// that, and was deleted when this enum made it unrepresentable).
///
/// The mirror image, on the process's side, is `cmd/gateway-svc`'s `AddrSource`
/// — `Env` or `Agent(url)`, likewise two variants and no third. The two spellings
/// are hand-copied (zero-sharing), which is fine: what must agree between them is
/// the WIRE, not this shape.
///
/// Owned (no longer `Copy`): the peer list is parsed from the operator's
/// `fleet.toml` into an owned `Vec`, not a `&'static` literal.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Addrs {
    /// TOLD, at spawn: `(env key, provider, kind)` per peer, each address
    /// DERIVED in [`compose_env_with_fleet`] from that provider's own port field
    /// — so the port declaration is the one authority for "where is X" and a
    /// port change propagates to every consumer by construction.
    ///
    /// Never write an address literal here or in [`ServiceDef::env`] — that is
    /// the two-authorities drift this seam replaced.
    Told(Vec<(String, String, AddrKind)>),
    /// ASKS the agent, at boot: [`compose_env_with_fleet`] hands this process
    /// [`ORCHESTRATOR_URL_ENV`] (derived from [`AGENT_PORT`]) and NONE of the
    /// addresses above, and it resolves each for itself over
    /// [`crate::agentapi`]'s `resolve`.
    ///
    /// Not a topology branch, and not a different fleet: `resolve` answers from
    /// [`PeerAddrs::from_fleet`] over the SAME booting slice
    /// [`compose_env_with_fleet`] composes from, through the same
    /// [`service_addr`]. Only the moment the process learns an address differs.
    Asks,
}

impl Addrs {
    /// The peer declarations, empty for a process that asks. Lets a sweep read
    /// "what is this told" uniformly; nothing may infer "not managed" from an
    /// empty slice (`Told(vec![])` — most services — is not `Asks`).
    pub fn told(&self) -> &[(String, String, AddrKind)] {
        match self {
            Addrs::Told(peers) => peers,
            Addrs::Asks => &[],
        }
    }
}

/// A single fleet process: its identity, ports, how it learns its peers, and
/// the env pairs unique to it (dev-mode opt-ins, own-process config).
///
/// Owned (parsed from `fleet.toml`), and domain-BLIND: weles no longer knows
/// which processes own a Postgres pool or how large. DSN, pool caps, CA paths
/// and secrets are ordinary keys in `env` (or forwarded by passthrough), never
/// weles concepts.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServiceDef {
    pub name: String,
    pub pkg: String,
    /// The SHORT domain name this process is dialed by — the one name the
    /// wire and the service registry already use (`remote::Stub::new(
    /// "characters", …)`, which archcheck rule 17 text-scans; the
    /// `modules/<name>` / `api/<name>` directory). [`Addrs::Told`] and
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
    pub provider: Option<String>,
    pub http_port: u16,
    pub edge_port: Option<u16>,
    pub player_port: Option<u16>,
    /// How this process learns where its peers are: TOLD the addresses at
    /// spawn, or ASKS the agent for them at boot. One field, because it is one
    /// decision — see [`Addrs`].
    pub addrs: Addrs,
    /// Literal, operator-authored env: dev-mode opt-ins, this process's own
    /// config (`TLS_MODE`, `PLAYER_EDGE_ADDR` — its own bind, not a peer's), and
    /// any opaque values weles is domain-blind to (`DATABASE_URL`,
    /// `DATABASE_POOL_MAX_CONNECTIONS`, `EDGE_CA_CERT`/`EDGE_CA_KEY`).
    ///
    /// Keys here are DISJOINT from [`Addrs::Told`] keys: `env` is applied last
    /// and would silently override a derived address (pinned by
    /// `no_env_extra_key_shadows_a_derived_peer_key`).
    pub env: BTreeMap<String, String>,
}

/// Builds an owned env map from literal pairs (temporary Step-1 helper for the
/// hardcoded fleets below, deleted with them in Step 4 once `fleet.toml`
/// fixtures replace them).
fn env_map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

/// Builds an owned `Addrs::Told` from literal peer tuples (temporary Step-1
/// helper, deleted with the hardcoded fleets in Step 4).
fn told(peers: &[(&str, &str, AddrKind)]) -> Addrs {
    Addrs::Told(peers.iter().map(|(k, p, kind)| (k.to_string(), p.to_string(), *kind)).collect())
}

/// The 12-process split fleet, in boot order. Each dependency (a peer a
/// service dials over the internal mTLS edge) appears strictly earlier in
/// this list than its dependent, matching
/// `tools/processctl/src/fleet.rs::game_backend_fleet`'s `dependencies`
/// constraint by construction.
///
/// TEMPORARY: this hardcoded table is deleted in Step 4 once
/// `weles/fleet.split.toml` replaces it; it survives Step 1 only so
/// `cargo build -p weles` stays green while the owned `ServiceDef` shape lands.
pub fn split_fleet() -> Vec<ServiceDef> {
    vec![
        ServiceDef {
            name: "accounts-svc".to_string(),
            pkg: "accounts-svc".to_string(),
            provider: Some("accounts".to_string()),
            http_port: 8084,
            edge_port: Some(9003),
            player_port: None,
            addrs: told(&[]),
            env: env_map(&[("ACCOUNTS_DEV_AUTH", "1")]),
        },
        ServiceDef {
            name: "apikeys-svc".to_string(),
            pkg: "apikeys-svc".to_string(),
            provider: Some("apikeys".to_string()),
            http_port: 8091,
            edge_port: Some(9009),
            player_port: None,
            addrs: told(&[]),
            env: env_map(&[("APIKEYS_DEV_SEED", "1")]),
        },
        ServiceDef {
            name: "audit-svc".to_string(),
            pkg: "audit-svc".to_string(),
            provider: Some("audit".to_string()),
            http_port: 8086,
            edge_port: Some(9004),
            player_port: None,
            addrs: told(&[]),
            env: env_map(&[]),
        },
        ServiceDef {
            name: "scheduler-svc".to_string(),
            pkg: "scheduler-svc".to_string(),
            provider: Some("scheduler".to_string()),
            http_port: 8087,
            edge_port: Some(9005),
            player_port: None,
            // Deliberately NO SCHEDULER_ENABLED here — this manifest is the
            // Development flavor of tools/processctl/src/fleet.rs, which
            // only sets SCHEDULER_ENABLED under FleetFlavor::Proof.
            addrs: told(&[]),
            env: env_map(&[]),
        },
        ServiceDef {
            name: "rating-svc".to_string(),
            pkg: "rating-svc".to_string(),
            provider: Some("rating".to_string()),
            http_port: 8089,
            edge_port: Some(9007),
            player_port: None,
            addrs: told(&[]),
            env: env_map(&[]),
        },
        ServiceDef {
            name: "leaderboard-svc".to_string(),
            pkg: "leaderboard-svc".to_string(),
            provider: Some("leaderboard".to_string()),
            http_port: 8090,
            edge_port: Some(9008),
            player_port: None,
            addrs: told(&[]),
            env: env_map(&[]),
        },
        ServiceDef {
            name: "match-svc".to_string(),
            pkg: "match-svc".to_string(),
            provider: Some("match".to_string()),
            http_port: 8088,
            edge_port: Some(9006),
            player_port: None,
            addrs: told(&[("RATING_EDGE_ADDR", "rating", AddrKind::Edge)]),
            env: env_map(&[]),
        },
        ServiceDef {
            name: "config-svc".to_string(),
            pkg: "config-svc".to_string(),
            provider: Some("config".to_string()),
            http_port: 8083,
            edge_port: Some(9002),
            player_port: None,
            addrs: told(&[]),
            env: env_map(&[]),
        },
        ServiceDef {
            name: "characters-svc".to_string(),
            pkg: "characters-svc".to_string(),
            provider: Some("characters".to_string()),
            http_port: 8080,
            edge_port: Some(9000),
            player_port: None,
            addrs: told(&[("CONFIG_EDGE_ADDR", "config", AddrKind::Edge)]),
            env: env_map(&[]),
        },
        ServiceDef {
            name: "inventory-svc".to_string(),
            pkg: "inventory-svc".to_string(),
            provider: Some("inventory".to_string()),
            http_port: 8081,
            edge_port: Some(9001),
            player_port: None,
            addrs: told(&[
                ("CHARACTERS_EDGE_ADDR", "characters", AddrKind::Edge),
                ("CONFIG_EDGE_ADDR", "config", AddrKind::Edge),
            ]),
            env: env_map(&[("INVENTORY_DEV_GRANT", "1")]),
        },
        ServiceDef {
            name: "gateway-svc".to_string(),
            pkg: "gateway-svc".to_string(),
            provider: Some("gateway".to_string()),
            http_port: 8082,
            edge_port: None,
            player_port: Some(9100),
            // M1's first managed process, and the natural first: it calls
            // peers, and nobody calls it (it serves no edge), so it can start
            // resolving without forcing another service to move. Instead of
            // the eight address keys it used to carry — six `*_EDGE_ADDR` plus
            // the two passthrough ORIGINS (`ADMIN_HTTP_ADDR`,
            // `ACCOUNTS_HTTP_ADDR`: admin-svc has no edge at all, and
            // accounts-svc is dialed as both kinds) — it is handed
            // ORCHESTRATOR_URL and asks the agent for each of the eight.
            addrs: Addrs::Asks,
            // PLAYER_EDGE_ADDR is this process's OWN player-plane bind, not a
            // peer's address — it stays a literal.
            env: env_map(&[("PLAYER_EDGE_ADDR", ":9100"), ("TLS_MODE", "off")]),
        },
        ServiceDef {
            name: "admin-svc".to_string(),
            pkg: "admin-svc".to_string(),
            provider: Some("admin".to_string()),
            http_port: 8085,
            edge_port: None,
            player_port: None,
            addrs: told(&[
                ("CHARACTERS_EDGE_ADDR", "characters", AddrKind::Edge),
                ("INVENTORY_EDGE_ADDR", "inventory", AddrKind::Edge),
                ("CONFIG_EDGE_ADDR", "config", AddrKind::Edge),
                ("ACCOUNTS_EDGE_ADDR", "accounts", AddrKind::Edge),
                ("AUDIT_EDGE_ADDR", "audit", AddrKind::Edge),
                ("SCHEDULER_EDGE_ADDR", "scheduler", AddrKind::Edge),
                ("APIKEYS_EDGE_ADDR", "apikeys", AddrKind::Edge),
            ]),
            env: env_map(&[
                ("ADMIN_COOKIE_SECURE", "0"),
                ("TRUSTED_PROXY_CIDRS", "127.0.0.1/32"),
            ]),
        },
    ]
}

/// The single-process monolith topology (`cmd/server`, package `server`).
///
/// TEMPORARY: deleted in Step 4 with [`split_fleet`] once the `fleet.toml`
/// fixtures replace both.
pub fn monolith() -> ServiceDef {
    ServiceDef {
        name: "server".to_string(),
        pkg: "server".to_string(),
        provider: None,
        http_port: 8080,
        edge_port: None,
        player_port: Some(9100),
        // One process hosts every module: there are no peers to dial, so the
        // monolith is trivially free of derived addresses.
        addrs: told(&[]),
        env: env_map(&[
            ("PLAYER_EDGE_ADDR", ":9100"),
            ("APIKEYS_DEV_SEED", "1"),
            ("ACCOUNTS_DEV_AUTH", "1"),
            ("INVENTORY_DEV_GRANT", "1"),
            ("TLS_MODE", "off"),
            ("ADMIN_COOKIE_SECURE", "0"),
            ("TRUSTED_PROXY_CIDRS", "127.0.0.1/32"),
        ]),
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
    let def = fleet.iter().find(|svc| svc.provider.as_deref() == Some(provider)).unwrap_or_else(|| {
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
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PeerAddrs {
    /// `(provider, kind, addr)`. A Vec, not a map: multiple instances of one
    /// provider are the eventual shape, and at twelve services a scan is not a
    /// data structure worth having an opinion about.
    entries: Vec<(String, AddrKind, String)>,
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
            let Some(provider) = &svc.provider else { continue };
            // Only the kinds this service actually HAS: `service_addr` answers
            // `None` for Edge against `edge_port: None` (admin-svc), so no Edge
            // entry is ever created for it and the lookup 404s instead of
            // falling back to the HTTP port.
            for kind in [AddrKind::Edge, AddrKind::Http] {
                if let Some(addr) = service_addr(svc, kind) {
                    entries.push((provider.clone(), kind, addr));
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
            .filter(|(name, entry_kind, _)| name.as_str() == provider && *entry_kind == kind)
            .map(|(_, _, addr)| addr.clone())
            .collect()
    }
}

/// Builds `svc`'s full spawn environment, resolving its peers against an
/// EXPLICIT fleet: peer addresses are a property of the topology being booted,
/// so the caller that chose the fleet passes it rather than this function
/// re-deriving (and possibly disagreeing with) it. The future `resolve` map is
/// built from the same slice, so env and `resolve` cannot diverge.
///
/// Composition ORDER (each layer overrides the one above it):
/// 1. the always-on [`SERVICE_ENV_ALLOWLIST`] floor plus the per-fleet
///    `passthrough` KEYS, both forwarded from weles's OWN environment (this is
///    how opaque operator values — `DATABASE_URL`, `EDGE_CA_*`, secrets — reach
///    a service without weles knowing their meaning);
/// 2. `PORT`, and `EDGE_ADDR` for a service that serves an edge;
/// 3. the peer addresses (TOLD) DERIVED from each provider's own port field, or
///    `ORCHESTRATOR_URL` (ASKS);
/// 4. the service's own `env` table LAST — so an operator-authored value always
///    wins over anything synthesized above it.
///
/// weles injects NO `DATABASE_URL`/`EDGE_CA_*`/`DATABASE_POOL_MAX_CONNECTIONS`
/// of its own: those are ordinary keys the operator supplies via `env` or
/// `passthrough`, or they do not reach the service at all. That is the domain
/// knowledge this function shed.
pub(crate) fn compose_env_with_fleet(
    svc: &ServiceDef,
    passthrough: &[String],
    fleet: &[ServiceDef],
) -> BTreeMap<OsString, OsString> {
    let mut env: BTreeMap<OsString, OsString> = BTreeMap::new();

    // The always-on floor plus the operator's passthrough KEYS, forwarded from
    // weles's own environment. weles knows the key NAME, never its meaning.
    for key in SERVICE_ENV_ALLOWLIST.iter().copied().chain(passthrough.iter().map(String::as_str)) {
        if let Some(value) = std::env::var_os(key) {
            env.insert(OsString::from(key), value);
        }
    }

    env.insert(OsString::from("PORT"), OsString::from(format!(":{}", svc.http_port)));
    if let Some(port) = svc.edge_port {
        env.insert(OsString::from("EDGE_ADDR"), OsString::from(format!(":{port}")));
    }

    // One decision, one match: a process is TOLD its peers or ASKS for them,
    // never both. `Addrs` makes "both" unrepresentable, so there is no invariant
    // to check here — a process that asks is handed WHO to ask and none of the
    // addresses. It resolves them for itself over the agent, which answers from
    // `PeerAddrs::from_fleet(fleet)` — the same slice, formatted by the same
    // `service_addr`. That shared derivation is why env and `resolve` cannot
    // disagree about the fleet; only the moment a service learns an address
    // differs.
    match &svc.addrs {
        Addrs::Told(peers) => {
            for (key, provider, kind) in peers {
                env.insert(
                    OsString::from(key),
                    OsString::from(peer_addr(fleet, &svc.name, provider, *kind)),
                );
            }
        }
        Addrs::Asks => {
            env.insert(OsString::from(ORCHESTRATOR_URL_ENV), OsString::from(agent_url()));
        }
    }

    for (key, value) in &svc.env {
        env.insert(OsString::from(key), OsString::from(value));
    }

    env
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

#[cfg(test)]
#[path = "manifest_tests.rs"]
mod manifest_tests;
