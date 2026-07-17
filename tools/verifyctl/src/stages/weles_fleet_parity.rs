//! `weles-fleet-parity` — a BLOCKING, pure in-memory verify stage that
//! machine-checks the weles fleet manifest (`weles::manifest`) against the
//! processctl fleet manifest (`tools/processctl/src/fleet.rs`) for the
//! Development flavor.
//!
//! ## Why this stage exists (the authority)
//!
//! weles is zero-sharing: it hand-copies the fleet definition from
//! `tools/processctl/src/fleet.rs` rather than importing it. Until now the two
//! copies were kept in parity ONLY by hand (comments and per-crate goldens),
//! and weles is NOT exercised by split-proof — so a drift between them (a port,
//! a peer `*_EDGE_ADDR`, a pool cap, a dev-seed) had NO gate at all. This stage
//! is that gate: parity is now checked against the real, live source of truth
//! on both sides, not against a hand-maintained comment or a self-golden. It is
//! BLOCKING (not advisory) because it is pure in-memory (no DB, no rollout), so
//! it is cheap and safe under `--fast`.
//!
//! It is no longer weles's ONLY gate: `weles-async-island` and
//! `weles-wire-contract` also constrain weles, and `weles-managed-gateway`
//! boots it for real. That last one matters HERE — see the exclusion below.
//!
//! ## How it normalizes the two composed environments
//!
//! Both sides are fed IDENTICAL dummy [`RuntimeInputs`]/[`FleetInputs`] (same
//! DB URL, same CA paths), so inputs-derived values (`DATABASE_URL`,
//! `EDGE_CA_CERT`, `EDGE_CA_KEY`) match by construction and need NO exclusion.
//! processctl is fed an EMPTY [`EnvironmentSnapshot`] so that (a) its ambient
//! `SERVICE_ENV_ALLOWLIST` passthrough is empty and (b) its `overrideable_env`
//! seam performs no ambient override — leaving the dev-seed values at exactly
//! the fixed values weles hardcodes.
//!
//! ## What is NOT compared — the two exclusions, and what pays for each
//!
//! Read this section hostilely; it is the only place the gate gives ground.
//!
//! 1. **The ambient [`SERVICE_ENV_ALLOWLIST`] passthrough keys** — key-only and
//!    service-blind ([`is_excluded`]/[`strip_excluded`]), because weles reads
//!    them from real ambient env while processctl reads them from the injected
//!    snapshot: their value is the operator's shell, never a topology decision.
//!    [`allowlist_diffs`] separately proves the two allowlists are equal, so a
//!    12th key cannot silently widen this set.
//!
//! 2. **A MANAGED process's peer-address wiring** ([`Delegation`]) — since M1
//!    Step 4 weles spawns gateway-svc [`weles::manifest::Addrs::Asks`]: it is
//!    handed `ORCHESTRATOR_URL` and asks the agent for each peer address, while
//!    processctl still composes the eight address keys for split-proof's
//!    standalone topology, which runs no agent. That divergence is DELIBERATE
//!    and permanent. This exclusion is NOT a key list and NOT a service name: it
//!    is keyed on the def's [`weles::manifest::Addrs`], so it follows the data —
//!    the day gateway-svc stops asking (or another service starts), the set
//!    moves with it. A hardcoded set could not shrink, and a permanently-widened
//!    green gate is worse than a red one.
//!
//! ### What pays for exclusion 2 — TWO stages, one per arm
//!
//! The design's law is that each departure from this gate is paid for by a live
//! proof. Exclusion 2 gives up ground on BOTH sides of the diff, and the two
//! halves are paid for by DIFFERENT stages. Crediting both to one is an unpaid
//! departure with a receipt stapled to it:
//!
//! * The **weles-only** arm (`ORCHESTRATOR_URL`, and weles composing no
//!   addresses at all) is paid for by the BLOCKING `weles-managed-gateway`,
//!   which boots `weles up split` and proves the managed gateway resolved an
//!   address from the agent and USED it.
//! * The **processctl-only** arm (the eight address keys — the bulk of the
//!   ground given up) is NOT covered by that stage: it boots weles's fleet and
//!   never reads a processctl `ServiceSpec`, so it is structurally blind to a
//!   defect in `tools/processctl/src/fleet.rs`'s `gateway_env`. What covers that
//!   is the BLOCKING **split-proof** stage, which boots the processctl fleet and
//!   drives ops through gateway-svc at :8082.
//!
//! Exclusion 2 is narrower than "skip those keys on gateway": only the two
//! ASYMMETRIC directions are excluded, and each only for a pair the delegation
//! actually explains — `ORCHESTRATOR_URL` bearing exactly
//! [`weles::manifest::agent_url`], and a processctl-only key whose OWN
//! `(provider, kind)` ([`claimed_peer`]) the agent's resolve map answers with
//! exactly that address. So a processctl peer address that drifted to a port the
//! agent does not serve still FAILs; so does one pointing at ANOTHER service's
//! real address (the copy-paste class); so does any non-address key, any value
//! mismatch on a key both sides compose, and any of it on a service that is not
//! managed.
//!
//! **Residual gap, recorded not smuggled:** if processctl DROPPED one of the
//! eight keys, it would be in neither map, enter no union, and produce no diff —
//! before Step 4 that was an `absent in processctl` FAIL. Nothing here can catch
//! it: a managed def declares no peer keys, so this gate no longer knows which
//! keys to expect. split-proof is a weak net for it, since `cmd/gateway-svc`'s
//! `ADDR_SPECS` carry standalone defaults equal to the real addresses — a dropped
//! key would silently fall back and still work. Closing it needs a declared
//! expectation of gateway's key set (its `ADDR_SPECS`), which is a separate step.
//!
//! Everything else — `PORT`, `EDGE_ADDR`, `DATABASE_POOL_MAX_CONNECTIONS`, every
//! peer `*_EDGE_ADDR`/`*_HTTP_ADDR` on the eleven told services,
//! `PLAYER_EDGE_ADDR`, `TLS_MODE`, and the dev-seed/security keys — is compared
//! in full.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use crate::{model::Outcome, runner::Context};
use anyhow::Result;

