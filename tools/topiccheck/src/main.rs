//! `topiccheck` — the defined-vs-subscribed event-topic drift check, reworked for the
//! pull-plane seam (plan Step 11). It observes the REAL wiring at runtime: for each
//! deployment profile (Monolith AND Split, from `checkmodules`) it builds every
//! process's module set with a recording durable-events transport, runs the two no-I/O
//! lifecycle phases (`register` → `init`), and validates the durable subscriptions that
//! actually got wired against the six defined contract topics.
//!
//! ## Why a runtime harness (not a `linkme`/annotation scheme)
//! A hand-written `subscribes!("x")` annotation can drift from the real `on_tx` call and
//! still pass. Go observed the REAL `bus.On` call sites via whole-program `go/types`;
//! Rust has no equivalent. So this drives the actual `register`/`init` wiring and records
//! every `on_tx`/`on_tx_raw` at the transport seam — a drifted string-literal topic is
//! caught for free (it won't match a defined contract). The DEFINE set is enumerated
//! explicitly below (the one conscious edit point, like `audit`'s `DURABLE_TOPICS`),
//! referencing each `bus::define` static directly so a renamed/removed topic breaks THIS
//! tool at compile time.
//!
//! ## Validations, applied PER deployment profile
//! 1. **version match** — every durable subscription's `(topic, version)` matches a
//!    defined contract (an undefined topic or a version mismatch is a seam violation).
//! 2. **single host** — each subscription id is hosted by exactly ONE process in the
//!    profile. The `Bus` already panics on a duplicate id WITHIN a process (so a
//!    same-process double-wire dies at build); this catches CROSS-process duplicates in
//!    the split (an id wired into two svc's module sets). Replicas of one service are the
//!    same process, listed once, so they never look like duplicates.
//! 3. **planeless processes host nothing** — `gateway-svc` hosts no DB / durable-events
//!    plane, so it must host ZERO durable subscriptions (asserted explicitly: the harness
//!    injects a transport so a stray `on_tx` there would NOT panic — this check is what
//!    catches it).
//! 4. **durability** (seam #3 / constraint 7) — a defined contract topic must never be
//!    subscribed in-process via plain `on()`; it must be delivered durably.
//! 5. **unsubscribed** (seam) — a defined contract topic with no durable subscriber in
//!    the profile, unless it is in `ALLOW_UNSUBSCRIBED`. A sinkless defined event is an
//!    explicit, diff-reviewed decision, not silent drift.
//!
//! ## No live DB needed
//! `register`/`init` do NO I/O (constraint 8), so each process's shared pool is a
//! `connect_lazy` handle that never connects — the same trick `checkmodules` relies on
//! for its dummy `ProcessWiring`. Building all 13 processes (1 monolith + 12 svc) touches
//! no database.
//!
//! ## Flags / exit
//! Advisory by default (prints the per-profile tables, exits 0). `--durability-strict`
//! (the BLOCKING `fortress`-stage invocation) exits non-zero on ANY SEAM violation in ANY
//! profile: the version / single-host / planeless / in-process-durability checks PLUS
//! `unsubscribed` (a defined contract with no subscriber in the profile and not in
//! `ALLOW_UNSUBSCRIBED`) — each breaches a hard durable-plane invariant. `--strict` (the
//! everything-strict gate) additionally exits non-zero on any advisory finding; the
//! advisory bucket is empty today (unsubscribed became a seam), so the two flags are
//! momentarily equivalent — `--strict` survives as the gate for any future advisory.

mod golden;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use bus::{AnyTx, Error, EventContract, HistoryPolicy, SubscriptionSpec, Transport, TxHandler};
use checkmodules::DeploymentProfile;
use lifecycle::{App, Context};

/// Dev-default DSN (mirrors CLAUDE.md). Only ever used to build a LAZY pool that never
/// connects — `register`/`init` do no I/O.
const DEFAULT_DSN: &str =
    "postgres://gamebackend:gamebackend@localhost:5432/gamebackend?sslmode=disable";

/// Sanctioned-sinkless registry: topics deliberately DEFINED and emitted today with no
/// durable subscriber yet ("emitting now, consumer comes later"). Adding a topic here is
/// the explicit, diff-reviewed decision to ship a sinkless event; a defined topic NOT
/// listed here and unsubscribed in a profile is a SEAM violation that fails
/// `--durability-strict`. Empty today: every one of the six defined topics has a live
/// durable subscriber in both profiles.
const ALLOW_UNSUBSCRIBED: &[&str] = &[];

