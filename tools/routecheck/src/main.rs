//! `routecheck` — the static monolith/split front-door route-parity checker
//! (remediation round 3, Step 7a). It makes the two topologies' front-door route
//! sets STRUCTURALLY EQUAL, catching the class of the inventory dev-grant bug (a
//! route present in the split but not the monolith, or vice versa) for every env
//! config and every future module, with no hand-maintained list.
//!
//! ## How it observes (topiccheck's harness shape)
//! For each deployment profile from `checkmodules` (the single-sourced per-process
//! module lists both topiccheck and requirecheck already build), it constructs every
//! process's real module set, runs the two no-I/O lifecycle phases (`register` →
//! `init`) via `App::build` with a LAZY pool + a no-op durable-events transport, and
//! reads the contribution slots the gateway itself reads:
//!   - `opsapi::SLOT` — the [`opsapi::Operation`]s (the front-door route table),
//!   - `opsapi::BINDING_SLOT` — each op's HTTP↔wire translation,
//!   - `opsapi::LOCAL_SLOT` — the in-process invokers,
//!   - `edge::EDGE_SLOT` — each module's internal-edge registration, applied to a
//!     fresh `edge::Server` (binds no socket) and read back via `Server::methods()`.
//!
//! ## The invariants (per env config)
//! 1. **FRONT-PARITY** — `ops(monolith server) == ops(split gateway-svc)`, compared
//!    as full `Operation` values (method/verb/path/auth/success/retry), symmetric
//!    diff reported. This is the inventory-bug catcher: after the Step-1 rollout
//!    every dev-gated op is contributed unconditionally (gated at the impl), so the
//!    two front sets are equal BY CONSTRUCTION and any conditional contribution
//!    reintroduced in either topology breaks this check.
//! 2. **PER-PROCESS INTEGRITY** — in every process of both profiles, the method set
//!    of contributed `Operation`s equals the method set of contributed `OpBinding`s
//!    (an op without a binding is a silently skipped route); in the monolith, every
//!    op method also has a `LocalOp` invoker (nothing dispatches Remote there).
//! 3. **SPLIT SERVE-PARITY** — every method gateway-svc fronts is actually served on
//!    some domain svc's internal edge (`methods(ops(gateway-svc)) ⊆ ⋃ edge(svc)`),
//!    catching "gate only the front" half-fixes. This is set-membership only —
//!    routecheck no longer needs to (and does not) check for a DUPLICATE edge
//!    method across a process's registrations, because that uniqueness is now a
//!    guarantee enforced by the authority itself: `edge::Server::handle`/
//!    `handle_identity` `panic!` the moment a second capability claims a method
//!    already registered (remediation round 4, Step 1). Since routecheck builds
//!    each profile's real edge registrations through the same `Server::methods()`
//!    path production uses, a duplicate inside one process's `EDGE_SLOT`
//!    contributions PANICS mid-run of routecheck's own profile build — a loud
//!    backtrace surfacing from what looks like a "static checker" is the
//!    intended failure mode here, not a bug in routecheck.
//!
//! ## Env configs (the gate matrix)
//! Both invariant sets are asserted under TWO env configs, sequentially:
//! all-gates-unset (the fail-closed default — this run alone catches the inventory
//! bug class) and all-gates-on. Env mechanics: `std::env::set_var`/`remove_var` are
//! unsound once other threads exist, and `register` reads env — so each config's
//! vars are applied BEFORE that config's tokio runtime is created, each config runs
//! on its OWN runtime (created after the env flip, fully dropped — worker threads
//! joined — before the next flip), and `main` is deliberately NOT `#[tokio::main]`.
//!
//! ## No live DB needed
//! `register`/`init` do no I/O (constraint 8): the pool is `connect_lazy` and never
//! connects — the same trick topiccheck/checkmodules rely on. Exit non-zero on any
//! finding; `cargo test -p routecheck` runs the identical check as a self-test.

use std::collections::BTreeSet;
use std::sync::Arc;

use bus::{AnyTx, Error as BusError, EventContract, HistoryPolicy, SubscriptionSpec, Transport, TxHandler};
use checkmodules::DeploymentProfile;
use lifecycle::{App, Context};
use opsapi::Operation;

