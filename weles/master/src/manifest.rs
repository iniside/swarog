//! Fleet composition machinery — the runtime TYPES and wiring for a fleet, now
//! that the fleet DATA (process names, ports, boot order, per-process env) lives
//! in an operator-authored `fleet.toml` parsed by [`crate::fleet_toml`]. This
//! module owns [`ServiceDef`]/[`Addrs`]/[`AddrKind`], the one address formatter
//! [`service_addr`], the peer-address derivation ([`peer_addr`],
//! [`PeerAddrs`]), and the env composer [`compose_env_with_fleet`] — everything
//! that turns a parsed fleet slice into what a service is spawned with. It no
//! longer holds the fleet itself: `fleet_toml::load` produces the `Vec` this
//! machinery operates on.
//!
//! The `Vec` order IS the boot order — dependencies are expressed implicitly by
//! position: every [`AddrKind::Edge`] entry in a service's [`Addrs::Told`] list
//! must appear strictly EARLIER in the Vec (enforced by
//! [`crate::fleet_toml::validate`]). An [`AddrKind::Http`] entry carries NO such
//! constraint — it is a passthrough ORIGIN, dialed per request rather than at
//! boot.
//!
//! **`Addrs::Asks` (the managed front door):** gateway-svc asks the agent where
//! its peers are (via [`ORCHESTRATOR_URL_ENV`]) instead of being told by env, so
//! it declares no peer list. The boot-order rule does not constrain a service
//! from its own declaration when it declares none, and the dual-kind provider
//! (`accounts`, edge + http) is proven through [`PeerAddrs`] — where the gateway
//! now actually reads it.
//!
//! Deliberate delta vs `tools/processctl/src/fleet.rs`'s Development flavor:
//! weles's composed env is fully deterministic — the `overrideable_env` seam
//! (which lets ambient `SCHEDULER_ENABLED`/… override the manifest) was
//! consciously NOT ported, per the config-as-code decision: what a service gets
//! is exactly what its `fleet.toml` `[service.env]` says, plus the always-on
//! [`SERVICE_ENV_ALLOWLIST`] floor and the fleet's `passthrough` keys.

use std::collections::BTreeMap;
use std::ffi::OsString;

use serde::{Deserialize, Serialize};

/// The minimal, always-on floor of parent-process env vars every spawned
/// process inherits — the ambient interpreter/toolchain plumbing a binary needs
/// to exec at all. It carries NO domain meaning: topology wiring and bind
/// addresses are never inherited here.
///
/// Anything beyond this floor reaches a service two domain-blind ways: its
/// per-service `env` table, or a per-fleet PASSTHROUGH list naming env KEYS
/// weles forwards from its OWN environment (threaded into
/// [`compose_env_with_fleet`] and `prep::run_prepare`). weles knows the
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

/// The loopback port weles's own agent HTTP endpoint (`agentapi`)
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
/// `pub` originally because verifyctl's now-DELETED `weles-fleet-parity` stage
/// (2026-07-21 errata: the fleet moved to `fleet.toml`, and that stage was
/// removed with it) named this key to EXCLUDE it from its diff against
/// processctl. That cross-crate reason is gone; it is now used only within this
/// crate (`compose_env_with_fleet`'s `Asks` branch + `manifest_tests.rs`), so
/// `pub(crate)` would now suffice.
pub const ORCHESTRATOR_URL_ENV: &str = "ORCHESTRATOR_URL";

/// Where a managed service reaches the agent. Derived from [`AGENT_PORT`] — the
/// one authority for the agent's port — rather than spelled as a literal in a
/// service's [`ServiceDef::env`] table, which would be the same fact twice and
/// free to drift the day the port moves.
///
/// The host is `127.0.0.1` by the same construction [`service_addr`] relies on:
/// `agentapi::AgentServer::bind` binds loopback, so the URL handed to a
/// service is the address that endpoint actually took.
///
/// `pub` for the same reason as [`ORCHESTRATOR_URL_ENV`] — originally
/// verifyctl's now-deleted `weles-fleet-parity` stage excluded the exact PAIR
/// (key AND value) this composes, not the key alone. That gate is gone
/// (2026-07-21 errata); it is now used only within this crate
/// (`compose_env_with_fleet` + `manifest_tests.rs`), so `pub(crate)` would suffice.
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
/// This is ALSO the `kind` on the agent's `resolve` wire (`agentapi`):
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

