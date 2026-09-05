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
//! 1. **FRONT-PARITY** — `local_front_ops(monolith server) == describe_union(split domain
//!    svcs)`, compared as full `Operation` values (method/verb/path/auth/success/retry_mode),
//!    symmetric diff reported. The MONOLITH side is its locally-contributed front ops
//!    (`opsapi::SLOT` at init, unchanged). The SPLIT side is the DESCRIBE-UNION under D2
//!    routing-as-data: gateway-svc contributes NO `opsapi::SLOT` ops at register/init — it
//!    builds its route table at START from each peer's runtime `__describe`, dials routecheck
//!    never runs — so its static SLOT set is empty. The faithful split front set is instead
//!    the union of every split domain svc's `__describe` manifest, each `OpManifest`
//!    reconstructed to an `Operation` by `opsapi::databind::operation` — the SAME builder the
//!    runtime gateway uses, so parity is compared against the ACTUAL operations it will front,
//!    one authority, no drift. This is the inventory-bug catcher AND the never-monolith-only
//!    guard: a dev-gated op must be contributed unconditionally (monolith) AND described by
//!    its svc (split), so a conditional/forgotten contribution on EITHER side breaks parity in
//!    the corresponding direction. (Invariants 3 SERVE-PARITY and 4 OVERLAP likewise read the
//!    split front set from this describe-union, since that is what the D2 gateway fronts.)
//! 2. **PER-PROCESS INTEGRITY** — in every process of both profiles, the method set
//!    of contributed `Operation`s equals the method set of contributed `OpBinding`s
//!    (an op without a binding is a silently skipped route); in the monolith, every
//!    op method also has a `LocalOp` invoker (nothing dispatches Remote there).
//! 3. **SPLIT SERVE-PARITY** — every method gateway-svc fronts is actually served on
//!    some DOMAIN svc's internal edge (`methods(ops(gateway-svc)) ⊆ ⋃ edge(svc)`;
//!    gateway-svc's own edge is excluded from the union — it serves the push
//!    backplane's `push.deliver` and no `#[http]` op, and the front must not front a
//!    route to itself),
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
//! 4. **OVERLAP** — no two `Operation`s fronted by the SAME process (the monolith
//!    `server` or split `gateway-svc` — the only two processes that ever build an
//!    `opsapi`-driven route table) may accept the same verb + request set, via the
//!    SAME `opsapi::pattern_overlaps` predicate `gateway::RouteTable::build` uses at
//!    startup. This is a static twin of that startup check, not a substitute for it:
//!    routecheck runs `register`/`init` only (constraint 8, no I/O) and never calls
//!    `RouteTable::build` itself, so without this invariant an overlapping pair would
//!    pass `cargo test -p routecheck` clean and only fail the moment `gateway-svc`
//!    or the monolith actually boots. `GET /x/{id}` vs `GET /x/me` is the
//!    motivating case: their SHAPES differ (a literal vs a wildcard at one
//!    position), so the narrower "identical shape" notion this predicate replaced
//!    would have missed it — see [`opsapi::pattern_overlaps`]'s doc.
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
use opsapi::{Operation, parse_pattern, pattern_overlaps};

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
    /// Method set drained from `opsapi::DESCRIBE_SLOT` — the `#[http]` ops this process
    /// would serve under the ONE reserved `__describe` op (routing-as-data SERVE side,
    /// D1.5b). The forget-guard (`describe_completeness_findings`) diffs this against the
    /// `#[http]` subset of `edge_methods` (what the process actually serves on its edge,
    /// NOT the stub-polluted `op_methods`): a `#[http]` module that forgot
    /// `ctx.contribute(DESCRIBE_SLOT, …)` still serves the op on its edge but leaves this
    /// set missing it → a managed gateway (D2) would build a dead route for it.
    describe_methods: BTreeSet<String>,
    /// Full `Operation`s reconstructed from this process's `DESCRIBE_SLOT` manifest via
    /// `opsapi::databind::operation` — the SAME builder the D2 managed gateway uses to turn a
    /// fetched `__describe` into its route table. The UNION of these over the split's domain
    /// svcs is what the D2 gateway-svc actually fronts at runtime (its routes come from
    /// describe at start, not from `opsapi::SLOT` at register/init — so `ops`/`op_methods` are
    /// empty for gateway-svc), and is the SPLIT side of FRONT-PARITY under routing-as-data.
    describe_ops: Vec<Operation>,
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
        // ONLY for processes that actually host an internal edge — every split svc,
        // gateway-svc INCLUDED: `cmd/gateway-svc` passes a real `edge::Server` so the
        // gateway module's `push.deliver` face is served there (the push backplane's
        // inbound half). The monolith "server" passes `None`, so `app::run` silently
        // drops its contributions; and since `edge::Server` panics on a duplicate method
        // name, applying the monolith's co-hosted contributions (every admin-page module
        // registers `admin.adminData` for ITS OWN svc's edge) to one Server would
        // manufacture a collision no real process ever sees. Modeling reality exactly:
        // only the edge-less monolith gets an empty served set.
        let hosts_internal_edge = process_id != "server";
        let edge_methods: BTreeSet<String> = if hosts_internal_edge {
            let mut server = edge::Server::new();
            for reg in ctx.contributions::<edge::EdgeReg>(edge::EDGE_SLOT) {
                reg.apply(&mut server);
            }
            server.methods().into_iter().collect()
        } else {
            BTreeSet::new()
        };

        // The `#[http]` manifest this process would serve under the ONE reserved
        // `__describe` op — the concat of every module's `DESCRIBE_SLOT` contribution,
        // exactly what `app::run` serves on an edge-hosting process. From this ONE manifest we
        // derive both the method set (the forget-guard diffs it against `op_methods`) and the
        // full `Operation`s (`describe_ops`), reconstructed via the SAME `opsapi::databind`
        // builder the D2 gateway uses — so FRONT-PARITY compares against the ACTUAL operations
        // the runtime gateway will front, not a re-derivation that could drift.
        let describe_manifest = opsapi::DescribeManifest::concat(
            ctx.contributions::<opsapi::DescribeManifest>(opsapi::DESCRIBE_SLOT),
        );
        let describe_methods: BTreeSet<String> =
            describe_manifest.ops.iter().map(|o| o.method.clone()).collect();
        let describe_ops: Vec<Operation> =
            describe_manifest.ops.iter().map(opsapi::databind::operation).collect();

        out.push(ProcessRoutes {
            process: process_id,
            ops,
            op_methods,
            bind_methods,
            local_methods,
            edge_methods,
            describe_methods,
            describe_ops,
        });
    }
    Ok(out)
}