/// Defined (contract) topics legitimately subscribed same-module on the in-process plane
/// (plain `on()`). Empty today — the clean tree has zero in-process subscriptions to any
/// defined topic. A future legitimate same-module reaction is whitelisted here with a
/// reason comment (the tool cannot prove same-module vs cross-module, so this is the
/// escape hatch — see check 4).
const ALLOW_INPROCESS_DEFINED: &[&str] = &[];

/// Processes that host no DB / durable-events plane and therefore must host ZERO durable
/// subscriptions. `gateway-svc` is the single front door with no store. `admin-svc` is
/// NOT listed anymore: since the admin-hardening rollout it is DB-backed (the admin
/// module owns schema `admin` for session auth and emits durable `admin.action`), so it
/// hosts the plane like any other DB-backed process. The monolith `server` DOES host the
/// plane, so it is not listed either.
const PLANELESS_PROCESSES: &[&str] = &["gateway-svc"];

/// A defined contract topic: the topic string, its payload-shape version, and its
/// history policy. Built from each `bus::define` static's [`EventContract`] — referenced
/// directly in [`defined_topics`], so a renamed/removed topic breaks THIS tool at compile
/// time.
#[derive(Clone)]
struct Contract {
    topic: String,
    version: u32,
    history: HistoryPolicy,
}

/// One durable subscription observed during a profile's `register`/`init`, tagged with
/// the process that wired it.
#[derive(Clone)]
struct Sub {
    id: String,
    topic: String,
    version: u32,
    /// The publisher's history policy the subscription carried (`Some` for a typed
    /// `on_tx`, `None` for a raw `on_tx_raw` sink) — cross-checked against the contract.
    history: Option<HistoryPolicy>,
    process: &'static str,
}

/// What one profile's harness run observes across all its processes.
struct Observation {
    /// Every durable subscription, across every process in the profile.
    subs: Vec<Sub>,
    /// Topics carrying an in-process (plain `on`/`subscribe`) subscriber, across every
    /// process — the topic-level view from [`bus::Bus::subscribed_topics`], including raw
    /// string-keyed `subscribe` callers that carry no version.
    inprocess: BTreeSet<String>,
    /// `(topic, version)` pairs carrying an in-process typed `on()` subscriber, across
    /// every process — the version-aware view from [`bus::Bus::subscribed_contracts`].
    inprocess_contracts: BTreeSet<(String, u32)>,
}

/// A `bus::Transport` that records every durable subscription instead of persisting
/// anything. `enqueue_tx` is a no-op (nothing is emitted during `register`/`init`);
/// `subscribe_tx` captures `(spec.id, topic, version, history)` per `on_tx`/`on_tx_raw`.
struct RecordingTransport {
    subs: Arc<Mutex<Vec<Recorded>>>,
}

/// One recorded `subscribe_tx` call — the raw tuple the transport sees, before it is
/// tagged with its hosting process into a [`Sub`].
struct Recorded {
    id: String,
    topic: String,
    version: u32,
    history: Option<HistoryPolicy>,
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

    fn subscribe_tx(
        &self,
        spec: SubscriptionSpec,
        topic: &str,
        version: u32,
        history: Option<HistoryPolicy>,
        _handler: Arc<dyn TxHandler>,
    ) {
        self.subs.lock().unwrap().push(Recorded {
            id: spec.id.to_string(),
            topic: topic.to_string(),
            version,
            history,
        });
    }
}

/// The DEFINE sites: the canonical `bus::define` statics, each read for its full
/// contract (topic + version + history) and paired with the static's label. Referenced
/// directly so a renamed/removed/added topic forces an edit here and breaks the build.
fn defined_topics() -> Vec<Contract> {
    fn of(k: &EventContract) -> Contract {
        Contract {
            topic: k.topic.to_string(),
            version: k.version,
            history: k.history,
        }
    }
    vec![
        of(accountsevents::PLAYER_REGISTERED.contract()),
        of(charactersevents::CREATED.contract()),
        of(charactersevents::DELETED.contract()),
        of(configevents::CHANGED.contract()),
        of(matchevents::FINISHED.contract()),
        of(schedulerevents::FIRED.contract()),
        of(adminevents::ACTION.contract()),
    ]
}

