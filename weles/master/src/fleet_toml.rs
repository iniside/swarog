//! The operator-authored `fleet.toml` — weles's fleet definition as strict data
//! rather than a hardcoded Rust table. This module is the ONE place that turns
//! that file into the owned runtime types the supervisor already operates on
//! ([`ServiceDef`], [`Addrs`], [`PrepareCmd`]); nothing downstream
//! learns that the fleet came from TOML.
//!
//! **Strict per the anti-magic rule.** Every deserialized struct is
//! `#[serde(deny_unknown_fields)]` (the same discipline as
//! `agentapi`'s wire structs): a typo'd or renamed key is a loud parse
//! error, never a silently defaulted one. There is NO layering, NO templating,
//! NO fleet-wide `[env]` table — shared values reach a service via the
//! per-fleet `passthrough` list (env KEYS forwarded from weles's own
//! environment), per-service values via each `[service.env]` table.
//!
//! [`load`] parses + converts to owned types; [`validate`] enforces the
//! topology-generic invariants that used to live in verifyctl's deleted
//! `weles-fleet-parity` stage (unique ports, peers name a declared provider
//! that serves the requested kind, edge boot-order). The two are separate so a
//! caller can `--dry-run` validate without spawning, and so parse failures
//! (unknown field, bad `resolve`) and semantic failures (dup port, dangling
//! peer) surface as distinct errors.

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::Deserialize;

use crate::manifest::{self, Addrs, AddrKind, Port, ServiceDef, AGENT_PORT};

/// The whole `fleet.toml`, as authored. Converted to [`Fleet`] by [`load`];
/// this shape exists only to carry the serde/TOML surface (defaults,
/// `deny_unknown_fields`) so the owned runtime types stay free of schema
/// concerns.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FleetToml {
    /// Env KEYS weles forwards from its OWN environment to every service and
    /// prepare hook — the domain-blind channel for opaque operator values
    /// (`DATABASE_URL`, secrets). weles knows the name, never the meaning.
    #[serde(default)]
    passthrough: Vec<String>,
    /// Provisioning commands run — in declared order — BEFORE any service
    /// spawns (CA mint, admin seed). A `[[prepare]]` table each.
    #[serde(default)]
    prepare: Vec<PrepareEntry>,
    /// The fleet processes, in boot order — a `[[service]]` table each. The
    /// Vec order IS the boot order (see [`validate`]'s edge boot-order rule).
    service: Vec<ServiceEntry>,
}

/// A provisioning command the fleet runs BEFORE any service spawns (CA mint,
/// admin seed) — the owned, schema-free runtime DATA the agent's `prep`
/// executes. It lives master-side (the parsed fleet is master's data); only its
/// EXECUTION (`prep::run_prepare`, which needs `platform::spawn`) is agent-side.
/// `PrepareEntry` below is its TOML face — the serde defaults +
/// `deny_unknown_fields` stay a schema concern here rather than being smuggled
/// onto this runtime type.
#[derive(Clone, Debug)]
pub struct PrepareCmd {
    /// Label + the `run_dir/<name>.{out,err}.log` stem.
    pub name: String,
    /// The staged package to execute (`layout.binary(&run)`).
    pub run: String,
    /// Verbatim argv handed to the command.
    pub args: Vec<String>,
    /// Literal env pairs, applied LAST — so an explicit value wins over a
    /// forwarded `passthrough` key of the same name.
    pub env: BTreeMap<String, String>,
    /// Env KEYS forwarded from weles's OWN environment (e.g. `DATABASE_URL` for
    /// the admin seed). weles knows the key name, never its meaning.
    pub passthrough: Vec<String>,
    /// Per-command deadline in seconds; `0` uses the agent's
    /// `prep::DEFAULT_PREPARE_TIMEOUT_SECS`.
    pub timeout_secs: u64,
}

