//! `requirecheck` — the registry require-vs-declare drift check, the registry-seam
//! analogue of `topiccheck` (which does the same for the event bus).
//!
//! ## What it flags
//! A **mandatory** `ctx.registry().require::<T>(key)` call whose provider module is
//! NOT declared in the calling module's `requires()`. `requires()` is a MANIFEST that
//! `app::validate_requires` checks name-vs-present-provider (under-provisioning); it
//! never observes an actual `require()` call, so the OPPOSITE drift — a real
//! `require("characters.ownership")` without a `requires("characters")` — compiles,
//! passes archcheck (the crate dep exists), passes `validate_requires` (nothing
//! declared to check), and only **panics at `init`** in a split process where the
//! provider isn't co-hosted and no stub is wired. This tool moves that class to static
//! time by observing the real `require` call vs the declared manifest.
//!
//! Optional `try_require` calls are recorded but NOT enforced (an optional dependency
//! is deliberately allowed to be undeclared — e.g. `gateway`'s `accounts.sessions`).
//!
//! ## How
//! It installs the opt-in `core/registry` require-observer, builds the MONOLITH module
//! set (the superset of every process — a `require` in ANY deployment counts), and runs
//! the two lifecycle wiring phases (`register` → `init`). Unlike `topiccheck`, it does
//! NOT call `App::build`: that runs every `init` in one loop, so the observer (which
//! sees only `RequireKind` + the `&str` key) could not tell WHICH module called
//! `require`. Instead it replicates the two phases itself and sets a "current module"
//! marker on the recorder immediately before each `m.init(&ctx)`, attributing every
//! observed require to its caller.
//!
//! ## Structural limitation (must be understood before trusting this tool)
//! The harness runs ONLY `register` + `init` (the two no-I/O phases). A
//! `require`/`try_require` issued from `start` (the real-I/O phase, never run here) or
//! resolved lazily inside a provided service's request handler is **invisible** to this
//! tool. The enforced invariant is therefore: **requires must be resolved in `init` to
//! be checked.** None are resolved elsewhere today (all sites are in `init`), but a
//! future `start`-time require would silently escape this net.
//!
//! ## No live DB needed
//! `register`/`init` do NO I/O (lifecycle constraint 8 — only `migrate`/`start` touch
//! the DB, and this tool runs neither), so the shared pool is a `connect_lazy` handle
//! that never connects, with the local Postgres DSN as the harmless fallback.
//!
//! Advisory by default (prints the table, exits 0). With `--strict` it exits non-zero
//! ONLY on an under-declaration violation (never on the advisory over-declaration
//! report), for use as the verify gate.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use bus::{AnyTx, Error, Transport, TxHandler};
use checkmodules::DeploymentProfile;
use lifecycle::Context;
use registry::RequireKind;

/// One observed registry lookup: `(module, kind, key)`.
type Hit = (String, RequireKind, String);

/// The harness output: ordered module names, the recorded hits, and each module's
/// declared `requires()`.
type Collected = (Vec<String>, Vec<Hit>, BTreeMap<String, Vec<String>>);

/// Dev-default DSN (mirrors CLAUDE.md). Only ever used to build a LAZY pool that
/// never connects — `register`/`init` do no I/O.
const DEFAULT_DSN: &str =
    "postgres://gamebackend:gamebackend@localhost:5432/gamebackend?sslmode=disable";

/// A `bus::Transport` that does nothing. `inventory::init` and `match::init` subscribe
/// via `ctx.bus().on_tx(...)`, and `Bus::on_tx_raw` PANICS if no transport is installed
/// — so a no-op transport MUST be injected before the two-phase loop, else the harness
/// dies at the first `init` before observing a single `require`. It stands in for the
/// app-owned durable-events plane and records nothing; this tool only cares about the
/// registry seam, not the bus.
struct NoopTransport;

#[async_trait::async_trait]
impl Transport for NoopTransport {
    async fn enqueue_tx(
        &self,
        _tx: AnyTx<'_>,
        _contract: &bus::EventContract,
        _payload: &[u8],
    ) -> Result<(), Error> {
        Ok(())
    }

