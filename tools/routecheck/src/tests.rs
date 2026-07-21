use super::*;
use opsapi::{AuthReq, RetryMode};

/// Builds a minimal fixture `Operation` — only the fields `overlap_findings` reads
/// (`method`/`verb`/`path`) matter; the rest are filler.
fn fixture_op(method: &str, verb: &str, path: &str) -> Operation {
    Operation {
        method: method.into(),
        verb: verb.into(),
        path: path.into(),
        auth: AuthReq::None,
        success: 200,
        retry_mode: RetryMode::Never,
    }
}

/// Invariant 4 (OVERLAP), the C17 fixture: `GET /x/{id}` and `GET /x/me` are
/// SHAPE-different (a wildcard vs a literal at the same position) but
/// REQUEST-SET-overlapping — a request to `/x/me` matches both. This is the exact
/// case the narrower "identical shape" predicate `RouteTable::build` used to use
/// missed; `overlap_findings` (the same `opsapi::pattern_overlaps` authority the
/// real gateway now uses) must catch it statically, without booting anything.
#[test]
fn overlap_findings_catches_literal_vs_wildcard_collision() {
    let ops = vec![fixture_op("x.byId", "GET", "/x/{id}"), fixture_op("x.me", "GET", "/x/me")];
    let findings = overlap_findings("fixture", "test-process", &ops);
    assert_eq!(findings.len(), 1, "exactly one overlapping pair: {findings:?}");
    assert!(findings[0].contains("x.byId") && findings[0].contains("x.me"), "{}", findings[0]);
}

/// A disjoint-verb pair and a disjoint-literal-prefix pair are both legitimate,
/// non-overlapping routes — no finding.
#[test]
fn overlap_findings_clean_on_disjoint_routes() {
    let ops = vec![
        fixture_op("x.get", "GET", "/x/{id}"),
        fixture_op("x.post", "POST", "/x/{id}"), // same path, different verb: fine
        fixture_op("char.byId", "GET", "/char/{id}"), // different literal prefix: fine
    ];
    assert!(overlap_findings("fixture", "test-process", &ops).is_empty());
}

/// Builds a method set from string literals — the unit both sides of the forget-guard
/// diff over.
fn methods(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|s| (*s).to_string()).collect()
}

/// Invariant 5 (DESCRIBE-COMPLETE), the forget-guard: a `#[http]` module that fronts an
/// op (it is in `op_methods`) but forgot `ctx.contribute(opsapi::DESCRIBE_SLOT, …)` (so
/// the op is absent from `describe_methods`) fails with a per-method FORGOT finding — the
/// exact D2-dead-route class this guard exists to catch BEFORE it ships.
#[test]
fn describe_guard_fails_a_http_module_that_forgot_to_contribute() {
    let op = methods(&["characters.create", "characters.delete"]);
    let described = methods(&["characters.create"]); // `delete` was NOT contributed
    let findings =
        describe_completeness_findings("fixture", "characters-svc", &op, &described);
    assert_eq!(findings.len(), 1, "exactly one forgotten op: {findings:?}");
    assert!(
        findings[0].contains("DESCRIBE-FORGOT")
            && findings[0].contains("characters.delete")
            && findings[0].contains("characters-svc"),
        "{}",
        findings[0]
    );
}

/// Builds a `ProcessRoutes` fixture. `op_methods` doubles as `bind_methods`/
/// `local_methods` so the integrity invariants stay quiet; the DESCRIBE-COMPLETE
/// invariant reads `edge_methods` and `describe_methods` independently.
fn proc_routes(
    process: &'static str,
    ops: Vec<Operation>,
    op_methods: &[&str],
    edge_methods: &[&str],
    describe_methods: &[&str],
) -> ProcessRoutes {
    ProcessRoutes {
        process,
        ops,
        op_methods: methods(op_methods),
        bind_methods: methods(op_methods),
        local_methods: methods(op_methods),
        edge_methods: methods(edge_methods),
        describe_methods: methods(describe_methods),
    }
}

