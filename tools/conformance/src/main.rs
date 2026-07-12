//! `conformancecheck` — the convention-conformance harness (`tools/conformance`,
//! plan `docs/plans/2026-07-12-0952-convention-conformance-harness-plan.md`).
//!
//! Per-module tests cannot catch a twin module that was skipped when a
//! convention was hardened: a missing test in module Y looks identical to "the
//! convention doesn't apply to Y". This harness targets the CONVENTION, not the
//! module, in four phases:
//!
//! 1. **Drift preflight (didn't-forget self-check).** The hand list in
//!    [`entries`] is three-way diffed against the `modules/*` directories on
//!    disk and the monolith module set from `checkmodules` (minus the sanctioned
//!    [`checks::CORE_INFRA_MODULES`]). Every mismatch is its own line; any
//!    finding fails the run before an assertion executes.
//! 2. **Completeness matrix.** Every module must declare an explicit stance for
//!    every [`Convention::ALL`]; `NotApplicable` needs a non-empty `why`.
//! 3. **Executors.** T6 env-validation (a bad env value must fail a fresh full
//!    `App::build()` with an error chain naming the var), T8 input byte caps
//!    (`!probe(cap) && probe(cap + 1)`), T7 infra-outage classification
//!    (`Unavailable`, never `Rejected`), T2 argon2 parameter parity (pairwise).
//! 4. **Report.** A module × convention table, per-fail detail lines, non-zero
//!    exit on any fail.
//!
//! ## Env discipline (the routecheck pattern, `tools/routecheck/src/main.rs:48-51`)
//! `std::env::set_var`/`remove_var` are unsound once other threads exist, and
//! `register`/`init` read env — so `main` is deliberately NOT `#[tokio::main]`:
//! each T6 case sets its var while this is the sole thread, builds a fresh
//! single-thread runtime, boots a fresh full module set inside it, drops the
//! runtime, then removes the var. This also concentrates the future
//! edition-2024 `unsafe set_var` cost in one place.
//!
//! ## No live Postgres needed
//! Every pool is `connect_lazy` and never connects: `register`/`init` do no I/O
//! (constraint 8) — audit's env validation errors at `init` before any I/O.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use bus::{
    AnyTx, Error as BusError, EventContract, HistoryPolicy, SubscriptionSpec, Transport, TxHandler,
};
use lifecycle::{App, Context};

mod checks;
mod input_inventory;
mod model;
mod policy;
#[cfg(test)]
mod tests;

use checks::{
    argon_parity_findings, completeness_findings, conv_label, drift_findings, eval_cap_probe,
};
use model::{ArgonParams, Convention, EnvCase, Fixture, InputPolicy, OutageClass, Stance};

/// Dev-default DSN (mirrors CLAUDE.md). Only ever used to build a LAZY pool that
/// never connects — `register`/`init` do no I/O.
const DEFAULT_DSN: &str =
    "postgres://gamebackend:gamebackend@localhost:5432/gamebackend?sslmode=disable";

/// A `bus::Transport` that ignores everything — nothing is emitted during
/// `register`/`init`, and this harness does not care about subscriptions (that
/// is topiccheck's job). Deliberate duplication of the private copy in
/// `tools/routecheck/src/main.rs:91-94`; hoisting it to a shared place is a
/// separate decision outside this rollout.
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

/// The names of every immediate subdirectory of `dir` holding a `Cargo.toml` —
/// the filesystem's own answer to "which crates live under modules/",
/// independent of workspace registration (the `archcheck::crate_dirs` pattern,
/// `tools/archcheck/src/main.rs:570-582` — deliberately the filesystem, not
/// cargo metadata, so an unregistered module still drifts loudly).
fn crate_dirs(dir: &Path) -> Vec<String> {
    let Ok(read) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    read.flatten()
        .filter(|e| e.path().is_dir() && e.path().join("Cargo.toml").is_file())
        .filter_map(|e| e.file_name().to_str().map(String::from))
        .collect()
}

/// `modules/` at the workspace root, located relative to this crate
/// (`tools/conformance`).
fn modules_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("modules")
}

/// `Module::name()` of every monolith module. Plain constructors, no lifecycle
/// phase runs — safe outside a runtime.
fn monolith_module_names() -> BTreeSet<String> {
    checkmodules::monolith_modules()
        .iter()
        .map(|m| m.name().to_string())
        .collect()
}

/// Boots a fresh FULL monolith module set through `register` → `init`
/// (`App::build`) with a lazy pool + no-op transport — the routecheck boot
/// shape, NOT a DB-less `Context::new()` (DB-backed modules would fail
/// `register` before env validation is ever reached). Must run inside a tokio
/// runtime (an in-process `Bus::on` during `init` spawns a task).
fn build_full_monolith() -> anyhow::Result<()> {
    let dsn = std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DSN.to_string());
    let pool = sqlx::postgres::PgPool::connect_lazy(&dsn)
        .map_err(|e| anyhow::anyhow!("conformancecheck: build lazy pool: {e}"))?;
    let ctx = Arc::new(Context::with_db_and_transport(
        pool,
        Arc::new(NoopTransport),
    ));
    let mut app = App::new(ctx);
    for m in checkmodules::monolith_modules() {
        app.add(m);
    }
    app.build()
}