/// Dev-default DSN (mirrors CLAUDE.md). Only ever used to build a LAZY pool that
/// never connects — `register`/`init` do no I/O.
const DEFAULT_DSN: &str =
    "postgres://gamebackend:gamebackend@localhost:5432/gamebackend?sslmode=disable";

/// An explicit, hand-curated allowlist of the dev/feature gate env vars read
/// during `register`/`init` that historically gated (or could plausibly gate)
/// which operations a module contributes, paired with their "on" values. This
/// list is NOT derived from the source tree — it is not "every env config",
/// it is exactly the entries below. The check asserts route parity with ALL of
/// them unset (the fail-closed default) and ALL of them set — a contribution
/// conditional on any of these diverges the two front sets in at least one
/// config and fails. Adding a new route-gating env var read in a module's
/// `register`/`init` REQUIRES adding it here — routecheck cannot discover it on
/// its own (see the add-game-module skill's module checklist, which carries the
/// matching reminder). `EPIC_CLIENT_ID` needs only presence (the OIDC verifier
/// constructs lazily, no I/O); the dummy value never dials anything in these
/// two phases.
const GATES: &[(&str, &str)] = &[
    ("ACCOUNTS_DEV_AUTH", "1"),
    ("INVENTORY_DEV_GRANT", "1"),
    ("EPIC_CLIENT_ID", "routecheck-dummy-epic-client-id"),
    ("APIKEYS_DEV_SEED", "1"),
];

/// A `bus::Transport` that ignores everything: nothing is emitted during
/// `register`/`init`, and routecheck does not care about subscriptions (that is
/// topiccheck's job) — it only needs `on_tx` not to panic in a harness process.
struct NoopTransport;

#[async_trait::async_trait]
impl Transport for NoopTransport {
    async fn enqueue_tx(
        &self,
        _tx: AnyTx<'_>,
        _contract: &EventContract,
        _payload: &[u8],
    ) -> Result<(), BusError> {
        Ok(())
    }

    fn subscribe_tx(
        &self,
        _spec: SubscriptionSpec,
        _topic: &str,
        _version: u32,
        _history: Option<HistoryPolicy>,
        _handler: Arc<dyn TxHandler>,
    ) {
    }
}

/// What one process contributed during `register` → `init`, read from the four
/// slots the real gateway/`app::run` read.
struct ProcessRoutes {
    process: &'static str,
    /// Full `Operation` values from `opsapi::SLOT` (the front route table).
    ops: Vec<Operation>,
    /// `Operation.method` set — the routes this process would front.
    op_methods: BTreeSet<String>,
    /// `OpBinding.method` set from `opsapi::BINDING_SLOT`.
    bind_methods: BTreeSet<String>,
    /// `LocalOp.method` set from `opsapi::LOCAL_SLOT`.
    local_methods: BTreeSet<String>,
    /// Methods served on the internal edge: every contributed `EdgeReg` applied to
    /// a fresh `edge::Server` (no socket), read via `Server::methods()`.
    edge_methods: BTreeSet<String>,
}

/// A stable, human-diffable rendering of one [`Operation`] — the unit of the
/// front-parity comparison (full value, not just the method name, so a changed
/// verb/path/auth/success/retry between topologies also fails).
fn op_key(op: &Operation) -> String {
    format!(
        "{} [{} {}] auth={:?} success={} retry={:?}",
        op.method, op.verb, op.path, op.auth, op.success, op.retry_mode
    )
}