/// Deterministic inputs fed to BOTH manifests. Identical on each side so that
/// `DATABASE_URL`/`EDGE_CA_CERT`/`EDGE_CA_KEY` match by construction (and thus
/// need no exclusion — an inputs-derived value is only a false positive if the
/// two sides get different inputs).
const DUMMY_DB: &str = "postgres://parity:parity@localhost:5432/parity";
const DUMMY_CA_CERT: &str = "dummy-ca-cert.pem";
const DUMMY_CA_KEY: &str = "dummy-ca-key.pem";

/// The only KEY-ONLY, service-blind exclusion (exclusion 1 in the module doc —
/// exclusion 2, a managed process's delegated peer addresses, is per-service and
/// lives in [`Delegation`], deliberately NOT here: this predicate is reached
/// before any diff context exists, so anything excluded here is excluded for
/// EVERY service, and only an ambient passthrough deserves that).
///
/// The ambient `SERVICE_ENV_ALLOWLIST` passthrough keys: weles reads them from the real
/// process environment (`std::env::var_os`) while processctl reads them from an
/// injected snapshot, so their value reflects the operator's live shell — not a
/// topology/wiring decision either manifest makes. The exclusion predicate is
/// DERIVED from `weles::manifest::SERVICE_ENV_ALLOWLIST` (not a parallel
/// hardcode), and [`allowlist_diffs`] separately proves that list equals
/// processctl's — so a 12th allowlist key cannot silently widen the exclusion
/// set, and allowlist drift between the two copies is itself a FAIL. A key is
/// NOT excluded merely because it currently mismatches; an unexplained mismatch
/// is a real drift FAIL.
const ALLOWLIST_REASON: &str = "ambient interpreter/toolchain passthrough — both manifests \
     filter the identical SERVICE_ENV_ALLOWLIST from the live environment; weles reads it from \
     real ambient env, processctl from an injected snapshot, so its value is the operator's shell, \
     not a topology decision";