/// A service's port field ([`ServiceDef::http_port`] / [`ServiceDef::edge_port`]):
/// either an operator-authored LITERAL, or the explicit marker `"mint"` asking
/// the agent to bind a free OS port at spawn (A4).
///
/// # Explicit, never magic
///
/// Per the config-as-code / anti-magic rule there is NO "0 means mint" overload:
/// `0` is just an (invalid) literal, and ONLY the string `"mint"` requests
/// minting. The two forms are distinct at parse time — an integer deserializes
/// to [`Port::Literal`], the exact string `"mint"` to [`Port::Mint`], and any
/// other string is a loud error (a typo never silently falls through to a
/// literal or to mint).
///
/// # The resolved-before-read invariant
///
/// A `Mint` field is a PROMISE, not an address. The agent's up-front mint pass
/// (`supervisor::mint_fleet_ports`) binds a free OS port and PATCHES every
/// `Mint` to a [`Port::Literal`] BEFORE any address is derived — so
/// [`service_addr`], [`PeerAddrs::from_fleet`] and [`compose_env_with_fleet`]
/// only ever see resolved literals. Reading a port through [`Port::resolved`]
/// before the mint pass PANICS, on purpose: it is a programmer error (an address
/// derived off an unminted fleet), not a runtime condition to tolerate.
///
/// The static validator (`fleet_toml::validate`) is the ONE reader that runs
/// PRE-mint; it asks only [`Port::literal`]/[`Port::is_mint`], never
/// [`Port::resolved`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Port {
    /// An operator-authored fixed port.
    Literal(u16),
    /// `"mint"` — the agent binds a free OS port at spawn and patches this to
    /// [`Port::Literal`] before any consumer reads it.
    Mint,
}

impl Port {
    /// `true` for a not-yet-bound [`Port::Mint`]. The mint pass consumes this;
    /// the validator uses it to reject a Told consumer of a mintable provider.
    pub fn is_mint(&self) -> bool {
        matches!(self, Port::Mint)
    }

    /// The operator-authored literal, or `None` for [`Port::Mint`]. The
    /// pre-mint reader (`fleet_toml::validate`'s uniqueness check) uses this: a
    /// minted field has no literal to validate for collisions (its uniqueness is
    /// the agent's bind-time invariant).
    pub fn literal(&self) -> Option<u16> {
        match self {
            Port::Literal(port) => Some(*port),
            Port::Mint => None,
        }
    }

    /// The bound port. PANICS on [`Port::Mint`] — see the type doc's
    /// resolved-before-read invariant: an address must never be derived off a
    /// fleet whose mint pass has not yet run.
    pub fn resolved(&self) -> u16 {
        match self {
            Port::Literal(port) => *port,
            Port::Mint => panic!(
                "Port::resolved() on an unminted \"mint\" port — the agent's mint pass must \
                 patch every Mint to a literal before any address is derived"
            ),
        }
    }
}

impl std::fmt::Display for Port {
    /// The literal number, or `mint` for a not-yet-bound field — for the
    /// operator-facing `weles validate` fleet dump, which runs pre-mint.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Port::Literal(port) => write!(f, "{port}"),
            Port::Mint => f.write_str("mint"),
        }
    }
}

impl From<u16> for Port {
    fn from(port: u16) -> Self {
        Port::Literal(port)
    }
}

impl<'de> Deserialize<'de> for Port {
    /// An integer becomes [`Port::Literal`]; the exact string `"mint"` becomes
    /// [`Port::Mint`]; any other string is a loud error. `deny_unknown_fields`
    /// on the enclosing [`crate::fleet_toml`] structs still holds — this only
    /// widens the single field's value space from "u16" to "u16 | \"mint\"".
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Repr {
            Literal(u16),
            Tag(String),
        }
        match Repr::deserialize(deserializer)? {
            Repr::Literal(port) => Ok(Port::Literal(port)),
            Repr::Tag(tag) if tag == "mint" => Ok(Port::Mint),
            Repr::Tag(tag) => Err(serde::de::Error::custom(format!(
                "port must be an integer or the string \"mint\", got {tag:?}"
            ))),
        }
    }
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
    /// `agentapi`'s `resolve`.
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
    /// Which node runs this service — a manifest ANNOTATION, not scheduling
    /// (`weles-design.md:245`). It names a node; it is NOT an address (a raw
    /// address here would be a second address authority — addresses stay
    /// agent-resolved via [`service_addr`], loopback on one machine). On the
    /// current single-machine deployment (master ≡ agent, one node) placement
    /// is degenerate: `None` or the reserved sentinel `"local"` are the only
    /// legal values, and neither changes any address. A real node name is
    /// rejected at [`crate::fleet_toml::validate`] time (no node registry
    /// exists yet — fail closed rather than silently no-op). Host derivation
    /// from placement is the future multi-machine seam.
    pub placement: Option<String>,
    /// The HTTP bind port — a [`Port::Literal`] or [`Port::Mint`] (the agent
    /// binds a free OS port at spawn). Resolved to a literal by the mint pass
    /// before any address is derived.
    pub http_port: Port,
    /// The internal-edge bind port, `None` where the service serves no edge
    /// (admin/gateway). `Some(Port::Mint)` is a mintable edge.
    pub edge_port: Option<Port>,
    /// The player-QUIC bind port, `None` for a non-front service. NOT mintable:
    /// the one player front is a fixed public port, so this stays a plain
    /// literal.
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
    /// `no_env_key_shadows_a_derived_peer_key`).
    pub env: BTreeMap<String, String>,
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
///
/// `pub(crate)` for the composed-env and `resolve`-map paths, both of which run
/// AFTER the mint pass, so every port here is a resolved literal (a `Mint` would
/// PANIC via [`Port::resolved`] — the resolved-before-read invariant on
/// [`Port`]). A `None` return means the service has no address of this kind
/// (`edge_port: None`), not "not yet minted".
///
/// The validator does NOT go through here for its "does this provider serve this
/// kind?" question — it runs pre-mint and asks [`serves_kind`] instead (which
/// never reads a port value), so a mintable provider is not tripped into a
/// `resolved()` panic during validation.
pub(crate) fn service_addr(def: &ServiceDef, kind: AddrKind) -> Option<String> {
    let port = match kind {
        AddrKind::Edge => def.edge_port.as_ref()?.resolved(),
        AddrKind::Http => def.http_port.resolved(),
    };
    Some(format!("127.0.0.1:{port}"))
}

