//! `topiccheck` — the defined-vs-subscribed event-topic drift check (Step 14b), the
//! Rust redesign of Go's `experiments/go-sketch/tools/topiccheck`.
//!
//! ## What it flags
//! An event topic DECLARED via `bus::define` (one of the `<name>events` crates'
//! `LazyLock<EventType<T>>` statics) but never SUBSCRIBED anywhere — in-process
//! (`Bus::on`), or on the durable plane (`Bus::on_tx` / `Bus::on_tx_raw`). A
//! defined-but-unsubscribed topic is dead vocabulary — an event nobody reacts to —
//! so surfacing it keeps the published-event surface honest as the monolith grows.
//!
//! ## Why not linkme (the plan's first sketch), and why a runtime harness
//! The plan named `linkme` distributed slices: a DEFINE macro and a SUBSCRIBE macro
//! that each record a `{topic, role}` entry at link time. That was rejected as
//! DISHONEST: a `subscribes!("x")` annotation is a hand-written claim decoupled from
//! the actual `on_tx` call — it can drift (annotate a subscribe that isn't wired, or
//! wire one without the annotation) and still pass. Go avoided that by observing the
//! REAL `bus.On` call sites through whole-program `go/types` object identity; Rust
//! has no equivalent whole-program analysis.
//!
//! So this observes the REAL wiring at runtime instead: it builds the MONOLITH module
//! set (the superset of every process — so a subscribe in ANY process counts), runs
//! the two lifecycle phases that do the wiring (`register` → `init`), and records
//! every subscription that actually happened. No per-call-site macro noise; a
//! string-literal `on_tx_raw` topic that drifts is caught for free (it simply won't
//! match a defined topic). Trade-off taken: the DEFINE set is enumerated explicitly
//! below (Rust can't discover the `bus::define` statics without whole-program
//! analysis) — the one conscious edit point, exactly like `audit`'s `DURABLE_TOPICS`.
//! Referencing each static directly means a renamed/removed topic breaks THIS tool at
//! compile time.
//!
//! ## No live DB needed
//! `register`/`init` do NO I/O (lifecycle constraint 8 — only `migrate`/`start` touch
//! the DB, and this tool runs neither), so the shared pool is a `connect_lazy` handle
//! that never connects. If a future module ever queried in `init`, the local Postgres
//! DSN (`DATABASE_URL`, else the dev default) is the fallback — but today none does.
//!
//! ## Second finding class: durability (seam #3 / constraint 7)
//! Every entry in `defined_topics()` is a cross-module CONTRACT topic (a `bus::define`
//! static in an `api/*/events` crate), so it MUST be delivered durably (`on_tx` /
//! `on_tx_raw`), never plain in-process `on()`. The harness records in-process subs
//! under the `IN_PROCESS_SENTINEL` subscriber; a defined topic carrying that sentinel is
//! a durability violation. HONEST precision limit: `bus().subscribed_topics()` gives NO
//! subscriber-module attribution (all in-process subs collapse to the one sentinel), so
//! the provable claim is the stricter *"no in-process subscription to ANY defined topic,
//! even the module's own"* — NOT "cross-module durability", which this cannot prove.
//! `ALLOW_INPROCESS_DEFINED` is the escape hatch for a future legitimate same-module
//! reaction.
//!
//! ## Flags / exit
//! Advisory by default (prints the table, exits 0). `--strict` exits non-zero on ANY
//! finding (unsubscribed OR durability) — the everything-strict gate. `--durability-strict`
//! exits non-zero ONLY on a durability finding (ignoring unsubscribed) — the BLOCKING
//! `fortress`-stage invocation, since a durability violation breaches a HARD constraint
//! while an unsubscribed topic is merely advisory dead vocabulary.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use bus::{AnyTx, Error, EventContract, SubscriptionSpec, Transport, TxHandler};
use checkmodules::monolith_modules;
use lifecycle::{App, Context};

/// Dev-default DSN (mirrors CLAUDE.md). Only ever used to build a LAZY pool that
/// never connects — `register`/`init` do no I/O.
const DEFAULT_DSN: &str =
    "postgres://gamebackend:gamebackend@localhost:5432/gamebackend?sslmode=disable";

/// Topics deliberately defined without a subscriber (Go's
/// `//topiccheck:allow-unsubscribed`). Empty today: every one of the six defined
/// topics has a live subscriber (see the table this prints). A new emit-only topic
/// with no reactor yet is added here, with a reason comment.
const ALLOW_UNSUBSCRIBED: &[&str] = &[];

/// Defined (contract) topics that are LEGITIMATELY subscribed same-module on the
/// in-process plane (plain `on()`). CLAUDE.md seam #3 permits plain `on()` for a
/// same-module reaction; the durability rule below forbids it for ANY defined topic,
/// because `bus().subscribed_topics()` carries no subscriber-module attribution (every
/// in-process sub collapses to the `"(in-process)"` sentinel — main.rs ~line 205), so
/// the tool cannot prove a sub is same-module vs cross-module. This allowlist is the
/// only escape hatch: whitelist such a topic HERE with a reason comment. Empty today —
/// the clean tree has zero in-process subscriptions to any defined topic.
const ALLOW_INPROCESS_DEFINED: &[&str] = &[];