fn is_excluded(key: &str) -> bool {
    weles::manifest::SERVICE_ENV_ALLOWLIST
        .iter()
        .any(|allowed| allowed.eq_ignore_ascii_case(key))
}

const DELEGATION_REASON: &str = "MANAGED by weles (weles::manifest::Addrs::Asks): weles hands it \
     ORCHESTRATOR_URL and it resolves each peer address from the agent, while processctl composes \
     those addresses at spawn for split-proof's standalone topology, which runs no agent. \
     Deliberate and permanent (M1 Step 4). Paid for by TWO live stages, one per arm: the \
     weles-only arm (ORCHESTRATOR_URL) by weles-managed-gateway, which boots weles's fleet and \
     proves a resolved address is used; the processctl-only arm (the eight address keys) by \
     split-proof, which boots the processctl fleet and drives ops through gateway-svc — \
     weles-managed-gateway never reads a processctl ServiceSpec and cannot cover it. ONLY the \
     asymmetric directions are excluded, and only for pairs the delegation explains: \
     ORCHESTRATOR_URL bearing exactly the agent's URL, and a processctl-only key whose own \
     (provider, kind) the agent's resolve map answers with exactly that address";

/// How a weles-spawned process learns where its peers are — read off the def's
/// [`weles::manifest::Addrs`], which is THE authority for that decision. This is
/// the exclusion key for a managed process's peer-address env (see the module
/// doc, exclusion 2).
///
/// It is deliberately NOT a service name and NOT a key list: keyed on the data,
/// the exclusion evaporates the moment a def stops asking, and it could never be
/// widened by a stale name outliving the decision it described.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Delegation {
    /// TOLD every peer address at spawn — the eleven other split services and
    /// the monolith, and EVERY processctl spec (processctl has no managed mode
    /// at all). Excludes nothing: every env key is compared.
    TellAtSpawn,
    /// ASKS the agent at boot. `url` is the exact value weles composes for
    /// [`weles::manifest::ORCHESTRATOR_URL_ENV`]; `resolve` is the agent's own
    /// map — the same one it would answer this process's `resolve` calls from.
    AskTheAgent { url: String, resolve: weles::manifest::PeerAddrs },
}

/// Reads processctl's OWN key spelling as the `(provider, kind)` that key claims
/// to carry: `CHARACTERS_EDGE_ADDR` → `("characters", Edge)`,
/// `ADMIN_HTTP_ADDR` → `("admin", Http)`.
///
/// Key-name parsing is the exact inversion `weles::manifest::AddrKind`-as-a-field
/// exists to prevent — so read the difference carefully. That ban is on DECIDING
/// where a service lives from a key's spelling. This decides nothing and composes
/// nothing: it interprets the OTHER tool's data in order to ask the agent's map a
/// question it could not otherwise be asked — *would you answer THIS key with
/// THIS address?* The answer still comes from `lookup`, which is the authority.
///
/// Fail-closed at every step: a key that does not parse, or names a provider or
/// kind the agent does not serve, is simply NOT explained by the delegation and
/// stays a FAIL.
fn claimed_peer(key: &str) -> Option<(String, weles::manifest::AddrKind)> {
    let (provider, kind) = key.strip_suffix("_ADDR")?.rsplit_once('_')?;
    let kind = match kind {
        "EDGE" => weles::manifest::AddrKind::Edge,
        "HTTP" => weles::manifest::AddrKind::Http,
        _ => return None,
    };
    Some((provider.to_ascii_lowercase(), kind))
}