/// One `[[prepare]]` table. Mirrors [`PrepareCmd`] rather than deriving
/// `Deserialize` on it directly: the serde defaults + `deny_unknown_fields` are
/// a schema concern that belongs HERE, not smuggled into the runtime type in
/// `prep.rs` (which would drag `serde` into that module for one caller). Cheap:
/// six fields, one `From` conversion.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PrepareEntry {
    name: String,
    run: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    passthrough: Vec<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    /// Omission → `0`, the sentinel `prep::run_one_prepare` maps to
    /// `prep::DEFAULT_PREPARE_TIMEOUT_SECS` (30). That `0`-mapping is the SOLE
    /// "30-when-unset" authority — this schema does NOT default to 30 itself, so
    /// the two never disagree.
    #[serde(default)]
    timeout_secs: u64,
}

impl From<PrepareEntry> for PrepareCmd {
    fn from(e: PrepareEntry) -> Self {
        PrepareCmd {
            name: e.name,
            run: e.run,
            args: e.args,
            env: e.env,
            passthrough: e.passthrough,
            timeout_secs: e.timeout_secs,
        }
    }
}

/// One `[[service]]` table. Mirrors [`ServiceDef`], with `resolve` + `peer`
/// standing in for the [`Addrs`] enum (which is not a natural TOML shape): a
/// service either declares `resolve = "asks"` (⇒ [`Addrs::Asks`]) or lists
/// `[[service.peer]]` tables (⇒ [`Addrs::Told`]). No `has_db`/`pool_max`: weles
/// is domain-blind, so a DB pool cap is just an ordinary `[service.env]` key.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ServiceEntry {
    name: String,
    pkg: String,
    #[serde(default)]
    provider: Option<String>,
    /// Which node runs this service — a manifest ANNOTATION, not scheduling
    /// (`weles-design.md:245`). It names a node, NOT an address (addresses stay
    /// agent-resolved). Omitted ⇒ `None`; on the single-machine deployment the
    /// only other legal value is the reserved sentinel `"local"`. Any real node
    /// name is rejected by [`validate`] (no node registry exists yet — fail
    /// closed, never a silent no-op).
    #[serde(default)]
    placement: Option<String>,
    /// A literal port (`http_port = 8080`) or the explicit marker
    /// (`http_port = "mint"`) asking the agent to bind a free OS port at spawn —
    /// parsed by [`Port`]'s own deserializer. `deny_unknown_fields` still holds;
    /// this only widens the value from an integer to `integer | "mint"`.
    http_port: Port,
    #[serde(default)]
    edge_port: Option<Port>,
    /// The player-QUIC port — NOT mintable (the one public player front is a
    /// fixed port), so this stays a plain literal.
    #[serde(default)]
    player_port: Option<u16>,
    /// The ONLY accepted value is `"asks"` ([`Addrs::Asks`]); absent means
    /// TOLD. Any other string is a loud error (a typo must never silently fall
    /// through to `Told`).
    #[serde(default)]
    resolve: Option<String>,
    #[serde(default)]
    peer: Vec<PeerEntry>,
    #[serde(default)]
    env: BTreeMap<String, String>,
}

// NOTE: no per-service `passthrough` field. The Step-1 owned `ServiceDef` has
// no home for it (only fleet-wide `passthrough` is threaded into
// `compose_env_with_fleet`), so accepting one here would silently drop the
// authored keys at conversion — the anti-magic smuggle the strict schema
// exists to prevent. Shared forwarded keys go through the fleet-level
// `passthrough`; per-service values through `[service.env]`. If per-service
// passthrough is ever needed, it is a `ServiceDef` + `compose_env` change
// (Step 1/3 scope), not a silently-parsed field here.

/// One `[[service.peer]]` table: which env key carries the address, which
/// provider it resolves, and which of that provider's two ports. `kind` is a
/// TYPED field (`kind = "edge"` | `"http"`, parsed by [`AddrKind`]'s own serde
/// derive), NEVER inferred from `env_key`'s spelling — the same authority
/// inversion the manifest's `Addrs::Told` field exists to prevent.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PeerEntry {
    env_key: String,
    provider: String,
    kind: AddrKind,
}

