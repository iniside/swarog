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
//! Advisory by default (prints the table, exits 0). With `--strict` it exits non-zero
//! when any non-allowlisted topic is unsubscribed, for use as the verify gate.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use bus::{Error, Transport, TxHandler};
use lifecycle::{App, Context, Module};

/// Dev-default DSN (mirrors CLAUDE.md). Only ever used to build a LAZY pool that
/// never connects — `register`/`init` do no I/O.
const DEFAULT_DSN: &str =
    "postgres://gamebackend:gamebackend@localhost:5432/gamebackend?sslmode=disable";

/// Topics deliberately defined without a subscriber (Go's
/// `//topiccheck:allow-unsubscribed`). Empty today: every one of the six defined
/// topics has a live subscriber (see the table this prints). A new emit-only topic
/// with no reactor yet is added here, with a reason comment.
const ALLOW_UNSUBSCRIBED: &[&str] = &[];

/// A `bus::Transport` that records every durable subscription instead of persisting
/// anything. `enqueue_tx` is a no-op (nothing is emitted during `register`/`init`);
/// `subscribe_tx` captures the `(topic, subscriber)` pair each `on_tx`/`on_tx_raw`
/// registers. Installed in place of `core/messaging`'s real transport, so building
/// the module set drives real subscribe calls straight into `subs`.
struct RecordingTransport {
    subs: Arc<Mutex<Vec<(String, String)>>>,
}

#[async_trait::async_trait]
impl Transport for RecordingTransport {
    async fn enqueue_tx(
        &self,
        _conn: &mut sqlx::PgConnection,
        _topic: &str,
        _payload: &[u8],
    ) -> Result<(), Error> {
        Ok(())
    }

    fn subscribe_tx(&self, topic: &str, subscriber: &str, _handler: Arc<dyn TxHandler>) {
        self.subs
            .lock()
            .unwrap()
            .push((topic.to_string(), subscriber.to_string()));
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

/// The monolith module set — the superset of every process, so a subscribe in ANY
/// deployment counts. Mirrors `cmd/server` MINUS `messaging` (the recording transport
/// stands in for its durable plane) and with a plain `Gateway::new()` (the player-edge
/// wiring is irrelevant to topic subscriptions).
fn monolith_modules() -> Vec<Box<dyn Module>> {
    vec![
        Box::new(config::Config::new()),
        Box::new(characters::Characters::new()),
        Box::new(inventory::Inventory::new()),
        Box::new(accounts::Accounts::new()),
        Box::new(admin::Admin::new()),
        Box::new(audit::Audit::new()),
        Box::new(scheduler::Scheduler::new()),
        Box::new(rating::Rating::new()),
        Box::new(match_module::MatchModule::new()),
        Box::new(leaderboard::LeaderboardModule::new()),
        Box::new(webui::WebUi::new()),
        Box::new(gateway::Gateway::new()),
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

/// Runs the harness: builds the module set with a recording transport and returns the
/// `(topic -> subscribers)` map observed across both planes.
fn collect_subscriptions() -> anyhow::Result<BTreeMap<String, BTreeSet<String>>> {
    // A LAZY pool: never connects, since register/init do no I/O (constraint 8).
    let dsn = std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DSN.to_string());
    let pool = sqlx::postgres::PgPool::connect_lazy(&dsn)
        .map_err(|e| anyhow::anyhow!("topiccheck: build lazy pool: {e}"))?;
    let ctx = Arc::new(Context::with_db(pool));

    // Replace the durable transport with a recording one, then build the set: every
    // real on_tx/on_tx_raw lands in `durable`, every real in-process `on` in the bus.
    let durable = Arc::new(Mutex::new(Vec::<(String, String)>::new()));
    ctx.bus().set_transport(Arc::new(RecordingTransport {
        subs: durable.clone(),
    }));

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
            .insert("(in-process)".to_string());
    }
    Ok(by_topic)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let strict = std::env::args().any(|a| a == "--strict");

    let by_topic = collect_subscriptions()?;
    let subscribed: BTreeSet<String> = by_topic.keys().cloned().collect();
    let defined = defined_topics();
    let findings = unsubscribed(&defined, &subscribed, ALLOW_UNSUBSCRIBED);

    // The report table: every defined topic, its define site, and its subscribers.
    println!("topiccheck: defined-vs-subscribed event topics\n");
    let header = format!("{:<20} | {:<34} | SUBSCRIBERS", "TOPIC", "DEFINED AT");
    println!("{header}");
    println!("{}", "-".repeat(90));
    for (topic, site) in &defined {
        let subs = by_topic.get(topic);
        let (subs_str, marker) = match subs {
            Some(s) if !s.is_empty() => {
                (s.iter().cloned().collect::<Vec<_>>().join(", "), "")
            }
            _ if ALLOW_UNSUBSCRIBED.contains(&topic.as_str()) => {
                ("(none — allowlisted)".to_string(), "")
            }
            _ => ("NONE".to_string(), "  <-- UNSUBSCRIBED"),
        };
        println!("{topic:<20} | {site:<34} | {subs_str}{marker}");
    }
    println!();

    if findings.is_empty() {
        println!("topiccheck: OK — all {} defined topics are subscribed (or allowlisted)", defined.len());
        return Ok(());
    }

    eprintln!(
        "topiccheck: FAIL — {} defined topic(s) have no subscriber:",
        findings.len()
    );
    for t in &findings {
        eprintln!("  - {t}");
    }
    if strict {
        std::process::exit(1);
    }
    Ok(())
}

#[cfg(test)]
mod tests;