/// T6, one case: set `var=bad_value` while this thread is alone, boot a fresh
/// full monolith in a fresh single-thread runtime, expect an `Err` whose anyhow
/// chain names the var, then restore env. `None` = pass.
fn run_env_case(case: &EnvCase) -> Option<String> {
    // Env first, runtime second — set_var is only sound while no runtime
    // (worker or blocked task) thread exists. The runtime is fully dropped
    // before the var is removed.
    std::env::set_var(case.var, case.bad_value);
    let result = (|| {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| anyhow::anyhow!("build runtime: {e}"))?;
        let out = rt.block_on(async { build_full_monolith() });
        drop(rt);
        out
    })();
    std::env::remove_var(case.var);

    match result {
        Ok(()) => Some(format!(
            "{}={:?}: App::build SUCCEEDED — the bad value was silently accepted or \
             defaulted instead of failing startup",
            case.var, case.bad_value
        )),
        Err(e) => {
            if e.chain().any(|cause| cause.to_string().contains(case.var)) {
                None
            } else {
                Some(format!(
                    "{}={:?}: App::build failed, but no error in the chain names the \
                     variable — the operator can't tell what to fix (chain: {e:#})",
                    case.var, case.bad_value
                ))
            }
        }
    }
}

/// One report cell.
#[derive(Clone, Copy, PartialEq)]
enum Cell {
    Pass,
    Fail,
    Gap,
    NotApplicable,
}

impl Cell {
    fn label(self) -> &'static str {
        match self {
            Cell::Pass => "pass",
            Cell::Fail => "FAIL",
            Cell::Gap => "GAP",
            Cell::NotApplicable => "n/a",
        }
    }
}

fn fail_phase(phase: &str, findings: &[String]) -> ! {
    eprintln!(
        "conformancecheck: {phase} FAILED — {} finding(s):",
        findings.len()
    );
    for f in findings {
        eprintln!("  - {f}");
    }
    std::process::exit(1);
}