/// The parsed, converted, owned fleet — the shape the supervisor (Step 3)
/// consumes. Nothing here remembers it came from TOML.
#[derive(Clone, Debug)]
pub struct Fleet {
    /// Provisioning hooks, in run order (before any spawn).
    pub prepare: Vec<PrepareCmd>,
    /// Fleet-wide passthrough env KEYS (forwarded from weles's own env).
    pub passthrough: Vec<String>,
    /// The fleet processes, in boot order.
    pub services: Vec<ServiceDef>,
}

/// Reads and parses `path` into an owned [`Fleet`]. Does NOT [`validate`] — a
/// caller runs that separately (so a parse error and a semantic error stay
/// distinguishable, and `--dry-run` can validate without side effects).
pub fn load(path: &Path) -> Result<Fleet> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("read fleet file {}", path.display()))?;
    parse(&text).with_context(|| format!("in fleet file {}", path.display()))
}

/// The pure string→[`Fleet`] core of [`load`], factored out so unit tests drive
/// it without a tempfile.
fn parse(text: &str) -> Result<Fleet> {
    let raw: FleetToml = toml::from_str(text).context("fleet.toml is not valid strict TOML")?;

    let prepare = raw.prepare.into_iter().map(PrepareCmd::from).collect();

    let mut services = Vec::with_capacity(raw.service.len());
    for entry in raw.service {
        services.push(to_service_def(entry)?);
    }

    Ok(Fleet { prepare, passthrough: raw.passthrough, services })
}

/// Converts one authored [`ServiceEntry`] into an owned [`ServiceDef`],
/// resolving the `resolve`/`peer` pair into the [`Addrs`] enum. A `resolve`
/// value other than `"asks"` is rejected here (never silently treated as
/// TOLD), as is `resolve = "asks"` carrying peers (the addresses would be
/// unread — [`Addrs::Asks`] holds none — which is exactly the silent-drift
/// state the enum exists to make unrepresentable).
fn to_service_def(entry: ServiceEntry) -> Result<ServiceDef> {
    let addrs = match entry.resolve.as_deref() {
        Some("asks") => {
            if !entry.peer.is_empty() {
                bail!(
                    "service {:?}: resolve = \"asks\" but it also declares {} peer(s); an \
                     asking service resolves its peers over the agent and must list none",
                    entry.name,
                    entry.peer.len()
                );
            }
            Addrs::Asks
        }
        Some(other) => bail!(
            "service {:?}: unknown resolve = {:?} (the only accepted value is \"asks\"; \
             omit the key for a told service)",
            entry.name,
            other
        ),
        None => Addrs::Told(
            entry
                .peer
                .into_iter()
                .map(|p| (p.env_key, p.provider, p.kind))
                .collect(),
        ),
    };

    Ok(ServiceDef {
        name: entry.name,
        pkg: entry.pkg,
        provider: entry.provider,
        placement: entry.placement,
        http_port: entry.http_port,
        edge_port: entry.edge_port,
        player_port: entry.player_port,
        addrs,
        env: entry.env,
    })
}