    fn subscribe_tx(
        &self,
        _spec: bus::SubscriptionSpec,
        _topic: &str,
        _version: u32,
        _history: Option<bus::HistoryPolicy>,
        _handler: Arc<dyn TxHandler>,
    ) {
    }
}

/// Accumulates observed require calls, attributed to the module whose `init` is running.
/// `current_module` is the marker set immediately before each `m.init(&ctx)`; a require
/// fired during phase-1 `register` (before any marker is set — none exist today) is
/// bucketed under `"(register-phase)"` rather than panicking.
#[derive(Default)]
struct Recorder {
    current_module: Option<String>,
    /// `(module, kind, key)` — one per observed `require`/`try_require`.
    hits: Vec<Hit>,
}

/// The provider module of a capability key — the prefix before the first `.`, exactly
/// how `registry::key(module, cap)` composes `"<module>.<cap>"`. A key with no `.`
/// (none today) maps to the whole key.
fn provider_of(key: &str) -> &str {
    key.split('.').next().unwrap_or(key)
}

/// The mandatory require providers a module actually called (deduped), for the table.
fn observed_mandatory(hits: &[Hit], module: &str) -> BTreeSet<String> {
    hits.iter()
        .filter(|(m, kind, _)| m == module && *kind == RequireKind::Mandatory)
        .map(|(_, _, key)| provider_of(key).to_string())
        .collect()
}

/// The pure diff: every `(module, provider)` where the module issued a MANDATORY
/// `require` for `provider` but does NOT declare `provider` in its `requires()` and
/// `provider` is not allowlisted. Factored out so it is unit-testable without the
/// lifecycle harness (mirrors how `topiccheck` factors `unsubscribed`).
///
/// Optional (`try_require`) hits are ignored — an optional dependency is deliberately
/// allowed to be undeclared.
fn undeclared(
    hits: &[Hit],
    declared: &BTreeMap<String, Vec<String>>,
    allow: &[&str],
) -> Vec<(String, String)> {
    let mut out = BTreeSet::new();
    for (module, kind, key) in hits {
        if *kind != RequireKind::Mandatory {
            continue;
        }
        let provider = provider_of(key).to_string();
        if allow.contains(&provider.as_str()) {
            continue;
        }
        let declared_here = declared.get(module).map(Vec::as_slice).unwrap_or(&[]);
        if !declared_here.iter().any(|d| d == &provider) {
            out.insert((module.clone(), provider));
        }
    }
    out.into_iter().collect()
}