impl Delegation {
    /// Does this delegation explain a key present ONLY in weles? Exactly one
    /// pair does: the key carrying the delegation, bearing the agent's real URL.
    /// A bogus URL, or any other weles-only key, is drift.
    fn explains_weles_only(&self, key: &str, value: &str) -> bool {
        match self {
            Delegation::TellAtSpawn => false,
            Delegation::AskTheAgent { url, .. } => {
                key == weles::manifest::ORCHESTRATOR_URL_ENV && value == url
            }
        }
    }

    /// Does this delegation explain a key present ONLY in processctl? Only if the
    /// agent would answer THAT key's own `(provider, kind)` with EXACTLY that
    /// address — i.e. processctl is telling the process the same thing weles
    /// arranges for it to resolve.
    ///
    /// Keyed on `(provider, kind)`, NOT on "is this any address in the fleet".
    /// The any-address form (this arm's first cut) accepted any fleet address
    /// under any key, which swallowed the whole COPY-PASTE class — gateway's
    /// `CHARACTERS_EDGE_ADDR` pointing at inventory's edge, or `ADMIN_HTTP_ADDR`
    /// at gateway's own port, are all resolvable addresses and would have gone
    /// green. That class is likelier than the typo class, and it is exactly what
    /// a parity gate is for.
    fn explains_processctl_only(&self, key: &str, value: &str) -> bool {
        match self {
            Delegation::TellAtSpawn => false,
            Delegation::AskTheAgent { resolve, .. } => claimed_peer(key)
                .is_some_and(|(provider, kind)| {
                    resolve.lookup(&provider, kind).iter().any(|addr| addr == value)
                }),
        }
    }
}

/// Human-readable rendering of the exclusion set, printed alongside any drift so
/// the excluded set is auditable rather than invisible: every allowlist key, plus
/// every service whose peer-address env is delegated to the agent. Both halves
/// are DERIVED, so this print can never claim an exclusion the diff does not make
/// (or hide one it does).
fn exclusion_policy() -> String {
    let mut out = String::from("env keys excluded from the parity diff (by design):");
    for key in weles::manifest::SERVICE_ENV_ALLOWLIST {
        out.push_str(&format!("\n  {key}: {ALLOWLIST_REASON}"));
    }
    for view in weles_split_views().iter().chain(std::iter::once(&weles_monolith_view())) {
        if let Delegation::AskTheAgent { url, .. } = &view.delegation {
            out.push_str(&format!(
                "\n  {}: peer addresses — {DELEGATION_REASON} ({} = {url})",
                view.name,
                weles::manifest::ORCHESTRATOR_URL_ENV,
            ));
        }
    }
    out
}

/// A manifest entry normalized to a topology-comparable shape, with the
/// excluded env keys already stripped. Built from either side so the diff logic
/// is agnostic to which manifest produced it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ServiceView {
    name: String,
    pkg: String,
    http_port: u16,
    edge_port: Option<u16>,
    player_port: Option<u16>,
    has_db: bool,
    pool_max: u32,
    /// Dedicated Postgres sessions held OUTSIDE the pool (plane workers +
    /// listeners, plus the scheduler's per-fire connection). Both manifests
    /// HAND-COPY the constants this is built from — comparing it closes the
    /// "Mirrors tools/processctl/src/fleet.rs::…" gap on the budget arithmetic.
    dedicated: u32,
    /// How this process learns its peers, read off `weles::manifest::Addrs` —
    /// the exclusion key (module doc, exclusion 2). The ONE field [`diff_view`]
    /// does not compare, because it IS the sanctioned divergence: comparing it
    /// would assert a parity Step 4 removed on purpose.
    ///
    /// **Only the weles side is derived, and only the weles side is read.**
    /// [`view_from_processctl`] fills a CONSTANT [`Delegation::TellAtSpawn`]
    /// here: it is derived from nothing and nothing reads it ([`diff_view`]
    /// passes the weles view's). It exists solely because both manifests are
    /// normalized into one view type. Do NOT read processctl's copy as a claim
    /// about processctl — if processctl ever grew a managed mode, this field
    /// would keep saying `TellAtSpawn`.
    delegation: Delegation,
    env: BTreeMap<String, String>,
}

