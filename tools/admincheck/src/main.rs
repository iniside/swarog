//! `admincheck` — the admin extension-point contract validator, shaped exactly like
//! `topiccheck` (a runtime harness over the REAL wiring, not a hand-written
//! annotation scheme). For each deployment profile (Monolith AND Split, from
//! `checkmodules`) it builds every process's module set with a lazy DB pool + no-op
//! bus transport, runs the two no-I/O lifecycle phases (`register` → `init`), reads
//! `adminapi::SLOT` contributions, and validates every contributed
//! [`adminapi::ExtensionEntry`] against the extension POINTS the page owners declare.
//!
//! ## Why a runtime harness (not a `linkme`/annotation scheme)
//! A contributor ships extension entries as PURE DATA on its `Item` (LOCAL, via
//! `Item::with_extensions`) — the same data rides `ItemData::extensions` REMOTE.
//! `register`/`init` do no I/O (constraint 8), so this drives the actual wiring and
//! reads the entries that ACTUALLY got contributed, per profile. The DECLARED POINTS
//! are enumerated explicitly below (the one conscious edit point), referencing each
//! owner const directly so a renamed/removed/added point breaks THIS tool at compile
//! time — the same coupling `topiccheck`'s `defined_topics()` has to `bus::define`.
//!
//! ## Validations, applied PER deployment profile (PURE DATA ONLY)
//! Renders do DB I/O, so `render()`/`admin_data()` are NEVER invoked — admincheck
//! reads only the declarative `Item::extensions` / point consts. It checks:
//! 1. **declared target** — every `ExtensionEntry.point` names a DECLARED point id (a
//!    typo'd/renamed target is drift).
//! 2. **present ↔ kind** — a `ModalActions` point accepts ONLY `Present::Modal`
//!    entries (a `Navigate` in a modal footer is a mis-binding); an `EntityMenu`
//!    accepts both `Navigate` and `Modal`.
//! 3. **interpolation keys** — every `{key}` placeholder in an entry's `link` is a
//!    member of the target point's `context_keys` (an unfillable `{key}` renders the
//!    entry silently skipped at request time).
//! 4. **label collisions** — no `(point, label)` pair is contributed by more than one
//!    distinct contributor (two modules adding an identically-labelled entry to one
//!    point is ambiguous chrome).
//!
//! ## KNOWN, ACCEPTED GAP (recorded deliberately)
//! A `Table::menu_point` / `Content::modal_point` BINDING value lives inside a page
//! owner's render OUTPUT, which admincheck cannot see without DB I/O, and the
//! domain-blind portal cannot validate it either (it never imports the owner consts).
//! A typo'd binding means the point's extensions silently don't appear — NOT caught
//! here. Coverage for the baseline points comes from split-proof's `[ADX1/ADX3]`
//! assertions (the entries ARE rendered end-to-end); any future point earns its own
//! split-proof assertion per the standing "extend split-proof when you add a flow"
//! rule. This tool validates the CONTRIBUTION side (entries ⇄ declared points); the
//! RENDER-side binding is proven live.
//!
//! ## No live DB needed
//! `register`/`init` do NO I/O (constraint 8), so each process's shared pool is a
//! `connect_lazy` handle that never connects (the same trick `checkmodules` relies
//! on). Building all 13 processes (1 monolith + 12 svc) touches no database.
//!
//! ## Flags / exit
//! Advisory by default (prints the per-profile tables + findings, exits 0).
//! `--strict` (the verify-stage invocation) exits non-zero on ANY finding in ANY
//! profile — mirroring `topiccheck`'s advisory-default / strict-fails contract.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use adminapi::{ExtensionEntry, ExtensionKind, ExtensionPoint, Present};
use bus::{AnyTx, Error as BusError, EventContract, HistoryPolicy, SubscriptionSpec, Transport, TxHandler};
use checkmodules::DeploymentProfile;
use lifecycle::{App, Context};