/// Enforces the topology-generic invariants a hand-authored `fleet.toml` must
/// satisfy — the value folded in from verifyctl's deleted `weles-fleet-parity`
/// stage (minus its dropped pg-budget check, which was domain knowledge weles
/// no longer holds):
///
/// (i) every LITERAL http/edge/player port is UNIQUE across the fleet AND
///     distinct from [`AGENT_PORT`] (weles's own endpoint) — two services on one
///     port, or a service squatting the agent's port, is a boot collision, not a
///     runtime surprise. A `"mint"` port has NO literal to collide here: its
///     uniqueness is the agent's BIND-TIME invariant (the mint pass binds a free
///     OS port distinct from every literal, [`AGENT_PORT`], and every prior
///     mint), enforced at runtime rather than in this static check;
/// (ii) every TOLD peer names a `provider` that some service in the fleet
///     provides AND that actually serves the requested [`AddrKind`] — checked
///     through the same [`manifest::service_addr`] the composed env uses, so a
///     `None` there (e.g. `Edge` against an `edge_port: None` service) is the
///     validator's error, with no second copy of the resolution rule;
/// (iii) every `Edge` peer's provider appears STRICTLY EARLIER in `services`
///     than its consumer — the boot-order invariant documented at
///     `manifest.rs:12-17` (an edge peer must already be listening);
/// (iv) every `name` — across the UNION of `[[service]]` and `[[prepare]]` — is
///     UNIQUE. All four share the `run_dir/<name>.{out,err}.log` files AND
///     `name` is the supervisor's per-process state key, so ANY duplicate
///     (service/service, prepare/prepare, or prepare/service) would clobber logs
///     and collide state. One pass over the union subsumes the old
///     prepare-vs-service-only check.
pub fn validate(fleet: &Fleet) -> Result<()> {
    validate_unique_ports(fleet)?;
    validate_unique_names(fleet)?;
    validate_peers(fleet)?;
    validate_no_told_peer_to_replicated_provider(fleet)?;
    validate_no_told_peer_to_mintable_provider(fleet)?;
    validate_placement(fleet)?;
    Ok(())
}

/// (vii) NO Told peer may name a provider whose port OF THAT KIND is `"mint"`. A
///     minted port is not known until the agent binds it at spawn (A4), and a
///     Told peer is handed a LITERAL address in an env var at compose time
///     ([`manifest::compose_env_with_fleet`]) — it cannot carry a not-yet-bound
///     port. Only a `resolve = "asks"` consumer learns a minted address, over
///     the agent's `resolve` (answered from [`manifest::PeerAddrs::from_fleet`],
///     which the agent derives AFTER the mint pass). So a mintable provider must
///     be consumed exclusively via `asks`. This is the counting shape of
///     [`validate_no_told_peer_to_replicated_provider`] — a per-Told-peer scan
///     against a per-provider fact — checked at the (provider, kind) granularity
///     the address itself has: telling a provider's HTTP address is fine while
///     its EDGE port is minted, and vice versa; only the minted kind is
///     unrepresentable in a Told env value. [`validate_peers`] has already
///     proven the provider exists and serves the kind, so the find here cannot
///     dangle (a missing provider is that check's error, not this one's).
fn validate_no_told_peer_to_mintable_provider(fleet: &Fleet) -> Result<()> {
    for svc in &fleet.services {
        for (env_key, provider, kind) in svc.addrs.told() {
            let Some(prov) = fleet
                .services
                .iter()
                .find(|def| def.provider.as_deref() == Some(provider.as_str()))
            else {
                continue;
            };
            let minted = match kind {
                AddrKind::Http => prov.http_port.is_mint(),
                AddrKind::Edge => prov.edge_port.as_ref().is_some_and(Port::is_mint),
            };
            if minted {
                bail!(
                    "service {:?}: Told peer {env_key:?} names provider {provider:?} as \
                     AddrKind::{kind:?}, whose {kind:?} port is \"mint\" — a Told peer receives a \
                     literal address and cannot carry a not-yet-bound minted port; consume a \
                     mintable provider with resolve = \"asks\"",
                    svc.name
                );
            }
        }
    }
    Ok(())
}