/// Check 1 — every durable subscription's `(topic, version)` matches a defined contract.
/// An undefined topic (a drifted string literal) or a version the contract does not
/// publish is a seam violation.
fn version_findings(defined: &[Contract], subs: &[Sub]) -> Vec<String> {
    // Keyed by (topic, version): the documented model lets a topic coexist at v1
    // and v2 (`bus::define(topic, 2, …)`), so a topic-only key would collide the
    // two contracts and misreport drift. The value is the matched contract, whose
    // history the belt-and-suspenders check below reads.
    let by_key: BTreeMap<(&str, u32), &Contract> = defined
        .iter()
        .map(|c| ((c.topic.as_str(), c.version), c))
        .collect();
    let mut out = Vec::new();
    for s in subs {
        match by_key.get(&(s.topic.as_str(), s.version)) {
            Some(c) => {
                // Belt-and-suspenders: a typed subscription's carried history policy must
                // equal the matched contract's (both derive from the same EventType, so a
                // mismatch would signal a corrupted seam). Raw sinks carry None and are
                // exempt. The contract is the exact (topic, version) match.
                if let Some(h) = s.history {
                    if h != c.history {
                        out.push(format!(
                            "subscription {:?} (process {}) carries history {:?} but contract {:?} declares {:?}",
                            s.id, s.process, h, s.topic, c.history
                        ));
                    }
                }
            }
            None => {
                // The (topic, version) pair is unmatched: distinguish an entirely
                // undefined topic (a drifted string literal) from a defined topic
                // subscribed at a version no contract publishes.
                let versions: Vec<u32> =
                    defined.iter().filter(|c| c.topic == s.topic).map(|c| c.version).collect();
                if versions.is_empty() {
                    out.push(format!(
                        "subscription {:?} (process {}) subscribes UNDEFINED topic {:?} — no \
                         bus::define contract declares it",
                        s.id, s.process, s.topic
                    ));
                } else {
                    let defined_str =
                        versions.iter().map(|v| format!("v{v}")).collect::<Vec<_>>().join(", ");
                    out.push(format!(
                        "subscription {:?} (process {}) subscribes {:?} at v{} but the contract defines {}",
                        s.id, s.process, s.topic, s.version, defined_str
                    ));
                }
            }
        }
    }
    out
}

/// Checks 2 + 3 restated — each subscription id must be hosted by exactly ONE process in
/// the profile. `>1` process is a cross-process duplicate host.
fn host_findings(subs: &[Sub]) -> Vec<String> {
    let mut hosts: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for s in subs {
        hosts.entry(s.id.as_str()).or_default().insert(s.process);
    }
    hosts
        .iter()
        .filter(|(_, ps)| ps.len() > 1)
        .map(|(id, ps)| {
            format!(
                "subscription {:?} hosted by {} processes ({}) — a subscription must be hosted by \
                 exactly one process per profile",
                id,
                ps.len(),
                ps.iter().cloned().collect::<Vec<_>>().join(", ")
            )
        })
        .collect()
}

/// Check 3 — a planeless process (`gateway-svc`) must host ZERO durable subscriptions.
fn planeless_findings(subs: &[Sub], planeless: &[&str]) -> Vec<String> {
    subs.iter()
        .filter(|s| planeless.contains(&s.process))
        .map(|s| {
            format!(
                "subscription {:?} hosted by planeless process {} — it hosts no DB / \
                 durable-events plane and must host zero durable subscriptions",
                s.id, s.process
            )
        })
        .collect()
}

/// Check 4 (topic-level pass) — a defined contract topic must never be subscribed
/// in-process (plain `on`); it must be delivered durably (`on_tx`/`on_tx_raw`).
/// Runs over the union of in-process topics from [`bus::Bus::subscribed_topics`],
/// which includes raw string-keyed `subscribe` callers that carry NO version.
/// Returns the offending topics. Paired with [`inprocess_defined_contracts`] (the
/// version-aware pass over `on()` registrations) — a finding from either fails,
/// so raw-subscribe coverage is not regressed by the tuple key.
fn inprocess_defined(defined: &[Contract], inprocess: &BTreeSet<String>, allow: &[&str]) -> Vec<String> {
    defined
        .iter()
        .filter(|c| !allow.contains(&c.topic.as_str()) && inprocess.contains(&c.topic))
        .map(|c| c.topic.clone())
        .collect()
}

/// Check 4 (tuple-aware pass) — the version-aware companion to
/// [`inprocess_defined`], run over the `(topic, version)` set from
/// [`bus::Bus::subscribed_contracts`] (populated only by typed `on()`, which holds
/// the contract version). Flags a defined `(topic, version)` contract subscribed
/// in-process. Returns the offending topics.
fn inprocess_defined_contracts(
    defined: &[Contract],
    inprocess: &BTreeSet<(String, u32)>,
    allow: &[&str],
) -> Vec<String> {
    defined
        .iter()
        .filter(|c| {
            !allow.contains(&c.topic.as_str()) && inprocess.contains(&(c.topic.clone(), c.version))
        })
        .map(|c| c.topic.clone())
        .collect()
}