/// Dev-default DSN (mirrors CLAUDE.md). Only ever used to build a LAZY pool that never
/// connects — `register`/`init` do no I/O.
const DEFAULT_DSN: &str =
    "postgres://gamebackend:gamebackend@localhost:5432/gamebackend?sslmode=disable";

/// A `bus::Transport` that ignores everything: nothing is emitted during
/// `register`/`init`, and admincheck does not care about subscriptions (that is
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

/// The DECLARED extension points: the canonical owner consts, referenced directly so
/// a renamed/removed/added point forces an edit HERE and breaks the build (the same
/// conscious edit point as `topiccheck`'s `defined_topics()`).
fn declared_points() -> Vec<ExtensionPoint> {
    vec![
        accountsapi::admin::PLAYERS_ROW_MENU,
        charactersapi::admin::CHARACTERS_CARD_MENU,
        charactersapi::admin::CHARACTER_MODAL_ACTIONS,
    ]
}

/// One contributed extension entry, tagged with the contributor (`Item::id`) and the
/// process that wired it — the report's `contributor @ process` cell.
struct Observed {
    entry: ExtensionEntry,
    contributor: String,
    process: &'static str,
}

/// Builds every process of `profile` with a no-op transport + lazy pool, runs the two
/// no-I/O lifecycle phases, and returns every contributed [`ExtensionEntry`] (tagged
/// with its contributing item + hosting process). Renders are NEVER invoked — only the
/// declarative `Item::extensions` field is read.
fn observe(profile: &DeploymentProfile) -> anyhow::Result<Vec<Observed>> {
    let dsn = std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DSN.to_string());
    let mut out = Vec::new();

    for (process_id, mods) in profile.processes() {
        // A LAZY pool per process: never connects, since register/init do no I/O.
        let pool = sqlx::postgres::PgPool::connect_lazy(&dsn)
            .map_err(|e| anyhow::anyhow!("admincheck: {process_id}: build lazy pool: {e}"))?;
        let ctx = Arc::new(Context::with_db_and_transport(pool, Arc::new(NoopTransport)));

        let mut app = App::new(ctx.clone());
        for m in mods {
            app.add(m);
        }
        app.build().map_err(|e| {
            anyhow::anyhow!("admincheck: {process_id}: lifecycle build failed: {e:#}")
        })?;

        for item in ctx.contributions(adminapi::SLOT) {
            for entry in &item.extensions {
                out.push(Observed {
                    entry: entry.clone(),
                    contributor: item.id.clone(),
                    process: process_id,
                });
            }
        }
    }
    Ok(out)
}

/// Extracts every `{key}` placeholder from a link template (e.g.
/// `"characters?owner={id}"` → `["id"]`). Unbalanced braces stop the scan.
fn placeholders(link: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = link;
    while let Some(open) = rest.find('{') {
        let after = &rest[open + 1..];
        match after.find('}') {
            Some(close) => {
                out.push(after[..close].to_string());
                rest = &after[close + 1..];
            }
            None => break,
        }
    }
    out
}