fn weles_inputs() -> weles::manifest::RuntimeInputs {
    weles::manifest::RuntimeInputs {
        database_url: DUMMY_DB.to_string(),
        ca_cert: PathBuf::from(DUMMY_CA_CERT),
        ca_key: PathBuf::from(DUMMY_CA_KEY),
    }
}

fn processctl_inputs() -> processctl::FleetInputs {
    processctl::FleetInputs {
        database_url: DUMMY_DB.to_string(),
        edge_ca_cert: PathBuf::from(DUMMY_CA_CERT),
        edge_ca_key: PathBuf::from(DUMMY_CA_KEY),
    }
}

fn strip_excluded(env: impl IntoIterator<Item = (String, String)>) -> BTreeMap<String, String> {
    env.into_iter()
        .filter(|(key, _)| !is_excluded(key))
        .collect()
}

/// `fleet` is the topology `def` belongs to — passed in by the caller that chose
/// it, exactly as `weles::manifest::compose_env_with_fleet` requires, so the
/// resolve map this reads a managed process's [`Delegation`] from is built from
/// the SAME slice weles composes that process's env from. Deriving it from
/// `split_fleet()` regardless would let the monolith's view claim addresses for
/// twelve processes that are not running.
fn view_from_weles(
    def: &weles::manifest::ServiceDef,
    fleet: &[weles::manifest::ServiceDef],
) -> ServiceView {
    let env = weles::manifest::compose_env(def, &weles_inputs())
        .into_iter()
        .map(|(k, v)| (k.to_string_lossy().into_owned(), v.to_string_lossy().into_owned()));
    // Keyed on the def's own Addrs — the authority for this decision — never on
    // a service name or a key list. See the module doc, exclusion 2.
    let delegation = match def.addrs {
        weles::manifest::Addrs::Told(_) => Delegation::TellAtSpawn,
        weles::manifest::Addrs::Asks => Delegation::AskTheAgent {
            url: weles::manifest::agent_url(),
            resolve: weles::manifest::PeerAddrs::from_fleet(fleet),
        },
    };
    // Same authority weles's own `validate_pg_budget` charges against
    // PG_SESSION_BUDGET — so a drift here is exactly what would make weles
    // under-reserve the shared Postgres.
    let (_pool, dedicated) = weles::manifest::service_pg_budget(def);
    ServiceView {
        name: def.name.to_string(),
        pkg: def.pkg.to_string(),
        http_port: def.http_port,
        edge_port: def.edge_port,
        player_port: def.player_port,
        has_db: def.has_db,
        pool_max: def.pool_max,
        dedicated,
        delegation,
        env: strip_excluded(env),
    }
}

fn view_from_processctl(spec: &processctl::ServiceSpec) -> ServiceView {
    let env = spec
        .env
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()));
    ServiceView {
        name: spec.name.to_string(),
        pkg: spec.executable_package.to_string(),
        http_port: spec.http_port,
        edge_port: spec.edge_port,
        player_port: spec.player_port,
        // processctl has no `has_db` field: a DB-backed process is exactly one
        // that reserves a pool (gateway-svc reserves 0). This mirrors weles's
        // `has_db` for every service.
        has_db: spec.pool_budget.pool_max > 0,
        pool_max: spec.pool_budget.pool_max,
        dedicated: spec.pool_budget.dedicated,
        // A CONSTANT, not a derivation, and nothing reads it — see the field's
        // doc. processctl has no managed mode to read one from: it composes every
        // peer address at spawn for every service, because split-proof's
        // standalone topology runs no agent to ask.
        delegation: Delegation::TellAtSpawn,
        env: strip_excluded(env),
    }
}