/// (vi) NO Told peer may name a provider that MORE THAN ONE service provides.
///     A Told peer carries exactly one address in one env var by design
///     ([`manifest::peer_addr`] — the composed-env path — resolves the provider
///     with a FIRST-match `find`, so a second replica of that provider is
///     invisible to the consumer). The Asks path ([`manifest::PeerAddrs`])
///     correctly returns ALL instances, so a replicated provider MUST be
///     consumed with `resolve = "asks"`. This fires ONLY on an actual Told
///     reference to a multi-instance provider: an Asks-only or unreferenced
///     replicated provider stays legal (nothing silently resolves it wrong).
///     `provider = None` (the monolith, or any un-provided service) is NOT
///     counted — two `None` services are not a replicated provider.
fn validate_no_told_peer_to_replicated_provider(fleet: &Fleet) -> Result<()> {
    // provider name -> how many services provide it. Keyed on Some(name) ONLY:
    // `None` is "provides nothing", never a shared provider key.
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for svc in &fleet.services {
        if let Some(provider) = &svc.provider {
            *counts.entry(provider.as_str()).or_insert(0) += 1;
        }
    }

    for svc in &fleet.services {
        for (env_key, provider, _kind) in svc.addrs.told() {
            let count = counts.get(provider.as_str()).copied().unwrap_or(0);
            if count > 1 {
                bail!(
                    "service {:?}: Told peer {env_key:?} names provider {provider:?}, which \
                     {count} services provide — a Told peer resolves to only the first; use \
                     resolve=\"asks\" for a replicated provider",
                    svc.name
                );
            }
        }
    }
    Ok(())
}

/// (v) `placement` is a manifest ANNOTATION naming which node runs a service
///     (`weles-design.md:245`), NOT an address — the address stays
///     agent-resolved (loopback on one machine, see [`manifest::service_addr`]).
///     On the current single-machine deployment (master ≡ agent, one node) the
///     ONLY legal values are absent (`None`) or the reserved sentinel `"local"`.
///     A real node name has nowhere to resolve — there is no node registry yet —
///     so it is rejected here rather than silently no-oping, per the repo's
///     loud-boot-failure convention. Host derivation from placement is the
///     future multi-machine seam.
fn validate_placement(fleet: &Fleet) -> Result<()> {
    for svc in &fleet.services {
        if let Some(node) = &svc.placement {
            if node != "local" {
                bail!(
                    "service {:?}: placement {node:?} names a node, but multi-node placement is \
                     not supported yet — omit placement or use \"local\"",
                    svc.name
                );
            }
        }
    }
    Ok(())
}

/// LITERAL-port uniqueness only. A `"mint"` field has no operator-authored value
/// to collide — the mint pass guarantees a bound minted port is distinct from
/// every literal, [`AGENT_PORT`], and every prior mint at BIND time (see
/// `supervisor::mint_fleet_ports`), which this static check cannot see.
fn validate_unique_ports(fleet: &Fleet) -> Result<()> {
    // literal port -> the label of the first service to claim it.
    let mut seen: HashMap<u16, String> = HashMap::new();

    for svc in &fleet.services {
        if let Some(port) = svc.http_port.literal() {
            claim_literal_port(&mut seen, &svc.name, "http", port)?;
        }
        if let Some(port) = svc.edge_port.as_ref().and_then(Port::literal) {
            claim_literal_port(&mut seen, &svc.name, "edge", port)?;
        }
        if let Some(port) = svc.player_port {
            claim_literal_port(&mut seen, &svc.name, "player", port)?;
        }
    }
    Ok(())
}

/// Claims one literal port for `label` of `svc_name`, rejecting a clash with
/// [`AGENT_PORT`] or a port a previous field already took.
fn claim_literal_port(
    seen: &mut HashMap<u16, String>,
    svc_name: &str,
    label: &str,
    port: u16,
) -> Result<()> {
    if port == AGENT_PORT {
        bail!(
            "service {svc_name:?}: {label} port {port} collides with weles's own agent port \
             (AGENT_PORT = {AGENT_PORT})"
        );
    }
    let owner = format!("{svc_name}({label})");
    if let Some(prev) = seen.insert(port, owner.clone()) {
        bail!("port {port} is claimed by both {prev} and {owner}");
    }
    Ok(())
}