/// Runs the harness: builds the monolith set with a no-op transport + the require
/// observer installed, drives the manual two-phase loop, and returns the ordered module
/// names, the recorded hits, and each module's declared `requires()`.
fn collect_requires() -> anyhow::Result<Collected> {
    // A LAZY pool: never connects, since register/init do no I/O (constraint 8).
    let dsn = std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DSN.to_string());
    let pool = sqlx::postgres::PgPool::connect_lazy(&dsn)
        .map_err(|e| anyhow::anyhow!("requirecheck: build lazy pool: {e}"))?;
    // A transport MUST exist before init: inventory/match subscribe via on_tx, and
    // Bus::on_tx_raw panics without one. This no-op stands in for the app-owned plane.
    let ctx = Arc::new(Context::with_db_and_transport(pool, Arc::new(NoopTransport)));

    // Install the require-observer; it attributes each require to the module whose init
    // is currently running (the marker set in phase 2 below). The closure touches only
    // its own Mutex<Recorder> — never the registry — per the setter's non-reentrancy
    // invariant.
    let recorder = Arc::new(Mutex::new(Recorder::default()));
    let rec = recorder.clone();
    ctx.registry()
        .set_require_observer(Arc::new(move |kind, key| {
            let mut r = rec.lock().unwrap();
            let module = r
                .current_module
                .clone()
                .unwrap_or_else(|| "(register-phase)".to_string());
            r.hits.push((module, kind, key.to_string()));
        }));

    let modules: Vec<Box<dyn lifecycle::Module>> = DeploymentProfile::Monolith
        .processes()
        .into_iter()
        .flat_map(|(_process_id, mods)| mods)
        .collect();

    // Manual two-phase loop (NOT App::build) — App::build runs every init in one loop,
    // so the observer could not attribute a require to a module. Phase 1: register.
    for m in &modules {
        m.register(&ctx)
            .map_err(|e| anyhow::anyhow!("requirecheck: register {:?}: {e:#}", m.name()))?;
    }
    // Phase 2: mark the current module, then init — so every require lands attributed.
    for m in &modules {
        recorder.lock().unwrap().current_module = Some(m.name().to_string());
        m.init(&ctx)
            .map_err(|e| anyhow::anyhow!("requirecheck: init {:?}: {e:#}", m.name()))?;
    }

    let order: Vec<String> = modules.iter().map(|m| m.name().to_string()).collect();
    let declared: BTreeMap<String, Vec<String>> = modules
        .iter()
        .map(|m| (m.name().to_string(), m.requires()))
        .collect();
    let hits = std::mem::take(&mut recorder.lock().unwrap().hits);
    Ok((order, hits, declared))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // No auth env needed: Admin::init no longer reads ADMIN_USER/ADMIN_PASS — session
    // auth is DB-backed (a zero-user boot merely warns), so the harness builds the
    // module graph with a bare environment.

    let strict = std::env::args().any(|a| a == "--strict");

    let (order, hits, declared) = collect_requires()?;
    let violations = undeclared(&hits, &declared, &[]);

    // The report table: every module, its declared requires(), the mandatory require
    // providers it actually called, and a verdict.
    println!("requirecheck: registry require() vs declared requires()\n");
    let header = format!(
        "{:<14} | {:<28} | {:<24} | VERDICT",
        "MODULE", "DECLARED requires()", "OBSERVED require providers"
    );
    println!("{header}");
    println!("{}", "-".repeat(90));

    // Advisory over-declaration: a declared provider (minus allowlist) never keyed-
    // required. Informational only — bus-only deps legitimately land here.
    let mut over_declared: Vec<(String, String)> = Vec::new();

    for module in &order {
        let decl = declared.get(module).cloned().unwrap_or_default();
        let observed = observed_mandatory(&hits, module);
        let decl_str = if decl.is_empty() {
            "-".to_string()
        } else {
            decl.join(", ")
        };
        let obs_str = if observed.is_empty() {
            "-".to_string()
        } else {
            observed.iter().cloned().collect::<Vec<_>>().join(", ")
        };
        let module_violations: Vec<&str> = violations
            .iter()
            .filter(|(m, _)| m == module)
            .map(|(_, p)| p.as_str())
            .collect();
        let verdict = if module_violations.is_empty() {
            "OK".to_string()
        } else {
            format!("UNDER-DECLARED: {}", module_violations.join(", "))
        };
        println!("{module:<14} | {decl_str:<28} | {obs_str:<24} | {verdict}");

        for d in &decl {
            if !observed.contains(d) {
                over_declared.push((module.clone(), d.clone()));
            }
        }
    }
    println!();

    // Advisory over-declaration report (never fails --strict).
    if !over_declared.is_empty() {
        println!("requirecheck: advisory — declared but never keyed-required (bus-only deps land here legitimately):");
        for (m, p) in &over_declared {
            println!("  - {m} declares {p:?} with no mandatory require()");
        }
        println!();
    }

    if violations.is_empty() {
        println!(
            "requirecheck: OK — every mandatory require() provider is declared in its caller's requires()"
        );
        return Ok(());
    }

    eprintln!(
        "requirecheck: FAIL — {} undeclared mandatory require provider(s):",
        violations.len()
    );
    for (m, p) in &violations {
        eprintln!("  - {m} requires provider {p:?} (mandatory require()) but does not declare it in requires()");
    }
    if strict {
        std::process::exit(1);
    }
    Ok(())
}

#[cfg(test)]
mod tests;