/// Builds every process of `profile` (register → init, lazy pool, no-op transport)
/// and reads its contributed route surface. Must run inside a tokio runtime (an
/// in-process `Bus::on` during `init` spawns a task).
fn observe_profile(profile: &DeploymentProfile) -> anyhow::Result<Vec<ProcessRoutes>> {
    let dsn = std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DSN.to_string());
    let mut out = Vec::new();

    for (process_id, mods) in profile.processes() {
        // A LAZY pool per process: never connects, since register/init do no I/O.
        let pool = sqlx::postgres::PgPool::connect_lazy(&dsn)
            .map_err(|e| anyhow::anyhow!("routecheck: {process_id}: build lazy pool: {e}"))?;
        let ctx = Arc::new(Context::with_db_and_transport(pool, Arc::new(NoopTransport)));

        let mut app = App::new(ctx.clone());
        for m in mods {
            app.add(m);
        }
        app.build().map_err(|e| {
            anyhow::anyhow!("routecheck: {process_id}: lifecycle build failed: {e:#}")
        })?;

        let ops: Vec<Operation> = ctx.contributions(opsapi::SLOT);
        let op_methods: BTreeSet<String> = ops.iter().map(|o| o.method.clone()).collect();
        let bind_methods: BTreeSet<String> = ctx
            .contributions::<opsapi::OpBinding>(opsapi::BINDING_SLOT)
            .iter()
            .map(|b| b.method.clone())
            .collect();
        let local_methods: BTreeSet<String> = ctx
            .contributions::<opsapi::LocalOp>(opsapi::LOCAL_SLOT)
            .iter()
            .map(|l| l.method.clone())
            .collect();

        // Apply every contributed EdgeReg to a fresh Server (binds no socket) and
        // read the served set — exactly what `app::run` does on an edge-hosting
        // process. `EdgeReg::apply` is one-shot across clones, which is fine: each
        // process is built once per config run.
        //
        // ONLY for processes that actually host an internal edge — every split
        // domain svc. The monolith "server" and split "gateway-svc" never call
        // `apply_edge_registrations` (their contributions are silently dropped by
        // `app::run`), and `edge::Server` panics on a duplicate method name — so
        // applying the monolith's co-hosted contributions (every admin-page module
        // registers `admin.adminData` for ITS OWN svc's edge) to one Server would
        // manufacture a collision no real process ever sees. Modeling reality
        // exactly: edge-less processes get an empty served set.
        let hosts_internal_edge = process_id != "server" && process_id != "gateway-svc";
        let edge_methods: BTreeSet<String> = if hosts_internal_edge {
            let mut server = edge::Server::new();
            for reg in ctx.contributions::<edge::EdgeReg>(edge::EDGE_SLOT) {
                reg.apply(&mut server);
            }
            server.methods().into_iter().collect()
        } else {
            BTreeSet::new()
        };

        out.push(ProcessRoutes {
            process: process_id,
            ops,
            op_methods,
            bind_methods,
            local_methods,
            edge_methods,
        });
    }
    Ok(out)
}

/// Runs the three invariants over one env config's observations. `label` names the
/// config in every finding.
fn check(label: &str, monolith: &[ProcessRoutes], split: &[ProcessRoutes]) -> Vec<String> {
    let mut findings = Vec::new();

    let server = monolith
        .iter()
        .find(|p| p.process == "server")
        .expect("Monolith profile must contain the \"server\" process");
    let gateway = split
        .iter()
        .find(|p| p.process == "gateway-svc")
        .expect("Split profile must contain the \"gateway-svc\" process");

    // Harness sanity: an empty monolith route table would make every equality below
    // vacuously true — that is a broken harness, not a clean tree.
    if server.ops.is_empty() {
        findings.push(format!(
            "[{label}] HARNESS: monolith \"server\" contributed ZERO operations — the \
             observation harness is broken (vacuous parity proves nothing)"
        ));
        return findings;
    }

    // 1. FRONT-PARITY — full-value symmetric diff between the two front doors.
    let mono_ops: BTreeSet<String> = server.ops.iter().map(op_key).collect();
    let split_ops: BTreeSet<String> = gateway.ops.iter().map(op_key).collect();
    for missing in split_ops.difference(&mono_ops) {
        findings.push(format!(
            "[{label}] FRONT-PARITY: op fronted by split gateway-svc but ABSENT from the \
             monolith front door: {missing}"
        ));
    }
    for missing in mono_ops.difference(&split_ops) {
        findings.push(format!(
            "[{label}] FRONT-PARITY: op fronted by the monolith but ABSENT from split \
             gateway-svc (the inventory-dev-grant bug class — a conditional contribution \
             or a missing stub route): {missing}"
        ));
    }

    // 2. PER-PROCESS INTEGRITY — ops ↔ bindings must pair up in every process.
    for p in monolith.iter().chain(split.iter()) {
        for m in p.op_methods.difference(&p.bind_methods) {
            findings.push(format!(
                "[{label}] INTEGRITY: {}: operation {m:?} has NO OpBinding — the gateway \
                 would skip the route",
                p.process
            ));
        }
        for m in p.bind_methods.difference(&p.op_methods) {
            findings.push(format!(
                "[{label}] INTEGRITY: {}: OpBinding {m:?} has NO Operation — dead binding, \
                 nothing routes to it",
                p.process
            ));
        }
    }
    // Monolith front: every op must dispatch Local (nothing is Remote there).
    for m in server.op_methods.difference(&server.local_methods) {
        findings.push(format!(
            "[{label}] INTEGRITY: server (monolith): operation {m:?} has NO LocalOp \
             invoker — it would dispatch Remote in the monolith"
        ));
    }

    // 3. SPLIT SERVE-PARITY — every fronted method is served on some domain svc's
    // internal edge.
    let served: BTreeSet<&String> = split
        .iter()
        .filter(|p| p.process != "gateway-svc")
        .flat_map(|p| p.edge_methods.iter())
        .collect();
    for m in &gateway.op_methods {
        if !served.contains(m) {
            findings.push(format!(
                "[{label}] SERVE-PARITY: gateway-svc fronts {m:?} but NO domain svc \
                 registers it on its internal edge — the route would 404/503 in the split"
            ));
        }
    }

    findings
}