/// Check 5 (seam) — a defined `(topic, version)` contract with no durable subscriber
/// in the profile and not in `allow` (the sanctioned-sinkless registry). Keyed on
/// `(topic, version)`, so a topic present at one version but subscribed only at another
/// still reports the unsubscribed version. A non-empty result fails `--durability-strict`.
fn unsubscribed(
    defined: &[Contract],
    subscribed: &BTreeSet<(String, u32)>,
    allow: &[&str],
) -> Vec<String> {
    defined
        .iter()
        .filter(|c| {
            !subscribed.contains(&(c.topic.clone(), c.version)) && !allow.contains(&c.topic.as_str())
        })
        .map(|c| c.topic.clone())
        .collect()
}

/// Builds every process of `profile` with a recording transport + lazy pool, runs the
/// two no-I/O lifecycle phases, and returns the durable subscriptions (tagged with their
/// hosting process) plus the union of in-process topics.
fn observe(profile: &DeploymentProfile) -> anyhow::Result<Observation> {
    let dsn = std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DSN.to_string());
    let mut subs = Vec::new();
    let mut inprocess = BTreeSet::new();
    let mut inprocess_contracts = BTreeSet::new();

    for (process_id, mods) in profile.processes() {
        // A LAZY pool per process: never connects, since register/init do no I/O.
        let pool = sqlx::postgres::PgPool::connect_lazy(&dsn)
            .map_err(|e| anyhow::anyhow!("topiccheck: {process_id}: build lazy pool: {e}"))?;
        let recorded = Arc::new(Mutex::new(Vec::<Recorded>::new()));
        let ctx = Arc::new(Context::with_db_and_transport(
            pool,
            Arc::new(RecordingTransport {
                subs: recorded.clone(),
            }),
        ));

        let mut app = App::new(ctx.clone());
        for m in mods {
            app.add(m);
        }
        app.build().map_err(|e| {
            anyhow::anyhow!("topiccheck: {process_id}: lifecycle build failed: {e:#}")
        })?;

        for r in recorded.lock().unwrap().iter() {
            subs.push(Sub {
                id: r.id.clone(),
                topic: r.topic.clone(),
                version: r.version,
                history: r.history,
                process: process_id,
            });
        }
        for topic in ctx.bus().subscribed_topics() {
            inprocess.insert(topic);
        }
        for tv in ctx.bus().subscribed_contracts() {
            inprocess_contracts.insert(tv);
        }
    }
    Ok(Observation {
        subs,
        inprocess,
        inprocess_contracts,
    })
}