fn validate_unique_names(fleet: &Fleet) -> Result<()> {
    // Every process name — service OR prepare hook — must be unique: they all
    // share `run_dir/<name>.{out,err}.log` and `name` is the supervisor state
    // key. One pass over the union catches service/service, prepare/prepare, and
    // prepare/service collisions alike.
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let names = fleet
        .services
        .iter()
        .map(|svc| svc.name.as_str())
        .chain(fleet.prepare.iter().map(|hook| hook.name.as_str()));
    for name in names {
        if !seen.insert(name) {
            bail!(
                "name {name:?} is claimed by more than one service/prepare hook; they would \
                 write the same run_dir/{name}.{{out,err}}.log and clobber each other (name is \
                 also the supervisor's per-process state key)"
            );
        }
    }
    Ok(())
}

fn validate_peers(fleet: &Fleet) -> Result<()> {
    for (idx, svc) in fleet.services.iter().enumerate() {
        for (env_key, provider, kind) in svc.addrs.told() {
            // (ii) the provider must exist AND serve this kind. Resolve it
            // through the same service_addr the composed env uses.
            let Some((prov_idx, prov_def)) = fleet
                .services
                .iter()
                .enumerate()
                .find(|(_, def)| def.provider.as_deref() == Some(provider.as_str()))
            else {
                bail!(
                    "service {:?} declares peer {env_key:?} → provider {provider:?}, which no \
                     service in this fleet provides",
                    svc.name
                );
            };
            // Kind-EXISTENCE via `serves_kind` (a port-field-presence question),
            // not `service_addr` (which resolves the port value): the validator
            // runs pre-mint, so a mintable provider must not be tripped into a
            // `Port::resolved()` panic here. A mintable provider still SERVES the
            // kind; whether a Told peer may name it is
            // `validate_no_told_peer_to_mintable_provider`'s call.
            if !manifest::serves_kind(prov_def, *kind) {
                bail!(
                    "service {:?} declares peer {env_key:?} → provider {provider:?} as \
                     AddrKind::{kind:?}, but {provider:?} has no address of that kind (e.g. \
                     Edge against a service with edge_port = None)",
                    svc.name
                );
            }

            // (iii) an edge peer must boot strictly before its consumer.
            if *kind == AddrKind::Edge && prov_idx >= idx {
                bail!(
                    "boot order: service {:?} (position {idx}) declares an Edge peer on \
                     provider {provider:?} (position {prov_idx}), which must appear strictly \
                     earlier so its edge is already listening",
                    svc.name
                );
            }
        }
    }
    Ok(())
}

/// Loads `weles/fleet.split.toml` — the committed 12-process split fixture,
/// resolved from `CARGO_MANIFEST_DIR`'s PARENT so it is found regardless of the
/// test's working directory. `weles-master`'s manifest dir is `weles/master`,
/// so the committed fixtures (which live beside the `weles` crate, shared with
/// verifyctl's `weles-managed-gateway` stage) are one level up. Test-only,
/// shared across this crate's `*_tests.rs` modules: the fixture is the single
/// source of the fleet's shape.
#[cfg(test)]
pub(crate) fn load_split_fixture() -> Fleet {
    let path = weles_dir().join("fleet.split.toml");
    load(&path).expect("weles/fleet.split.toml must load")
}

/// Loads `weles/fleet.monolith.toml` — the committed single-process monolith
/// fixture. See [`load_split_fixture`].
#[cfg(test)]
pub(crate) fn load_monolith_fixture() -> Fleet {
    let path = weles_dir().join("fleet.monolith.toml");
    load(&path).expect("weles/fleet.monolith.toml must load")
}

/// The `weles` crate directory (this crate's parent), where the committed fleet
/// fixtures live.
#[cfg(test)]
fn weles_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("weles-master's manifest dir has a parent (the weles crate)")
        .to_path_buf()
}

#[cfg(test)]
#[path = "fleet_toml_tests.rs"]
mod fleet_toml_tests;