/// Invariant 4 (OVERLAP): pairwise-scans one process's contributed `Operation`s for
/// a same-verb, overlapping-path pair, via the SAME `opsapi::pattern_overlaps`
/// predicate `gateway::RouteTable::build` uses at real startup. `O(n^2)` over one
/// process's op count (a few dozen today) — fine for a static checker.
fn overlap_findings(label: &str, process: &str, ops: &[Operation]) -> Vec<String> {
    let mut findings = Vec::new();
    for i in 0..ops.len() {
        for j in (i + 1)..ops.len() {
            let a = &ops[i];
            let b = &ops[j];
            if !a.verb.eq_ignore_ascii_case(&b.verb) {
                continue;
            }
            if pattern_overlaps(&parse_pattern(&a.path), &parse_pattern(&b.path)) {
                findings.push(format!(
                    "[{label}] OVERLAP: {process}: route {} {:?} and {} {:?} may overlap \
                     — the same request could match both (methods {:?} and {:?}) — \
                     this would fail gateway::RouteTable::build at real startup",
                    a.verb, a.path, b.verb, b.path, a.method, b.method
                ));
            }
        }
    }
    findings
}

/// Invariant 5 (DESCRIBE-COMPLETE), the forget-guard for routing-as-data's SERVE side
/// (D1.5b): every `#[http]` op a process SERVES on its internal edge (`served_http`) MUST
/// appear in that process's `__describe` manifest (`describe_methods`, drained from
/// `opsapi::DESCRIBE_SLOT`) — and vice versa.
///
/// `served_http` is `edge_methods ∩ global_http`, NOT the process's raw `op_methods`: a
/// domain svc that holds a peer `remote::Stub` also gets that peer's HTTP route bindings
/// contributed to `opsapi::SLOT` (inventory-svc fronts `characters.*` because it
/// `require`s `characters::Ownership`), so `op_methods` is polluted by ops the process
/// CONSUMES but does not serve. The ops a process actually serves on its edge — what a
/// managed gateway dialing it would dispatch there — is the `#[http]` subset of
/// `edge_methods`. A NEW `#[http]` module that forgot
/// `ctx.contribute(opsapi::DESCRIBE_SLOT, …)` in its `init` still registers its edge
/// handlers (via `EDGE_SLOT`), so `served_http` carries the method while
/// `describe_methods` does not → a per-method FORGOT finding, fail-closed, BEFORE a
/// managed gateway (D2) silently builds a dead route for it. The reverse diff (a describe
/// entry the process does not actually serve) catches a stale/foreign manifest.
fn describe_completeness_findings(
    label: &str,
    process: &str,
    served_http: &BTreeSet<String>,
    describe_methods: &BTreeSet<String>,
) -> Vec<String> {
    let mut findings = Vec::new();
    for m in served_http.difference(describe_methods) {
        findings.push(format!(
            "[{label}] DESCRIBE-FORGOT: {process}: #[http] op {m:?} is served on this \
             process's internal edge but ABSENT from its __describe manifest — its module \
             forgot `ctx.contribute(opsapi::DESCRIBE_SLOT, …)` in `init`; a managed gateway \
             (D2) would build a DEAD route (UnknownMethod → NotFound) for it"
        ));
    }
    for m in describe_methods.difference(served_http) {
        findings.push(format!(
            "[{label}] DESCRIBE-EXTRA: {process}: __describe carries {m:?} but this process \
             does NOT serve it on its internal edge — a stale or foreign describe entry"
        ));
    }
    findings
}