fn weles_split_views() -> Vec<ServiceView> {
    let fleet = weles::manifest::split_fleet();
    fleet.iter().map(|def| view_from_weles(def, &fleet)).collect()
}

fn processctl_split_views() -> Vec<ServiceView> {
    let inputs = processctl_inputs();
    let environment = processctl::EnvironmentSnapshot::from_values(std::iter::empty::<(String, String)>());
    let fleet = processctl::game_backend_fleet_with_environment(
        &inputs,
        processctl::FleetFlavor::Development,
        &environment,
    );
    fleet.services().iter().map(view_from_processctl).collect()
}

fn weles_monolith_view() -> ServiceView {
    let monolith = weles::manifest::monolith();
    view_from_weles(&monolith, std::slice::from_ref(&monolith))
}

fn processctl_monolith_view() -> ServiceView {
    let inputs = processctl_inputs();
    let environment = processctl::EnvironmentSnapshot::from_values(std::iter::empty::<(String, String)>());
    let spec = processctl::game_backend_monolith(
        &inputs,
        processctl::FleetFlavor::Development,
        &environment,
    );
    view_from_processctl(&spec)
}

/// processctl's explicit dependency graph for the split, keyed by service name.
/// weles expresses the same graph implicitly by boot-order position, so parity
/// is: every processctl dependency must appear strictly earlier than its
/// dependent in weles's boot order (see [`dependency_order_diffs`]).
fn processctl_split_dependencies() -> Vec<(String, Vec<String>)> {
    let inputs = processctl_inputs();
    let environment = processctl::EnvironmentSnapshot::from_values(std::iter::empty::<(String, String)>());
    let fleet = processctl::game_backend_fleet_with_environment(
        &inputs,
        processctl::FleetFlavor::Development,
        &environment,
    );
    fleet
        .services()
        .iter()
        .map(|spec| {
            (
                spec.name.to_string(),
                spec.dependencies.iter().map(|d| d.to_string()).collect(),
            )
        })
        .collect()
}

/// Compares two normalized views field by field. `compare_name` is false for
/// the monolith, whose display label legitimately differs (weles labels it by
/// its package `server`; processctl labels it `monolith`) — the authoritative
/// identity there is the package, which IS compared.
fn diff_view(topology: &str, weles: &ServiceView, processctl: &ServiceView, compare_name: bool) -> Vec<String> {
    let mut diffs = Vec::new();
    let label = &weles.pkg;
    if compare_name && weles.name != processctl.name {
        diffs.push(format!(
            "{topology} {label}: name weles={:?} processctl={:?}",
            weles.name, processctl.name
        ));
    }
    if weles.pkg != processctl.pkg {
        diffs.push(format!(
            "{topology} {label}: pkg weles={:?} processctl={:?}",
            weles.pkg, processctl.pkg
        ));
    }
    if weles.http_port != processctl.http_port {
        diffs.push(format!(
            "{topology} {label}: http_port weles={} processctl={}",
            weles.http_port, processctl.http_port
        ));
    }
    if weles.edge_port != processctl.edge_port {
        diffs.push(format!(
            "{topology} {label}: edge_port weles={:?} processctl={:?}",
            weles.edge_port, processctl.edge_port
        ));
    }
    if weles.player_port != processctl.player_port {
        diffs.push(format!(
            "{topology} {label}: player_port weles={:?} processctl={:?}",
            weles.player_port, processctl.player_port
        ));
    }
    if weles.has_db != processctl.has_db {
        diffs.push(format!(
            "{topology} {label}: has_db weles={} processctl={}",
            weles.has_db, processctl.has_db
        ));
    }
    if weles.pool_max != processctl.pool_max {
        diffs.push(format!(
            "{topology} {label}: pool_max weles={} processctl={}",
            weles.pool_max, processctl.pool_max
        ));
    }
    if weles.dedicated != processctl.dedicated {
        diffs.push(format!(
            "{topology} {label}: dedicated (Postgres sessions) weles={} processctl={}",
            weles.dedicated, processctl.dedicated
        ));
    }
    // `delegation` is deliberately NOT compared — it is the sanctioned
    // divergence itself (see the field's doc). The WELES side is passed DOWN
    // instead (processctl's copy is an unread constant), because `diff_env` is
    // the only place with both the service and the key in hand, and the exclusion
    // is per-service AND per-key. weles is the manifest that departed, so weles's
    // side is the one that may explain a difference.
    diffs.extend(diff_env(topology, label, &weles.delegation, &weles.env, &processctl.env));
    diffs
}

