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
//! BLOCKING (not advisory) because it is weles's only parity gate and it is
//! pure in-memory (no DB, no rollout), so it is cheap and safe under `--fast`.
//!
//! ## How it normalizes the two composed environments
//!
//! Both sides are fed IDENTICAL dummy [`RuntimeInputs`]/[`FleetInputs`] (same
//! DB URL, same CA paths), so inputs-derived values (`DATABASE_URL`,
//! `EDGE_CA_CERT`, `EDGE_CA_KEY`) match by construction and need NO exclusion.
//! processctl is fed an EMPTY [`EnvironmentSnapshot`] so that (a) its ambient
//! `SERVICE_ENV_ALLOWLIST` passthrough is empty and (b) its `overrideable_env`
//! seam performs no ambient override — leaving the dev-seed values at exactly
//! the fixed values weles hardcodes. The only env keys excluded from the diff
//! are the [`SERVICE_ENV_ALLOWLIST`] passthrough keys (see [`ENV_EXCLUSIONS`]),
//! which weles reads from real ambient env while processctl reads from the
//! injected snapshot — an operator-environment value, never a topology
//! decision. Everything else — `PORT`, `EDGE_ADDR`, `DATABASE_POOL_MAX_CONNECTIONS`,
//! every peer `*_EDGE_ADDR`/`*_HTTP_ADDR`, `PLAYER_EDGE_ADDR`, `TLS_MODE`, and
//! the dev-seed/security keys — is compared in full.

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

/// The ONLY legitimate env-diff exclusions are the ambient
/// `SERVICE_ENV_ALLOWLIST` passthrough keys: weles reads them from the real
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

/// Human-readable rendering of the exclusion set (each allowlist key + the
/// shared reason), printed alongside any drift so the excluded set is auditable
/// rather than invisible.
fn exclusion_policy() -> String {
    let mut out = String::from("env keys excluded from the parity diff (by design):");
    for key in weles::manifest::SERVICE_ENV_ALLOWLIST {
        out.push_str(&format!("\n  {key}: {ALLOWLIST_REASON}"));
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

fn view_from_weles(def: &weles::manifest::ServiceDef) -> ServiceView {
    let env = weles::manifest::compose_env(def, &weles_inputs())
        .into_iter()
        .map(|(k, v)| (k.to_string_lossy().into_owned(), v.to_string_lossy().into_owned()));
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
        env: strip_excluded(env),
    }
}

fn weles_split_views() -> Vec<ServiceView> {
    weles::manifest::split_fleet()
        .iter()
        .map(view_from_weles)
        .collect()
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
    view_from_weles(&weles::manifest::monolith())
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
    diffs.extend(diff_env(topology, label, &weles.env, &processctl.env));
    diffs
}

fn diff_env(
    topology: &str,
    label: &str,
    weles: &BTreeMap<String, String>,
    processctl: &BTreeMap<String, String>,
) -> Vec<String> {
    let mut diffs = Vec::new();
    let keys: BTreeSet<&String> = weles.keys().chain(processctl.keys()).collect();
    for key in keys {
        match (weles.get(key), processctl.get(key)) {
            (Some(w), Some(p)) if w != p => diffs.push(format!(
                "{topology} {label}: env {key} weles={w:?} processctl={p:?}"
            )),
            (Some(_), Some(_)) => {}
            (Some(w), None) => diffs.push(format!(
                "{topology} {label}: env {key} present in weles ({w:?}) but absent in processctl"
            )),
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