/// Runs the invariants over one env config's observations. `label` names the
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

    // The SPLIT front door's fronted-op set under D2 routing-as-data: the DESCRIBE-UNION —
    // the union of every split DOMAIN svc's `__describe` manifest (each `OpManifest`
    // reconstructed to an `Operation` by `opsapi::databind::operation`, the SAME builder the
    // gateway's own describe router uses at start). gateway-svc contributes NO `opsapi::SLOT`
    // ops at register/init (its route table is built at start from these very manifests), so
    // `gateway.ops`/`gateway.op_methods` are empty — they are NOT the split front set here.
    // The union is over `process != "gateway-svc"` (the domain svcs that serve `__describe`);
    // gateway-svc describes no op of its own (it contributes nothing to `DESCRIBE_SLOT`).
    // This is exactly what the runtime split front door fronts, so it is the faithful
    // SPLIT side of every front-door invariant below (1/3/4).
    let split_front_ops: Vec<Operation> = split
        .iter()
        .filter(|p| p.process != "gateway-svc")
        .flat_map(|p| p.describe_ops.iter().cloned())
        .collect();
    let split_front_methods: BTreeSet<String> =
        split_front_ops.iter().map(|o| o.method.clone()).collect();

    // 1. FRONT-PARITY — full-value symmetric diff: the monolith's LOCAL front ops
    // (`server.ops`, contributed at init) vs the split's DESCRIBE-UNION front ops. Same op
    // identity/verb/path/auth/success/retry compared as before; only the split side's SOURCE
    // changed (register-time slots → runtime describe) to match how the D2 gateway routes.
    let mono_ops: BTreeSet<String> = server.ops.iter().map(op_key).collect();
    let split_ops: BTreeSet<String> = split_front_ops.iter().map(op_key).collect();
    for missing in split_ops.difference(&mono_ops) {
        findings.push(format!(
            "[{label}] FRONT-PARITY: op fronted by split gateway-svc (via a domain svc's \
             __describe) but ABSENT from the monolith front door: {missing}"
        ));
    }
    for missing in mono_ops.difference(&split_ops) {
        findings.push(format!(
            "[{label}] FRONT-PARITY: op fronted by the monolith but ABSENT from the split \
             describe-union (the inventory-dev-grant bug class — a conditional contribution, \
             or a domain svc that forgot to describe the op so the gateway can't route it): \
             {missing}"
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
    // internal edge. gateway-svc's OWN edge is excluded from the served union on
    // purpose: it serves one face there (`push.deliver`, the push backplane's inbound
    // half) and no `#[http]` op, and the front door must never satisfy this invariant by
    // dispatching a fronted route to itself.
    let served: BTreeSet<&String> = split
        .iter()
        .filter(|p| p.process != "gateway-svc")
        .flat_map(|p| p.edge_methods.iter())
        .collect();
    // Under D2 the gateway fronts the describe-union (`split_front_methods`), not
    // `gateway.op_methods` (empty). Iterate the real fronted set so this stays a live guard:
    // a described-but-unserved method (an svc whose `__describe` lists a method its edge does
    // not register) is caught here as well as by DESCRIBE-COMPLETE below.
    for m in &split_front_methods {
        if !served.contains(m) {
            findings.push(format!(
                "[{label}] SERVE-PARITY: the split front door fronts {m:?} (via __describe) \
                 but NO domain svc registers it on its internal edge — the route would \
                 404/503 in the split"
            ));
        }
    }

    // 4. OVERLAP — the two real front doors (monolith "server", split "gateway-svc")
    // must never front two same-verb, overlapping-path operations; see the module doc's
    // invariant 4. The monolith checks its local `server.ops`; the split checks its
    // DESCRIBE-UNION (`split_front_ops`) — exactly the set the D2 gateway feeds to
    // `RouteTable::build_from_parts`, which `bail!`s on overlap at start. So a describe-union
    // overlap (two domain svcs describing colliding routes) is caught statically here, not
    // only at boot. Each front is checked independently so a FRONT-PARITY bypass can't hide it.
    findings.extend(overlap_findings(label, server.process, &server.ops));
    findings.extend(overlap_findings(label, gateway.process, &split_front_ops));

    // 5. DESCRIBE-COMPLETE — every edge-serving process serves its whole `#[http]`
    // surface under the ONE reserved `__describe` op (routing-as-data SERVE side, the
    // forget-guard). Checked ONLY on the SPLIT processes: the monolith serves no
    // internal edge, so its DESCRIBE_SLOT contributions are never registered
    // (`app::run` gates on the edge, exactly as `edge_methods` is empty for it here) —
    // asserting on it would flag inert data. gateway-svc IS checked and passes
    // vacuously: its one edge face is not an `#[http]` op, so `served_http` is empty and
    // it describes nothing. The monolith's aggregation is proven by
    // app's unit tests; here every real edge-serving svc must describe exactly what it
    // serves. `global_http` (the set of #[http] methods) is the monolith front door's op
    // set — the monolith fronts every #[http] op locally — used to filter each svc's
    // served edge methods down to its HTTP subset (an svc's edge also carries wire-only
    // and admin-fan-out methods, which `__describe` deliberately excludes).
    let global_http = &server.op_methods;
    for p in split.iter() {
        let served_http: BTreeSet<String> =
            p.edge_methods.intersection(global_http).cloned().collect();
        findings.extend(describe_completeness_findings(
            label,
            p.process,
            &served_http,
            &p.describe_methods,
        ));
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
            // The SPLIT front count under D2 is the describe-union size (what gateway-svc
            // fronts at runtime), NOT gateway-svc's own `opsapi::SLOT` (empty at register/init).
            let gw_n = split
                .iter()
                .filter(|p| p.process != "gateway-svc")
                .flat_map(|p| p.describe_ops.iter())
                .map(op_key)
                .collect::<BTreeSet<_>>()
                .len();
            println!(
                "routecheck [{}]: monolith fronts {mono_n} ops, split describe-union fronts \
                 {gw_n} — {} finding(s)",
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