fn diff_env(
    topology: &str,
    label: &str,
    delegation: &Delegation,
    weles: &BTreeMap<String, String>,
    processctl: &BTreeMap<String, String>,
) -> Vec<String> {
    let mut diffs = Vec::new();
    let keys: BTreeSet<&String> = weles.keys().chain(processctl.keys()).collect();
    for key in keys {
        match (weles.get(key), processctl.get(key)) {
            // A key BOTH sides compose is always compared: a delegation explains
            // an address weles stopped composing, never a value it still does.
            (Some(w), Some(p)) if w != p => diffs.push(format!(
                "{topology} {label}: env {key} weles={w:?} processctl={p:?}"
            )),
            (Some(_), Some(_)) => {}
            (Some(w), None) if delegation.explains_weles_only(key, w) => {}
            (Some(w), None) => diffs.push(format!(
                "{topology} {label}: env {key} present in weles ({w:?}) but absent in processctl"
            )),
            (None, Some(p)) if delegation.explains_processctl_only(key, p) => {}
            (None, Some(p)) => diffs.push(format!(
                "{topology} {label}: env {key} present in processctl ({p:?}) but absent in weles"
            )),
            (None, None) => unreachable!("key came from one of the two maps"),
        }
    }
    diffs
}

/// Diffs the two split fleets: the service SET, each shared service's fields +
/// env, and the boot-order-vs-dependency-graph consistency.
fn diff_split(
    weles: &[ServiceView],
    processctl: &[ServiceView],
    processctl_deps: &[(String, Vec<String>)],
) -> Vec<String> {
    let mut diffs = Vec::new();
    let weles_names: BTreeSet<&str> = weles.iter().map(|v| v.name.as_str()).collect();
    let processctl_names: BTreeSet<&str> = processctl.iter().map(|v| v.name.as_str()).collect();

    for name in weles_names.difference(&processctl_names) {
        diffs.push(format!("split: service {name} in weles but not in processctl"));
    }
    for name in processctl_names.difference(&weles_names) {
        diffs.push(format!("split: service {name} in processctl but not in weles"));
    }

    for w in weles {
        if let Some(p) = processctl.iter().find(|p| p.name == w.name) {
            diffs.extend(diff_view("split", w, p, true));
        }
    }

    diffs.extend(dependency_order_diffs(weles, processctl_deps));
    diffs
}

/// weles's boot order (its `split_fleet` Vec position) must honor processctl's
/// explicit dependency graph: every dependency appears strictly earlier than
/// its dependent. This is how the two express the SAME ordering constraint —
/// weles implicitly by position, processctl explicitly by a `dependencies` list.
fn dependency_order_diffs(
    weles: &[ServiceView],
    processctl_deps: &[(String, Vec<String>)],
) -> Vec<String> {
    let mut diffs = Vec::new();
    let index = |name: &str| weles.iter().position(|v| v.name == name);
    for (service, deps) in processctl_deps {
        let Some(service_index) = index(service) else {
            // A missing service is already reported by the set diff; skip here.
            continue;
        };
        for dep in deps {
            match index(dep) {
                Some(dep_index) if dep_index < service_index => {}
                Some(_) => diffs.push(format!(
                    "split: weles boot order violates dependency — {dep} must appear before {service}"
                )),
                None => diffs.push(format!(
                    "split: processctl dependency {dep} of {service} is absent from the weles fleet"
                )),
            }
        }
    }
    diffs
}