/// The sentinel subscriber string that `collect_subscriptions` records for every
/// in-process (`Bus::on`) subscription, since `bus().subscribed_topics()` returns bare
/// topic strings with no subscriber-module attribution. Both the merge site
/// (`collect_subscriptions`) and the durability check key off this exact string — a
/// conscious edit point, like the manual `defined_topics()` enumeration.
const IN_PROCESS_SENTINEL: &str = "(in-process)";

/// A `bus::Transport` that records every durable subscription instead of persisting
/// anything. `enqueue_tx` is a no-op (nothing is emitted during `register`/`init`);
/// `subscribe_tx` captures the `(topic, subscriber)` pair each `on_tx`/`on_tx_raw`
/// registers. Injected as this tool's own durable-events plane transport, so building
/// the module set drives real subscribe calls straight into `subs`.
struct RecordingTransport {
    subs: Arc<Mutex<Vec<(String, String)>>>,
}

#[async_trait::async_trait]
impl Transport for RecordingTransport {
    async fn enqueue_tx(
        &self,
        _tx: AnyTx<'_>,
        _contract: &EventContract,
        _payload: &[u8],
    ) -> Result<(), Error> {
        Ok(())
    }

    /// Records `(topic, spec.id)` — the subscription id is the durable-plane
    /// subscriber label this tool reports. `version`/`spec.start` validations
    /// arrive with the pull-plane checker rework (plan Step 11).
    fn subscribe_tx(
        &self,
        spec: SubscriptionSpec,
        topic: &str,
        _version: u32,
        _history: Option<bus::HistoryPolicy>,
        _handler: Arc<dyn TxHandler>,
    ) {
        self.subs
            .lock()
            .unwrap()
            .push((topic.to_string(), spec.id.to_string()));
    }
}

/// The DEFINE sites: the canonical `bus::define` statics, each paired with a label
/// naming the static. Referenced directly so a renamed/removed/added topic forces an
/// edit here and breaks the build otherwise.
fn defined_topics() -> Vec<(String, &'static str)> {
    vec![
        (
            accountsevents::PLAYER_REGISTERED.topic().to_string(),
            "accountsevents::PLAYER_REGISTERED",
        ),
        (
            charactersevents::CREATED.topic().to_string(),
            "charactersevents::CREATED",
        ),
        (
            charactersevents::DELETED.topic().to_string(),
            "charactersevents::DELETED",
        ),
        (
            configevents::CHANGED.topic().to_string(),
            "configevents::CHANGED",
        ),
        (
            matchevents::FINISHED.topic().to_string(),
            "matchevents::FINISHED",
        ),
        (
            schedulerevents::FIRED.topic().to_string(),
            "schedulerevents::FIRED",
        ),
    ]
}