/// The two env configs, in the mandated order: unset-first (the fail-closed default
/// — this run alone catches the inventory bug class), then all-on.
enum GateConfig {
    AllUnset,
    AllOn,
}

impl GateConfig {
    fn label(&self) -> &'static str {
        match self {
            GateConfig::AllUnset => "gates-unset",
            GateConfig::AllOn => "gates-on",
        }
    }

    /// Applies this config's env. MUST be called while no tokio runtime (or any
    /// other thread that might read env) exists — see the module doc's env
    /// mechanics. Unset is explicit (`remove_var`), so an ambient dev shell with
    /// `INVENTORY_DEV_GRANT=1` exported cannot mask the fail-closed run.
    fn apply(&self) {
        for (key, on_value) in GATES {
            match self {
                GateConfig::AllUnset => std::env::remove_var(key),
                GateConfig::AllOn => std::env::set_var(key, on_value),
            }
        }
    }
}

/// Runs both env configs sequentially, each on its OWN runtime created after the
/// env flip and fully dropped (worker threads joined) before the next flip.
/// Returns every finding across both configs. Shared by `main` and the self-test.
fn run_all() -> anyhow::Result<Vec<String>> {
    let mut findings = Vec::new();
    for config in [GateConfig::AllUnset, GateConfig::AllOn] {
        // Env first, runtime second — set_var/remove_var are only sound while this
        // is the sole thread.
        config.apply();
        let rt = tokio::runtime::Runtime::new()?;
        let config_findings = rt.block_on(async {
            let monolith = observe_profile(&DeploymentProfile::Monolith)?;
            let split = observe_profile(&DeploymentProfile::Split)?;
            let f = check(config.label(), &monolith, &split);
            let mono_n = monolith
                .iter()
                .find(|p| p.process == "server")
                .map_or(0, |p| p.ops.len());
            let gw_n = split
                .iter()
                .find(|p| p.process == "gateway-svc")
                .map_or(0, |p| p.ops.len());
            println!(
                "routecheck [{}]: monolith fronts {mono_n} ops, gateway-svc fronts {gw_n} \
                 — {} finding(s)",
                config.label(),
                f.len()
            );
            anyhow::Ok(f)
        })?;
        findings.extend(config_findings);
        drop(rt); // joins worker threads before the next config's env flip
    }
    Ok(findings)
}

fn main() -> anyhow::Result<()> {
    println!("routecheck: monolith/split front-door route parity (static)\n");
    let findings = run_all()?;
    if findings.is_empty() {
        println!(
            "\nroutecheck: OK — monolith and split front-door route sets are structurally \
             equal, every op has a binding (and a LocalOp in the monolith), and every \
             fronted method is served on a domain svc's edge, under both env configs"
        );
        return Ok(());
    }
    eprintln!("\nroutecheck: FAIL — {} finding(s):", findings.len());
    for f in &findings {
        eprintln!("  - {f}");
    }
    std::process::exit(1);
}

#[cfg(test)]
mod tests;