/// Runs every check for one profile, prints its report table + findings, and returns
/// the finding count folded into the exit decision.
fn run_profile(name: &str, profile: &DeploymentProfile, points: &[ExtensionPoint]) -> anyhow::Result<usize> {
    let observed = observe(profile)?;
    let by_id: BTreeMap<&str, &ExtensionPoint> = points.iter().map(|p| (p.id, p)).collect();

    let mut findings: Vec<String> = Vec::new();
    // Point ids that carry at least one finding, so the table's STATUS column reflects it.
    let mut flagged: BTreeSet<String> = BTreeSet::new();
    // (point, label) → distinct contributors, for the collision check.
    let mut label_owners: BTreeMap<(String, String), BTreeSet<String>> = BTreeMap::new();

    for o in &observed {
        let where_ = format!("{} @ {}", o.contributor, o.process);
        label_owners
            .entry((o.entry.point.clone(), o.entry.label.clone()))
            .or_default()
            .insert(o.contributor.clone());

        match by_id.get(o.entry.point.as_str()) {
            None => {
                findings.push(format!(
                    "entry {:?} ({where_}) targets UNDECLARED point {:?} — no owner const declares it",
                    o.entry.label, o.entry.point
                ));
            }
            Some(point) => {
                // Check 2 — present ↔ kind. ModalActions accepts only Modal.
                if point.kind == ExtensionKind::ModalActions && o.entry.present != Present::Modal {
                    flagged.insert(o.entry.point.clone());
                    findings.push(format!(
                        "entry {:?} ({where_}) on ModalActions point {:?} is {:?} — a modal-footer \
                         action must be Present::Modal",
                        o.entry.label, o.entry.point, o.entry.present
                    ));
                }
                // Check 3 — interpolation keys ⊆ context_keys.
                for key in placeholders(&o.entry.link) {
                    if !point.context_keys.contains(&key.as_str()) {
                        flagged.insert(o.entry.point.clone());
                        findings.push(format!(
                            "entry {:?} ({where_}) link {:?} interpolates {{{key}}} but point {:?} \
                             declares context_keys {:?}",
                            o.entry.label, o.entry.link, o.entry.point, point.context_keys
                        ));
                    }
                }
            }
        }
    }

    // Check 4 — (point, label) contributed by more than one distinct contributor.
    for ((point, label), owners) in &label_owners {
        if owners.len() > 1 {
            flagged.insert(point.clone());
            findings.push(format!(
                "(point {point:?}, label {label:?}) is contributed by {} distinct contributors ({}) \
                 — a colliding menu entry",
                owners.len(),
                owners.iter().cloned().collect::<Vec<_>>().join(", ")
            ));
        }
    }

    // Per-point contributor view for the table.
    let mut entries_by_point: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    for o in &observed {
        if by_id.contains_key(o.entry.point.as_str()) {
            entries_by_point
                .entry(o.entry.point.as_str())
                .or_default()
                .push(format!("{} @ {}", o.contributor, o.process));
        }
    }

    println!("== profile: {name} ==");
    println!(
        "{:<40} | {:<12} | {:<48} | STATUS",
        "POINT", "KIND", "ENTRIES (contributor @ process)"
    );
    println!("{}", "-".repeat(120));
    for p in points {
        let kind = match p.kind {
            ExtensionKind::EntityMenu => "EntityMenu",
            ExtensionKind::ModalActions => "ModalActions",
        };
        let entries_str = entries_by_point
            .get(p.id)
            .map(|v| v.join(", "))
            .unwrap_or_else(|| "NONE".to_string());
        let status = if flagged.contains(p.id) {
            "<-- FINDING"
        } else if entries_by_point.contains_key(p.id) {
            "OK"
        } else {
            "OK (no entries)"
        };
        println!("{:<40} | {kind:<12} | {entries_str:<48} | {status}", p.id);
    }
    println!();

    if findings.is_empty() {
        println!("admincheck [{name}]: OK — every contributed extension entry targets a declared point");
    } else {
        eprintln!("admincheck [{name}]: {} FINDING(S):", findings.len());
        for f in &findings {
            eprintln!("  - {f}");
        }
    }
    println!();
    Ok(findings.len())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // A tokio runtime must be live: an in-process `Bus::on` during `init` spawns a task.
    let strict = std::env::args().any(|a| a == "--strict");

    let points = declared_points();
    println!("admincheck: contributed extension entries vs declared extension points\n");

    let mut total_findings = 0usize;
    for (name, profile) in [
        ("Monolith", DeploymentProfile::Monolith),
        ("Split", DeploymentProfile::Split),
    ] {
        total_findings += run_profile(name, &profile, &points)?;
    }

    if total_findings == 0 {
        println!(
            "admincheck: OK — all {} declared extension points validated against contributed \
             entries in both profiles",
            points.len()
        );
    }

    if strict && total_findings > 0 {
        std::process::exit(1);
    }
    Ok(())
}