/// Compares the two HAND-COPIED `PG_SESSION_BUDGET` constants. weles labels its
/// copy "Mirrors tools/processctl/src/fleet.rs::PG_SESSION_BUDGET" with no
/// machine check; this is that check. A drift here means one manifest computes
/// a stale fleet-fit total against the shared local Postgres.
fn budget_diffs(weles: u32, processctl: u32) -> Vec<String> {
    if weles == processctl {
        Vec::new()
    } else {
        vec![format!(
            "budget: PG_SESSION_BUDGET weles={weles} processctl={processctl} \
             (weles's hand-copied 'Mirrors …' constant drifted)"
        )]
    }
}

/// Compares the two HAND-COPIED `SERVICE_ENV_ALLOWLIST` slices (order-insensitive:
/// the passthrough set, not its order, is the contract). Because processctl is
/// fed an empty snapshot, allowlist content never reaches the per-service env
/// diff — so without this explicit check the two copies could silently diverge
/// (weles adds `APPDATA`, or drops `WINDIR`, and processctl does not).
fn allowlist_diffs(weles: &[&str], processctl: &[&str]) -> Vec<String> {
    let weles_set: BTreeSet<&str> = weles.iter().copied().collect();
    let processctl_set: BTreeSet<&str> = processctl.iter().copied().collect();
    let mut diffs = Vec::new();
    for key in weles_set.difference(&processctl_set) {
        diffs.push(format!(
            "allowlist: SERVICE_ENV_ALLOWLIST key {key} in weles but not processctl"
        ));
    }
    for key in processctl_set.difference(&weles_set) {
        diffs.push(format!(
            "allowlist: SERVICE_ENV_ALLOWLIST key {key} in processctl but not weles"
        ));
    }
    diffs
}

/// The full parity diff over both topologies against the real HEAD manifests,
/// PLUS the fleet-wide hand-copied constants (session budget + allowlist).
fn parity_diffs() -> Vec<String> {
    let mut diffs = diff_split(
        &weles_split_views(),
        &processctl_split_views(),
        &processctl_split_dependencies(),
    );
    // The monolith's display name legitimately differs (weles: "server",
    // processctl: "monolith"); the authoritative package identity is compared.
    diffs.extend(diff_view(
        "monolith",
        &weles_monolith_view(),
        &processctl_monolith_view(),
        false,
    ));
    diffs.extend(budget_diffs(
        weles::manifest::PG_SESSION_BUDGET,
        processctl::PG_SESSION_BUDGET,
    ));
    diffs.extend(allowlist_diffs(
        weles::manifest::SERVICE_ENV_ALLOWLIST,
        processctl::SERVICE_ENV_ALLOWLIST,
    ));
    diffs
}

pub fn run(ctx: &mut Context<'_>) -> Result<Outcome> {
    let diffs = parity_diffs();
    if diffs.is_empty() {
        return Ok(Outcome::Pass);
    }
    eprintln!(
        "verifyctl: weles<->processctl fleet parity drift ({} finding(s)):",
        diffs.len()
    );
    for diff in &diffs {
        eprintln!("  {diff}");
        ctx.note(diff)?;
    }
    // Surface the exclusion policy so an operator can see what the stage did NOT
    // compare (and thus rule out a false positive from an ambient passthrough).
    let policy = exclusion_policy();
    eprintln!("{policy}");
    ctx.note(&policy)?;
    Ok(Outcome::Fail)
}

#[cfg(test)]
#[path = "weles_fleet_parity_tests.rs"]
mod weles_fleet_parity_tests;