/// The pure diff: every defined topic that is neither subscribed nor allowlisted.
/// Factored out so it is unit-testable without the lifecycle harness.
fn unsubscribed(
    defined: &[(String, &'static str)],
    subscribed: &BTreeSet<String>,
    allow: &[&str],
) -> Vec<String> {
    defined
        .iter()
        .filter(|(t, _)| !subscribed.contains(t) && !allow.contains(&t.as_str()))
        .map(|(t, _)| t.clone())
        .collect()
}

/// The pure durability diff: every defined (contract) topic that has an in-process
/// (`Bus::on`) subscriber — i.e. `by_topic[topic]` contains the `IN_PROCESS_SENTINEL` —
/// and is not allowlisted. A published contract topic must be delivered durably
/// (`on_tx`/`on_tx_raw`), so an in-process subscription to one is a seam #3 / constraint
/// 7 violation. HONEST claim (per the plan's precision limit): because
/// `subscribed_topics()` collapses every in-process sub to one sentinel with NO
/// subscriber-module attribution, this proves only "no in-process subscription to any
/// defined topic" — it cannot distinguish a legitimate same-module reaction from a
/// cross-module violation, hence the `ALLOW_INPROCESS_DEFINED` escape hatch. Factored
/// out (like `unsubscribed`) so it is unit-testable without the lifecycle harness.
fn inprocess_defined(
    defined: &[(String, &'static str)],
    by_topic: &BTreeMap<String, BTreeSet<String>>,
    allow: &[&str],
) -> Vec<String> {
    defined
        .iter()
        .filter(|(t, _)| {
            !allow.contains(&t.as_str())
                && by_topic
                    .get(t)
                    .is_some_and(|s| s.contains(IN_PROCESS_SENTINEL))
        })
        .map(|(t, _)| t.clone())
        .collect()
}

/// Runs the harness: builds the module set with a recording transport and returns the
/// `(topic -> subscribers)` map observed across both planes.
fn collect_subscriptions() -> anyhow::Result<BTreeMap<String, BTreeSet<String>>> {
    // A LAZY pool: never connects, since register/init do no I/O (constraint 8).
    let dsn = std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DSN.to_string());
    let pool = sqlx::postgres::PgPool::connect_lazy(&dsn)
        .map_err(|e| anyhow::anyhow!("topiccheck: build lazy pool: {e}"))?;

    // Inject a recording durable-events plane transport, then build the set: every
    // real on_tx/on_tx_raw lands in `durable`, every real in-process `on` in the bus.
    let durable = Arc::new(Mutex::new(Vec::<(String, String)>::new()));
    let ctx = Arc::new(Context::with_db_and_transport(
        pool,
        Arc::new(RecordingTransport {
            subs: durable.clone(),
        }),
    ));

    let mut app = App::new(ctx.clone());
    for m in monolith_modules() {
        app.add(m);
    }
    app.build()
        .map_err(|e| anyhow::anyhow!("topiccheck: lifecycle build failed: {e:#}"))?;

    let mut by_topic: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (topic, subscriber) in durable.lock().unwrap().iter() {
        by_topic
            .entry(topic.clone())
            .or_default()
            .insert(subscriber.clone());
    }
    for topic in ctx.bus().subscribed_topics() {
        by_topic
            .entry(topic)
            .or_default()
            .insert(IN_PROCESS_SENTINEL.to_string());
    }
    Ok(by_topic)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // `--strict`: superset gate — exit non-zero on ANY finding (unsubscribed OR
    // durability). `--durability-strict`: the BLOCKING verify invocation (fortress
    // stage, wired in Step 3) — exit non-zero ONLY on a durability finding, ignoring
    // unsubscribed (which is advisory "dead vocabulary"). Both still print the table.
    let strict = std::env::args().any(|a| a == "--strict");
    let durability_strict = std::env::args().any(|a| a == "--durability-strict");

    let by_topic = collect_subscriptions()?;
    let subscribed: BTreeSet<String> = by_topic.keys().cloned().collect();
    let defined = defined_topics();
    let findings = unsubscribed(&defined, &subscribed, ALLOW_UNSUBSCRIBED);
    let durability = inprocess_defined(&defined, &by_topic, ALLOW_INPROCESS_DEFINED);
    let durability_set: BTreeSet<&str> = durability.iter().map(|t| t.as_str()).collect();

    // The report table: every defined topic, its define site, its subscribers, and a
    // status column carrying BOTH finding classes (unsubscribed / in-process durability).
    println!("topiccheck: defined-vs-subscribed event topics\n");
    let header = format!(
        "{:<20} | {:<34} | {:<38} | STATUS",
        "TOPIC", "DEFINED AT", "SUBSCRIBERS"
    );
    println!("{header}");
    println!("{}", "-".repeat(110));
    for (topic, site) in &defined {
        let subs = by_topic.get(topic);
        let (subs_str, mut status) = match subs {
            Some(s) if !s.is_empty() => (s.iter().cloned().collect::<Vec<_>>().join(", "), "OK"),
            _ if ALLOW_UNSUBSCRIBED.contains(&topic.as_str()) => {
                ("(none — allowlisted)".to_string(), "OK (allowlisted)")
            }
            _ => ("NONE".to_string(), "<-- UNSUBSCRIBED"),
        };
        // A durability finding takes precedence in the status column: a defined topic
        // with an in-process subscriber is a seam #3 / constraint 7 violation.
        if durability_set.contains(topic.as_str()) {
            status = "<-- IN-PROCESS (durability violation)";
        }
        println!("{topic:<20} | {site:<34} | {subs_str:<38} | {status}");
    }
    println!();

    // Report both finding classes distinctly.
    if !durability.is_empty() {
        eprintln!(
            "topiccheck: DURABILITY FAIL — {} defined (contract) topic(s) have an in-process \
             subscription; a published contract topic must be delivered durably (on_tx / \
             on_tx_raw), never plain on() (CLAUDE.md seam #3, constraint 7):",
            durability.len()
        );
        for t in &durability {
            eprintln!("  - {t}");
        }
    }
    if !findings.is_empty() {
        eprintln!(
            "topiccheck: UNSUBSCRIBED — {} defined topic(s) have no subscriber:",
            findings.len()
        );
        for t in &findings {
            eprintln!("  - {t}");
        }
    }
    if durability.is_empty() && findings.is_empty() {
        println!(
            "topiccheck: OK — all {} defined topics are subscribed durably (or allowlisted)",
            defined.len()
        );
    }

    // Exit logic. `--durability-strict` blocks ONLY on a durability finding; `--strict`
    // blocks on either class (everything-strict superset).
    if durability_strict && !durability.is_empty() {
        std::process::exit(1);
    }
    if strict && (!durability.is_empty() || !findings.is_empty()) {
        std::process::exit(1);
    }
    Ok(())
}

#[cfg(test)]
mod tests;