fn main() {
    println!("conformancecheck: convention-conformance matrix (module × convention)\n");

    // ---- Phase 1: drift preflight (fail before anything else) ---------------
    let entries = policy::entries();

    let discovered_inputs = input_inventory::discover(&input_inventory::api_root())
        .unwrap_or_else(|error| fail_phase("input discovery", &[format!("{error:#}")]));
    let input_policies = policy::input_policies();
    let policy_keys = input_policies
        .iter()
        .map(|(key, _)| key.clone())
        .collect::<Vec<_>>();
    let input_policy_drift = input_inventory::policy_key_findings(&discovered_inputs, &policy_keys);
    if !input_policy_drift.is_empty() {
        fail_phase("input policy drift", &input_policy_drift);
    }
    let actual_golden = input_inventory::render_golden(&discovered_inputs);
    let committed_golden =
        std::fs::read_to_string(input_inventory::golden_path()).unwrap_or_else(|error| {
            fail_phase("input golden", &[format!("read committed golden: {error}")])
        });
    let golden = input_inventory::golden_findings(&actual_golden, &committed_golden);
    if !golden.is_empty() {
        fail_phase("input golden", &golden);
    }

    println!(
        "conformancecheck: RPC request string inventory ({} fields)",
        discovered_inputs.len()
    );
    for (key, stance) in &input_policies {
        let policy = match stance {
            InputPolicy::Validated { cap, basis } => format!("validated({cap}): {basis}"),
            InputPolicy::KnownGap {
                planned_cap,
                remediation,
            } => format!("GAP(planned {planned_cap}): {remediation}"),
            InputPolicy::Opaque { rationale } => format!("opaque: {rationale}"),
            InputPolicy::Unrestricted { rationale } => format!("unrestricted: {rationale}"),
        };
        println!("  {}\t{policy}", input_inventory::render_key(key));
    }
    println!();
    let disk: BTreeSet<String> = crate_dirs(&modules_dir()).into_iter().collect();
    let entry_names: BTreeSet<String> = entries.iter().map(|e| e.module.to_string()).collect();
    let monolith = monolith_module_names();
    let drift = drift_findings(&disk, &entry_names, &monolith);
    if !drift.is_empty() {
        fail_phase("drift preflight", &drift);
    }

    // ---- Phase 2: completeness matrix ---------------------------------------
    let completeness = completeness_findings(&entries);
    if !completeness.is_empty() {
        fail_phase("completeness matrix", &completeness);
    }

    // ---- Phase 3: executors --------------------------------------------------
    let n_conv = Convention::ALL.len();
    let mut cells: Vec<Vec<Cell>> = vec![vec![Cell::NotApplicable; n_conv]; entries.len()];
    let mut details: Vec<String> = Vec::new();
    let mut gaps: Vec<String> = Vec::new();
    // (module, params, entry index, convention index) — parity is judged across
    // modules after the walk, so the cells are patched afterwards.
    let mut argon: Vec<(&str, ArgonParams, usize, usize)> = Vec::new();

    for (i, entry) in entries.iter().enumerate() {
        for (j, &conv) in Convention::ALL.iter().enumerate() {
            let stance = entry
                .stance(conv)
                .expect("completeness matrix guarantees a stance per convention");
            let fixture = match stance {
                Stance::NotApplicable { .. } => {
                    cells[i][j] = Cell::NotApplicable;
                    continue;
                }
                Stance::KnownGap { why, remediation } => {
                    cells[i][j] = Cell::Gap;
                    gaps.push(format!(
                        "[{} × {}] known gap: {why}; remediation: {remediation}",
                        entry.module,
                        conv_label(conv)
                    ));
                    continue;
                }
                Stance::Applies(f) => f,
            };
            let mut case_failures: Vec<String> = Vec::new();
            match fixture {
                Fixture::EnvValidation(cases) => {
                    // Each case flips env with NO runtime alive (the transient
                    // runtime inside run_env_case is dropped before restore).
                    for case in cases {
                        case_failures.extend(run_env_case(case));
                    }
                }
                Fixture::InputByteCaps(cases) => {
                    for case in cases {
                        let rejected_at_cap = (case.probe)(case.cap);
                        let rejected_over_cap = (case.probe)(case.cap + 1);
                        case_failures.extend(eval_cap_probe(
                            case.name,
                            case.cap,
                            rejected_at_cap,
                            rejected_over_cap,
                        ));
                    }
                }
                Fixture::InfraOutage503(cases) => {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .expect("build tokio runtime for outage probes");
                    for case in cases {
                        match rt.block_on((case.probe)()) {
                            OutageClass::Unavailable => {}
                            OutageClass::Rejected => case_failures.push(format!(
                                "{}: infra outage classified as REJECTED (401-class) — \
                                 must surface as Unavailable (503-class)",
                                case.name
                            )),
                            OutageClass::Other(desc) => case_failures.push(format!(
                                "{}: infra outage classified as neither Unavailable nor \
                                 Rejected: {desc}",
                                case.name
                            )),
                        }
                    }
                }
                Fixture::ArgonParity(params) => {
                    argon.push((entry.module, *params, i, j));
                    cells[i][j] = Cell::Pass; // provisional; patched below on mismatch
                    continue;
                }
            }
            if case_failures.is_empty() {
                cells[i][j] = Cell::Pass;
            } else {
                cells[i][j] = Cell::Fail;
                for f in case_failures {
                    details.push(format!("[{} × {}] {f}", entry.module, conv_label(conv)));
                }
            }
        }
    }

    // T2 is cross-module: any pairwise mismatch fails EVERY participant's cell
    // (parity is a property of the set, not of one module).
    let argon_params: Vec<(&str, ArgonParams)> =
        argon.iter().map(|(m, p, _, _)| (*m, *p)).collect();
    let parity = argon_parity_findings(&argon_params);
    if !parity.is_empty() {
        for (_, _, i, j) in &argon {
            cells[*i][*j] = Cell::Fail;
        }
        for f in parity {
            details.push(format!("[argon-parity] {f}"));
        }
    } else if argon.len() == 1 {
        println!(
            "note: argon-parity has a single participant ({}) — parity holds vacuously\n",
            argon[0].0
        );
    }

    // ---- Phase 4: report ------------------------------------------------------
    let module_w = entries
        .iter()
        .map(|e| e.module.len())
        .max()
        .unwrap_or(6)
        .max("module".len());
    let col_ws: Vec<usize> = Convention::ALL
        .iter()
        .map(|&c| conv_label(c).len().max(4))
        .collect();

    let mut header = format!("{:module_w$}", "module");
    for (j, &c) in Convention::ALL.iter().enumerate() {
        header.push_str(&format!("  {:w$}", conv_label(c), w = col_ws[j]));
    }
    println!("{header}");
    for (i, entry) in entries.iter().enumerate() {
        let mut row = format!("{:module_w$}", entry.module);
        for (j, cell) in cells[i].iter().enumerate() {
            row.push_str(&format!("  {:w$}", cell.label(), w = col_ws[j]));
        }
        println!("{row}");
    }

    let any_fail = cells.iter().flatten().any(|c| *c == Cell::Fail);
    if any_fail {
        eprintln!(
            "\nconformancecheck: FAIL — {} detail line(s):",
            details.len()
        );
        for d in &details {
            eprintln!("  - {d}");
        }
        std::process::exit(1);
    }
    if gaps.is_empty() {
        println!(
            "\n[conformancecheck] OK: {} modules × {} conventions",
            entries.len(),
            n_conv
        );
    } else {
        println!("\n[conformancecheck] GAP: {} known gap(s)", gaps.len());
        for gap in gaps {
            println!("  - {gap}");
        }
    }
}