/// Runs every check for one profile, prints its report table + findings, and returns
/// `(seam_findings, advisory_findings)` counts folded into the two exit buckets.
fn run_profile(name: &str, profile: &DeploymentProfile, defined: &[Contract]) -> anyhow::Result<(bool, bool)> {
    let obs = observe(profile)?;
    let subscribed: BTreeSet<(String, u32)> =
        obs.subs.iter().map(|s| (s.topic.clone(), s.version)).collect();

    let versions = version_findings(defined, &obs.subs);
    let hosts = host_findings(&obs.subs);
    let planeless = planeless_findings(&obs.subs, PLANELESS_PROCESSES);
    // Check 4 is a DUAL pass: the version-aware pass over typed `on()`
    // registrations AND the topic-level pass over the raw `subscribe` set (which
    // carries no version). A finding from either fails — folding both into one
    // topic list so the table's status column reflects both.
    let mut durability =
        inprocess_defined_contracts(defined, &obs.inprocess_contracts, ALLOW_INPROCESS_DEFINED);
    for t in inprocess_defined(defined, &obs.inprocess, ALLOW_INPROCESS_DEFINED) {
        if !durability.contains(&t) {
            durability.push(t);
        }
    }
    let unsub = unsubscribed(defined, &subscribed, ALLOW_UNSUBSCRIBED);

    // Per-(topic, version) subscriber view for the table.
    let mut by_key: BTreeMap<(&str, u32), Vec<String>> = BTreeMap::new();
    for s in &obs.subs {
        by_key
            .entry((s.topic.as_str(), s.version))
            .or_default()
            .push(format!("{} @ {}", s.id, s.process));
    }

    println!("== profile: {name} ==");
    let header = format!(
        "{:<24} | {:<12} | {:<48} | STATUS",
        "TOPIC vN", "HISTORY", "SUBSCRIBERS (id @ process)"
    );
    println!("{header}");
    println!("{}", "-".repeat(110));
    for c in defined {
        let key = (c.topic.as_str(), c.version);
        let subs_str = by_key
            .get(&key)
            .map(|v| v.join(", "))
            .unwrap_or_else(|| "NONE".to_string());
        let hist = match c.history {
            HistoryPolicy::MinRetention { days } => format!("{days}d"),
            HistoryPolicy::KeepForever => "keep".to_string(),
        };
        let status = if durability.contains(&c.topic) {
            "<-- IN-PROCESS (durability violation)"
        } else if by_key.contains_key(&key) {
            "OK"
        } else if ALLOW_UNSUBSCRIBED.contains(&c.topic.as_str()) {
            "OK (sanctioned sinkless)"
        } else {
            "<-- UNSUBSCRIBED (seam)"
        };
        let row_topic = format!("{} v{}", c.topic, c.version);
        println!("{row_topic:<24} | {hist:<12} | {subs_str:<48} | {status}");
    }
    println!();

    let mut seam = false;
    for (label, findings) in [
        ("VERSION", &versions),
        ("SINGLE-HOST", &hosts),
        ("PLANELESS", &planeless),
    ] {
        if !findings.is_empty() {
            seam = true;
            eprintln!("topiccheck [{name}]: {label} FAIL:");
            for f in findings {
                eprintln!("  - {f}");
            }
        }
    }
    if !durability.is_empty() {
        seam = true;
        eprintln!(
            "topiccheck [{name}]: DURABILITY FAIL — defined contract topic(s) subscribed \
             in-process (must be durable, seam #3 / constraint 7):"
        );
        for t in &durability {
            eprintln!("  - {t}");
        }
    }
    if !unsub.is_empty() {
        seam = true;
        eprintln!(
            "topiccheck [{name}]: UNSUBSCRIBED (SEAM) — defined contract has no subscriber \
             in this profile and is not in ALLOW_UNSUBSCRIBED:"
        );
        for t in &unsub {
            eprintln!("  - {t}");
        }
    }
    // No advisory-bucket findings exist today (unsubscribed became a seam); the tuple's
    // second slot stays wired so `--strict` keeps gating any advisory added later.
    let advisory = false;
    Ok((seam, advisory))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // No auth env needed: Admin::init no longer reads ADMIN_USER/ADMIN_PASS — session
    // auth is DB-backed (a zero-user boot merely warns), so the harness builds every
    // module graph with a bare environment.

    // A tokio runtime must be live: an in-process `Bus::on` during `init` spawns a task.
    // `--durability-strict`: the BLOCKING fortress invocation — exit non-zero on ANY SEAM
    // violation (version / single-host / planeless / in-process-durability / unsubscribed-
    // outside-`ALLOW_UNSUBSCRIBED`), each of which breaches a hard durable-plane invariant.
    // `--strict`: also block on any advisory finding — none exist today (the bucket is
    // empty since unsubscribed became a seam). Both still print every profile's table.
    // `contract-golden [--bless]`: the VALUE-level contract baseline (Step 6c) — an
    // independent subcommand that never builds the process harness (see `golden.rs`).
    if std::env::args().any(|a| a == "contract-golden") {
        let args = std::env::args().collect::<Vec<_>>();
        if let Some(index) = args.iter().position(|a| a == "--output") {
            let path = args
                .get(index + 1)
                .ok_or_else(|| anyhow::anyhow!("--output requires a path"))?;
            golden::render_to(std::path::Path::new(path))?;
            return Ok(());
        }
        let bless = std::env::args().any(|a| a == "--bless");
        return golden::run(bless);
    }

    let strict = std::env::args().any(|a| a == "--strict");
    let durability_strict = std::env::args().any(|a| a == "--durability-strict");

    let defined = defined_topics();
    println!("topiccheck: defined-vs-subscribed durable event topics\n");

    let mut any_seam = false;
    let mut any_advisory = false;
    for (name, profile) in [
        ("Monolith", DeploymentProfile::Monolith),
        ("Split", DeploymentProfile::Split),
    ] {
        let (seam, advisory) = run_profile(name, &profile, &defined)?;
        any_seam |= seam;
        any_advisory |= advisory;
    }

    if !any_seam && !any_advisory {
        println!(
            "topiccheck: OK — all {} defined topics are subscribed durably, single-hosted, and \
             version-matched in both profiles",
            defined.len()
        );
    }

    if durability_strict && any_seam {
        std::process::exit(1);
    }
    if strict && (any_seam || any_advisory) {
        std::process::exit(1);
    }
    Ok(())
}

#[cfg(test)]
mod tests;