/// Drives the REAL `check()` — and thus the production source-of-truth line
/// `served_http = p.edge_methods.intersection(global_http)` (`main.rs`) — to a
/// DESCRIBE-FORGOT, rather than hand-feeding sets to `describe_completeness_findings`
/// like the leaf tests. A domain svc SERVES `x.foo` on its internal edge (it is in
/// `edge_methods`) and the monolith front fronts it locally (so it is in `global_http`),
/// but the svc's `describe_methods` is missing it — the missed-forget the guard exists
/// to catch. If a mutation WEAKENS `served_http` (e.g. intersecting with
/// `describe_methods`, or an accidental empty set), `served_http ⊆ describe_methods`
/// holds vacuously, ZERO findings fire, and this assertion turns RED — so the guard can
/// never silently become decorative.
#[test]
fn describe_guard_fires_through_the_real_served_http_computation() {
    // Monolith front door: fronts `x.foo` locally — this op set IS `global_http`.
    let server = proc_routes(
        "server",
        vec![fixture_op("x.foo", "GET", "/x/foo")],
        &["x.foo"],
        &[],
        &["x.foo"],
    );
    // Split: gateway-svc (required present by `check`) + a domain svc that SERVES `x.foo`
    // on its edge but FORGOT to contribute it to DESCRIBE_SLOT.
    let gateway = proc_routes("gateway-svc", vec![], &[], &[], &[]);
    let forgetful = proc_routes("x-svc", vec![], &[], &["x.foo"], &[]);

    let findings = check("fixture", &[server], &[gateway, forgetful]);
    let forgot: Vec<&String> =
        findings.iter().filter(|f| f.contains("DESCRIBE-FORGOT")).collect();
    assert_eq!(
        forgot.len(),
        1,
        "exactly one DESCRIBE-FORGOT via the real served_http path: {findings:?}"
    );
    assert!(
        forgot[0].contains("x.foo") && forgot[0].contains("x-svc"),
        "{}",
        forgot[0]
    );
}

/// A process whose describe manifest EXACTLY covers its `#[http]` ops passes clean — the
/// property every real edge-serving process must hold on a clean tree.
#[test]
fn describe_guard_clean_when_every_http_op_is_described() {
    let op = methods(&["characters.create", "characters.delete", "characters.list"]);
    assert!(describe_completeness_findings("fixture", "characters-svc", &op, &op).is_empty());
}

/// The reverse diff: a describe entry with no matching `#[http]` Operation (a stale or
/// foreign manifest) fails with a DESCRIBE-EXTRA finding.
#[test]
fn describe_guard_fails_a_stale_describe_entry() {
    let op = methods(&["characters.create"]);
    let described = methods(&["characters.create", "characters.ghost"]);
    let findings =
        describe_completeness_findings("fixture", "characters-svc", &op, &described);
    assert_eq!(findings.len(), 1, "exactly one stale entry: {findings:?}");
    assert!(
        findings[0].contains("DESCRIBE-EXTRA") && findings[0].contains("characters.ghost"),
        "{}",
        findings[0]
    );
}

/// The check IS the test (archcheck/checkmodules self-test pattern): run the full
/// two-config route-parity check over the real workspace module lists and assert a
/// clean tree. This is what makes the invariant enforceable from `cargo test
/// --workspace`, not only from the dedicated verify stage.
///
/// Real-tree OVERLAP coverage: `check()` (called via `run_all`) now runs
/// `overlap_findings` over both front doors' real contributed ops as part of this
/// same assertion — a clean tree here also proves invariant 4 holds for real,
/// today's 12 `#[http]` ops, not just the synthetic fixture above.
///
/// Env note: `run_all` flips the gate env vars before creating each config's
/// runtime. The two fixture tests above are pure functions over synthetic
/// `Operation` vecs — no env read or write — so they cannot race this test's env
/// mutation regardless of test-thread interleaving; this remains the only test in
/// this binary that touches env.
#[test]
fn workspace_route_sets_are_structurally_equal() {
    let findings = run_all().expect("routecheck harness must build every process");
    assert!(
        findings.is_empty(),
        "routecheck found {} route-parity violation(s):\n  - {}",
        findings.len(),
        findings.join("\n  - ")
    );
}