/// Whether `def` serves an address of `kind` AT ALL — a PORT-FIELD-PRESENCE
/// question that never reads the port value, so it is safe pre-mint (a mintable
/// provider still "serves" the kind; only its literal is not yet known).
///
/// This is [`crate::fleet_toml::validate`]'s kind-existence authority, split
/// from [`service_addr`] precisely so the validator (which runs before the mint
/// pass) can ask "peer requests a kind this provider has not got" without
/// resolving a `Mint` port.
pub(crate) fn serves_kind(def: &ServiceDef, kind: AddrKind) -> bool {
    match kind {
        AddrKind::Edge => def.edge_port.is_some(),
        AddrKind::Http => true,
    }
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
    /// list as `[]`, not here. See `agentapi` for that boundary.
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
pub fn compose_env_with_fleet(
    svc: &ServiceDef,
    passthrough: &[String],
    fleet: &[ServiceDef],
) -> BTreeMap<OsString, OsString> {
    let mut env: BTreeMap<OsString, OsString> = BTreeMap::new();

    // The always-on floor plus the operator's passthrough KEYS, forwarded from
    // weles's own environment. weles knows the key NAME, never its meaning.
    //
    // Forwarded through [`lookup_env`] — the SAME helper the agent's
    // `prep::run_one_prepare` uses for a prepare hook's passthrough (it imports
    // this one) — so a passthrough key resolves identically for a service and
    // for a prepare hook (on Windows both are case-insensitive; a service used
    // to use exact-case `std::env::var_os` while hooks were case-insensitive —
    // Step-1-review deferred finding, now closed with one lookup authority).
    for key in SERVICE_ENV_ALLOWLIST.iter().copied().chain(passthrough.iter().map(String::as_str)) {
        if let Some(value) = lookup_env(key) {
            env.insert(OsString::from(key), value);
        }
    }

    // Ports are resolved literals here: this runs AFTER the agent's mint pass,
    // which patched every `Mint` field. `resolved()` panics on an unminted port
    // rather than silently emitting `:mint` into a service's env.
    env.insert(OsString::from("PORT"), OsString::from(format!(":{}", svc.http_port.resolved())));
    if let Some(port) = &svc.edge_port {
        env.insert(OsString::from("EDGE_ADDR"), OsString::from(format!(":{}", port.resolved())));
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

/// Reads one env var from weles's OWN environment, case-insensitively on
/// Windows (to match `%VAR%` lookup semantics) and exact-case on Unix. This is
/// the SINGLE passthrough-lookup authority: both [`compose_env_with_fleet`]
/// (services) and the agent's `prep::run_one_prepare` (prepare hooks) forward
/// passthrough KEYS through it, so a Windows case-variant passthrough key
/// resolves identically for a service and for a prepare hook.
///
/// It lives master-side because env COMPOSITION is master's job — reading the
/// orchestrator's own environment is not the platform I/O (`spawn`/`try_wait`)
/// that the agent boundary fences off. The agent's `prep` imports THIS function
/// rather than keeping a second copy, which is what keeps the two callers in
/// lockstep (they used to disagree — `compose_env_with_fleet` once used
/// exact-case `std::env::var_os`).
#[cfg(windows)]
pub fn lookup_env(key: &str) -> Option<OsString> {
    std::env::vars_os().find_map(|(candidate, value)| {
        candidate
            .to_str()
            .is_some_and(|candidate| candidate.eq_ignore_ascii_case(key))
            .then_some(value)
    })
}

#[cfg(not(windows))]
pub fn lookup_env(key: &str) -> Option<OsString> {
    std::env::var_os(key)
}

#[cfg(test)]
#[path = "manifest_tests.rs"]
mod manifest_tests;
