//! `scheduler` — a data-driven, durable event SOURCE (port of Go's `modules/scheduler`).
//! It owns a catalogue of named schedules (`name` + `interval_seconds`) in schema
//! `scheduler`, and on each 1s tick emits `scheduler.fired{name}` for every schedule
//! whose interval has elapsed. It runs NO job closures — a closure can't cross a process
//! boundary, which would make the scheduler the one module that couldn't be split out.
//! Instead it publishes through the same bus → shared event log seam every domain module
//! uses, so a consumer (e.g. audit's prune) reacts in its OWN process and the scheduler
//! is fully decoupled and independently deployable (see `cmd/scheduler-svc`).
//!
//! Schedules are DATA, not code: the target way to add one is a runtime INSERT into
//! `scheduler.schedules` (via ops/admin), not an edit here. The migration seeds only a
//! minimal bootstrap row (`audit-prune`, 86400s).
//!
//! ## Exactly-once across replicas (the concurrency dance in [`fire`])
//! Every horizontal replica scans the same `scheduler.schedules`, so two could see the
//! same schedule "due" in one window. [`fire`] serializes them per-schedule with a
//! Postgres SESSION-level `pg_try_advisory_lock` keyed by an FNV-1a hash of the name,
//! taken on ONE DEDICATED, per-fire connection (a session lock is held only by the
//! connection that took it, and the tx that relies on it must run on that same session).
//! Under the lock it RE-CHECKS `still_due` (a replica that held the lock just before us
//! may already have fired), then bumps `last_fired` and `emit_tx`s `scheduler.fired`
//! in ONE tx, COMMITs, and only THEN unlocks — so the next winner always observes the
//! moved `last_fired`.
//!
//! ## Dedicated per-fire connections (round-4 remediation; supersedes Go NOTE #10)
//! [`fire`] opens its OWN `PgConnection` from the shared pool's connect options
//! (`PgConnection::connect_with(pool.connect_options())` — the module has no DSN)
//! rather than checking a `PoolConnection` out of the pool; asyncevents' dedicated
//! delivery backends are the precedent. The lock, the re-check, the tx, and the unlock
//! all run on that one session, and the connection is CLOSED when the fire ends —
//! nothing is ever returned to a shared pool, so no session state (advisory lock or
//! `statement_timeout`) can leak into it.
//!
//! The payoff is ABORT-SAFETY. Historically, any timeout/abort that could drop a fire
//! future mid-`await` was FORBIDDEN here: a dropped `PoolConnection` returns to the
//! pool STILL HOLDING its session advisory lock, silently poisoning a pooled session —
//! that schedule then never fires again on any replica. That hazard is specific to
//! POOLED connections and is gone by construction: dropping the fire future drops the
//! dedicated connection, which closes the SOCKET, and Postgres releases the session's
//! advisory lock and rolls back its in-flight tx when it notices the disconnect.
//! Exactly-once still holds under an abort because the `last_fired` bump and the
//! durable emit share the dying session's transaction — they commit together or roll
//! back together, and the released lock lets another replica (or the next tick)
//! re-check and fire. The explicit unlock still runs on the normal paths (deterministic
//! release, commit-before-unlock ordering preserved); the session close is the backstop
//! for unlock failures and aborts.
//!
//! ## Bounding a wedged DB (three layered bounds)
//! - **Session acquisition**: [`due_schedules`]' pool checkout and [`fire`]'s dedicated
//!   connect are wrapped in `tokio::time::timeout(`[`ACQUIRE_DEADLINE`]`, …)` —
//!   dropping a PENDING acquire/connect carries no session state, so cancelling it is
//!   always safe.
//! - **In-flight statements**, at the DB layer: each tick computes ONE aggregate
//!   deadline ([`TICK_DEADLINE`]) and every [`fire`] sets a session-scoped
//!   `statement_timeout` of the REMAINING tick budget — N due schedules share one
//!   budget instead of N×30s. When the budget is exhausted, the remaining due schedules
//!   are SKIPPED for this tick (logged, tick counted as errored; the next tick re-reads
//!   due schedules) rather than attempted with a floored timeout. A wedged statement
//!   ERRORS through the existing error arms — the future is not dropped on this path.
//! - **Shutdown**: [`run_loop`]'s `select!` races only the stop signal against the
//!   ticker, and `tick` re-checks the signal between fires, so a stop is normally
//!   observed at a schedule boundary with every unlock run inline. If the loop is
//!   wedged mid-fire past [`STOP_GRACE`], `stop` ABORTS it — safe per the
//!   dedicated-connection design above — so this module resolves before the app-level
//!   `MODULE_STOP_GRACE_MS` abandons the stop future.
//!
//! Loop health is a [`Liveness`] probe folded into `/readyz` as the `"scheduler"`
//! check: the task dying (panic caught by the supervision wrapper in `start`) flips
//! `dead`, and "no fully-healthy tick in [`TICK_STALL_MAX`]" flags a wedge/error loop
//! that never exits.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use bus::{AnyTx, Bus};
use futures::FutureExt;
use lifecycle::{Context, Module};
use sqlx::{Connection, PgConnection, PgPool};
use tokio::sync::watch;
use tokio::task::JoinHandle;

/// How often the emission loop scans for due schedules. It bounds firing latency (a
/// schedule fires within ~1s of becoming due), not accuracy — `last_fired` is
/// authoritative, so a slow tick never double-fires.
const TICK_INTERVAL: Duration = Duration::from_secs(1);

/// The per-statement DB bound for one tick's work (`statement_timeout`) — a wedged
/// query ERRORS after this instead of stalling every schedule forever. Generous:
/// a healthy tick statement is sub-second, so 30s only ever cancels a genuine wedge
/// (lost lock holder, stuck backend). See the module docs on why this is a DB-layer
/// bound and never a future-dropping `tokio::time::timeout`.
const TICK_DEADLINE: Duration = Duration::from_secs(30);

/// Bound on OBTAINING a DB session — [`due_schedules`]' pool checkout and [`fire`]'s
/// dedicated connect. Dropping a pending acquire/connect carries no session state
/// (no lock, no open tx), so a cancelling `tokio::time::timeout` is safe here, unlike
/// around in-flight fire work (which the session `statement_timeout` bounds instead).
const ACQUIRE_DEADLINE: Duration = Duration::from_secs(5);

/// How long `stop` waits for the emission loop to exit on its own before ABORTING it
/// (mirrors `invalidation::Plane::stop`). Deliberately UNDER `core/app`'s 5s
/// `MODULE_STOP_GRACE_MS` so this module's `stop` resolves before the lifecycle
/// abandons (drops) the stop future and the task would be left detached. Abort safety
/// is documented at [`Scheduler::stop_tasks`] and in the module docs.
const STOP_GRACE: Duration = Duration::from_secs(4);

/// `/readyz` flags the scheduler when no FULLY-healthy tick completed for this long
/// (analogous to `core/app`'s `DELIVERY_STALL_MAX` for the asyncevents workers).
/// 2× [`TICK_DEADLINE`]: one wedged statement errors at 30s and the next tick gets a
/// full deadline window to recover before readiness flips — a stamp older than this
/// means ticks have been failing (or wedging) across two consecutive deadline windows.
const TICK_STALL_MAX: Duration = Duration::from_secs(60);

/// The admin surface ids — shared by the contributed LOCAL `Item` and the
/// `admin.adminData` edge reply so a remote admin renders the same Section/Label.
const ADMIN_ITEM_ID: &str = "scheduler";
const ADMIN_SECTION: &str = "Platform";
const ADMIN_LABEL: &str = "Schedules";

/// Creates this module's OWN schema and seeds the bootstrap row — full logical
/// isolation (#10). Idempotent. Verbatim from Go's `schemaDDL` (with `interval_seconds`
/// widened to `bigint`). `last_fired` defaults to the epoch so a fresh schedule is
/// immediately due on the first tick. Adding a schedule is normally a runtime data
/// INSERT, not a code change; the one seeded row (the audit prune cadence) lets the
/// wired-up system do something out of the box — the producer knowing the consumer's
/// name (`audit-prune`) is coupling-through-a-string, now pushed to a shared contract
/// constant (`schedulerevents::schedule_names::AUDIT_PRUNE`) rather than eliminated:
/// `seeded_schedule_names_are_contract` (`tests.rs`) links this literal to that const.
const SCHEMA_DDL: &str = r#"
CREATE SCHEMA IF NOT EXISTS scheduler;
CREATE TABLE IF NOT EXISTS scheduler.schedules (
	name             text        PRIMARY KEY,
	interval_seconds bigint      NOT NULL CHECK (interval_seconds > 0),
	last_fired       timestamptz NOT NULL DEFAULT to_timestamp(0)
);
INSERT INTO scheduler.schedules (name, interval_seconds)
	VALUES ('audit-prune', 86400)
	ON CONFLICT (name) DO NOTHING;
INSERT INTO scheduler.schedules (name, interval_seconds)
	VALUES ('accounts-sessions-prune', 86400)
	ON CONFLICT (name) DO NOTHING;"#;

/// The due-check SQL for [`due_schedules`], extracted to a const so the anti-drift test
/// (`tests.rs`) can assert `interval_seconds > 0` is present without re-parsing the
/// function body — `CREATE TABLE IF NOT EXISTS` no-ops on an existing table, so a
/// non-positive row can still exist on an un-wiped DB until the CHECK constraint above
/// is actually in place; this filter is the belt to that DDL's braces.
const DUE_SQL: &str = "SELECT name FROM scheduler.schedules \
     WHERE now() - last_fired >= make_interval(secs => interval_seconds) \
     AND interval_seconds > 0";

/// The re-check SQL for [`fire_locked`] (run UNDER the per-schedule advisory lock),
/// extracted to a const for the same anti-drift reason as [`DUE_SQL`]. A row with a
/// non-positive interval simply never re-confirms as due (`fetch_optional` yields
/// `None`/`Some(false)`), the same treatment as a row deleted between the scan and here.
const FIRE_RECHECK_SQL: &str = "SELECT now() - last_fired >= make_interval(secs => interval_seconds) \
     FROM scheduler.schedules WHERE name = $1 AND interval_seconds > 0";

// ============================================================================
// Loop liveness — the `"scheduler"` /readyz probe (mirrors asyncevents' Liveness).
// ============================================================================

/// Coarse monotonic seconds since the first call in this process — the base for the
/// tick-staleness probe. Deliberately not wall-clock (a clock jump must not flap
/// `/readyz`, and tests never depend on the system time). Same shape as
/// `asyncevents::coarse_now_secs`, private to each owner (foundations don't export it).
fn coarse_now_secs() -> u64 {
    static BASE: OnceLock<Instant> = OnceLock::new();
    BASE.get_or_init(Instant::now).elapsed().as_secs()
}

/// Pure staleness predicate behind [`Liveness::check`], split out so the DB-free test
/// can drive it deterministically. `last_ok_secs == 0` means the loop never seeded the
/// clock (disabled, or `start` not reached) — never a stall; a controlled stop is not
/// a stall either.
fn stalled_from(last_ok_secs: u64, now_secs: u64, stopping: bool, max_age: Duration) -> bool {
    !stopping && last_ok_secs != 0 && now_secs.saturating_sub(last_ok_secs) > max_age.as_secs()
}

/// A cloneable emission-loop health probe, folded into `/readyz` as the `"scheduler"`
/// check (contributed in `init`): `dead` is flipped once by the supervision wrapper in
/// `start` if the loop task dies while the module is running (panic in a tick, or a
/// premature exit); the `last_ok_secs` stamp ages past [`TICK_STALL_MAX`] when no
/// FULLY-healthy tick lands — a loop that is alive but erroring/wedging every pass
/// never exits, so `dead` alone would keep `/readyz` green forever.
#[derive(Clone, Default)]
struct Liveness {
    dead: Arc<AtomicBool>,
    stopping: Arc<AtomicBool>,
    /// Coarse-clock second of the last fully-healthy tick; `0` = never seeded.
    last_ok_secs: Arc<AtomicU64>,
}

impl Liveness {
    /// The `/readyz` verdict: `Err` when the loop task died or no fully-healthy tick
    /// landed within `stall_max`.
    fn check(&self, stall_max: Duration) -> Result<(), String> {
        if self.dead.load(Ordering::SeqCst) {
            return Err("scheduler emission loop task died".to_string());
        }
        let last = self.last_ok_secs.load(Ordering::SeqCst);
        let stopping = self.stopping.load(Ordering::SeqCst);
        if stalled_from(last, coarse_now_secs(), stopping, stall_max) {
            return Err(format!(
                "no healthy scheduler tick in >{}s",
                stall_max.as_secs()
            ));
        }
        Ok(())
    }

    /// Stamps "fully-healthy tick completed now". Seeded at loop entry (HTTP serves
    /// before the first tick on a cold boot — age must start at 0, not "infinite").
    /// `max(1)` because `0` is the "never seeded" sentinel.
    fn mark_tick_ok(&self) {
        self.last_ok_secs
            .store(coarse_now_secs().max(1), Ordering::SeqCst);
    }

    /// Marks a controlled shutdown: the supervision wrapper must not read the loop's
    /// stop-signal exit as a death, and a stopping process must not read as stalled.
    fn set_stopping(&self) {
        self.stopping.store(true, Ordering::SeqCst);
    }
}

// ============================================================================
// The firing logic — free functions so the exactly-once test can drive them
// directly against the live DB with a fake bus transport.
// ============================================================================

/// The names whose interval has elapsed. `last_fired` is the authority: a name reported
/// here may still turn out not-due once [`fire`] re-checks under the advisory lock
/// (another replica fired it between this scan and the lock), which is exactly why
/// [`fire`] double-checks. The scan runs inside its own tx so `SET LOCAL
/// statement_timeout` bounds a wedged scan and reverts automatically at tx end —
/// nothing leaks to the shared pool.
async fn due_schedules(pool: &PgPool, deadline: Duration) -> anyhow::Result<Vec<String>> {
    // Bounded checkout: a starved/wedged pool must not stall the tick loop forever.
    // Dropping the pending `begin` on elapse carries no session state — safe to cancel.
    let mut tx = tokio::time::timeout(ACQUIRE_DEADLINE, pool.begin())
        .await
        .map_err(|_| {
            anyhow::anyhow!(
                "scheduler: due-scan pool checkout timed out after {}s",
                ACQUIRE_DEADLINE.as_secs()
            )
        })??;
    // `SET` takes no bind parameters; the interpolated value is a plain integer (ms).
    sqlx::query(&format!(
        "SET LOCAL statement_timeout = {}",
        deadline.as_millis().max(1)
    ))
    .execute(&mut *tx)
    .await?;
    let rows: Vec<(String,)> = sqlx::query_as(DUE_SQL).fetch_all(&mut *tx).await?;
    tx.commit().await?; // read-only tx; commit ends the SET LOCAL scope
    Ok(rows.into_iter().map(|(n,)| n).collect())
}

/// Finds every due schedule and tries to fire each within ONE aggregate budget: the
/// caller computes `tick_deadline` once per tick and every [`fire`] receives only the
/// REMAINING budget for its session `statement_timeout` — a tick of N schedules is
/// bounded by `budget`, not N×`budget`. Once the budget is exhausted the remaining due
/// schedules are SKIPPED for this tick (logged, counted as failures) instead of
/// attempted with a floored timeout — no point burning a connect + advisory lock on a
/// guaranteed statement-timeout error; the next tick re-reads due schedules. The stop
/// signal is also honored BETWEEN fires, so an in-progress tick yields at the next
/// schedule boundary (a controlled stop, not a failure). A per-schedule failure is
/// LOGGED and does NOT abort the others (Go's `tick`) — but any failure/skip makes the
/// whole tick report `Err`, so [`run_loop`] withholds the [`Liveness`] stamp and a
/// persistently failing/wedging schedule surfaces on `/readyz` instead of staying
/// silently broken. `tick_deadline` is a parameter (not computed here) so the
/// budget-exhaustion path is directly testable.
async fn tick(
    pool: &PgPool,
    bus: &Bus,
    budget: Duration,
    tick_deadline: Instant,
    stop: &watch::Receiver<bool>,
) -> anyhow::Result<()> {
    let mut failed = 0usize;
    for name in due_schedules(pool, budget).await? {
        if *stop.borrow() {
            tracing::info!(
                schedule = %name,
                "scheduler stopping; yielding the tick at this schedule boundary"
            );
            break;
        }
        let remaining = tick_deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            tracing::error!(
                schedule = %name,
                budget_secs = budget.as_secs(),
                "scheduler tick budget exhausted; skipping this due schedule until the next tick"
            );
            failed += 1;
            continue;
        }
        if let Err(e) = fire(pool, bus, &name, remaining).await {
            tracing::error!(schedule = %name, error = %e, "scheduler fire failed");
            failed += 1;
        }
    }
    if failed > 0 {
        anyhow::bail!("{failed} schedule fire(s) failed this tick");
    }
    Ok(())
}

/// Emits `scheduler.fired` for one due schedule EXACTLY ONCE across horizontal replicas.
/// See the module-level docs for the full connection/lock/cancellation rationale.
///
/// Runs on a DEDICATED, per-fire connection (opened from the shared pool's connect
/// options, bounded by [`ACQUIRE_DEADLINE`]) — never a pooled one. If this future is
/// dropped mid-flight (the loop abort in [`Scheduler::stop_tasks`]), the connection
/// drops with it, the socket closes, and Postgres releases the session advisory lock
/// and rolls back the in-flight tx server-side — nothing is ever returned to a shared
/// pool, so no session state can leak. Every statement runs under a session
/// `statement_timeout` of `deadline` (the tick's REMAINING budget), so a wedged
/// lock/re-check/update/emit ERRORS back through the caller's error arm.
async fn fire(pool: &PgPool, bus: &Bus, name: &str, deadline: Duration) -> anyhow::Result<()> {
    let key = lock_key(name);

    // The module has no DSN of its own — the ctx-provided pool's connect options are
    // the sanctioned source (asyncevents' dedicated delivery backends are the precedent).
    let mut conn = tokio::time::timeout(
        ACQUIRE_DEADLINE,
        PgConnection::connect_with(&*pool.connect_options()),
    )
    .await
    .map_err(|_| {
        anyhow::anyhow!(
            "scheduler: dedicated fire connection timed out after {}s",
            ACQUIRE_DEADLINE.as_secs()
        )
    })??;

    let result = fire_on(&mut conn, bus, name, key, deadline).await;

    // Graceful close on every non-abort path: ends the session immediately, which also
    // releases the advisory lock should the explicit unlock in `fire_on` have failed.
    // On the abort path this line never runs — the dropped connection closes the socket
    // and the server releases the lock once it notices the disconnect.
    let _ = conn.close().await;
    result
}

/// The per-session body of [`fire`]: set the statement bound, take the per-schedule
/// advisory lock, run the guarded work capturing its `Result`, then ALWAYS attempt the
/// unlock on the same session before returning (Go's `defer unlock` — deterministic
/// release on the common path, preserving commit-before-unlock ordering). An unlock
/// failure is only logged: [`fire`] closes the session either way, which releases the
/// lock server-side.
async fn fire_on(
    conn: &mut PgConnection,
    bus: &Bus,
    name: &str,
    key: i64,
    deadline: Duration,
) -> anyhow::Result<()> {
    sqlx::query(&format!(
        "SET statement_timeout = {}",
        deadline.as_millis().max(1)
    ))
    .execute(&mut *conn)
    .await?;

    let locked: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
        .bind(key)
        .fetch_one(&mut *conn)
        .await?;
    if !locked {
        // Another replica holds this key (or a colliding one) and is firing now.
        return Ok(());
    }

    let result = fire_locked(conn, bus, name).await;

    if let Err(e) = sqlx::query("SELECT pg_advisory_unlock($1)")
        .bind(key)
        .execute(&mut *conn)
        .await
    {
        tracing::warn!(
            schedule = name, error = %e,
            "scheduler advisory unlock failed; the session close in fire() releases the lock"
        );
    }
    result
}

/// The lock-held critical section: re-check `still_due`, then bump `last_fired` +
/// `emit_tx` the durable event in ONE tx and COMMIT. Split out so [`fire`] can capture
/// this Result and still guarantee the unlock.
async fn fire_locked(conn: &mut PgConnection, bus: &Bus, name: &str) -> anyhow::Result<()> {
    // Re-check UNDER the lock: a replica that held the lock just before us may already
    // have fired this schedule and moved `last_fired`. `fetch_optional` returns None when
    // the row vanished (deleted between the due-scan and here) — treat as not-due.
    let still_due: Option<bool> = sqlx::query_scalar(FIRE_RECHECK_SQL)
        .bind(name)
        .fetch_optional(&mut *conn)
        .await?;
    let Some(true) = still_due else {
        return Ok(()); // not due, or deleted between the scan and here
    };

    // `last_fired` bump + the durable event append commit together, on the LOCKED connection.
    // (Commit happens here, before the unlock back in `fire`.)
    let mut tx = conn.begin().await?;
    sqlx::query("UPDATE scheduler.schedules SET last_fired = now() WHERE name = $1")
        .bind(name)
        .execute(&mut *tx)
        .await?;
    bus.emit_tx(
        AnyTx::new(&mut *tx), // erased after the deref: Transaction<'_> isn't 'static
        &schedulerevents::FIRED,
        &schedulerevents::Fired {
            name: name.to_string(),
        },
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Derives a stable 64-bit advisory-lock key from a schedule name via FNV-1a, then
/// reinterprets it as `i64` (Go's `int64(h.Sum64())` wrap — pg advisory keys use the
/// full signed bigint range). Two different names CAN hash to the same key: they then
/// share one lock, which merely serializes their firing — it never breaks exactly-once,
/// because the re-check under the lock is per-name against that name's own `last_fired`.
fn lock_key(name: &str) -> i64 {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut h = OFFSET_BASIS;
    for b in name.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(PRIME);
    }
    h as i64
}

/// Reads `key` as a bool, returning `def` when unset or unparseable (Go's `envBool`).
/// Accepts the same spellings Go's `strconv.ParseBool` does.
fn env_bool(key: &str, def: bool) -> bool {
    match std::env::var(key) {
        Ok(v) if v.is_empty() => def,
        Ok(v) => match v.to_ascii_lowercase().as_str() {
            "1" | "t" | "true" => true,
            "0" | "f" | "false" => false,
            _ => def,
        },
        Err(_) => def,
    }
}

// ============================================================================
// Service — backs the read-only "Schedules" admin view (local render + edge fan-out).
// ============================================================================

/// Holds the pool for the read-only admin view. Constructed in phase-1 `register`.
pub struct Service {
    pool: PgPool,
}

impl Service {
    /// The schedule catalogue as admin widgets (Go's `adminRender`): a read-only table
    /// of Schedule / Interval (s) / Last fired.
    async fn admin_content(&self) -> anyhow::Result<adminapi::Content> {
        let rows: Vec<(String, i64, String)> = sqlx::query_as(
            "SELECT name, interval_seconds, last_fired::text \
             FROM scheduler.schedules ORDER BY name",
        )
        .fetch_all(&self.pool)
        .await?;

        let mut table = adminapi::Table {
            columns: vec!["Schedule".into(), "Interval (s)".into(), "Last fired".into()],
            rows: Vec::with_capacity(rows.len()),
        };
        for (name, interval, last_fired) in rows {
            table.rows.push(vec![
                adminapi::Cell::mono(&name),
                adminapi::Cell::text(interval.to_string()),
                adminapi::Cell::text(&last_fired),
            ]);
        }
        Ok(adminapi::Content {
            kpis: Vec::new(),
            table: Some(table),
            form: None,
        })
    }
}

#[async_trait]
impl adminapi::AdminData for Service {
    /// The admin fan-out: this module's "Schedules" page as [`adminapi::ItemData`] (same
    /// Section/Label the local `Item` carries), served on the edge as `admin.adminData`
    /// so a remote admin process renders it cross-process.
    async fn admin_data(&self) -> Result<adminapi::ItemData, opsapi::Error> {
        let content = self
            .admin_content()
            .await
            .map_err(|e| opsapi::Error::internal(e.to_string()))?;
        Ok(adminapi::ItemData {
            id: ADMIN_ITEM_ID.into(),
            section: ADMIN_SECTION.into(),
            label: ADMIN_LABEL.into(),
            content,
        })
    }
}

// ============================================================================
// Module — the lifecycle wiring.
// ============================================================================

/// The scheduler module. Holds the pool-backed service (admin render + edge face), the
/// pool + bus the emission loop needs, the enable gate, and the loop's cancel/join
/// handles.
pub struct Scheduler {
    svc: OnceLock<Arc<Service>>,
    pool: OnceLock<PgPool>,
    bus: OnceLock<Arc<Bus>>,
    enabled: OnceLock<bool>,
    /// Loop health for the `"scheduler"` `/readyz` check — cloned into both the
    /// `init`-time [`httpmw::ReadyCheck`] and the `start`-time supervision wrapper.
    liveness: Liveness,
    stop_tx: Mutex<Option<watch::Sender<bool>>>,
    tasks: Mutex<Vec<JoinHandle<()>>>,
}

impl Default for Scheduler {
    fn default() -> Self {
        Scheduler::new()
    }
}

impl Scheduler {
    pub fn new() -> Scheduler {
        Scheduler {
            svc: OnceLock::new(),
            pool: OnceLock::new(),
            bus: OnceLock::new(),
            enabled: OnceLock::new(),
            liveness: Liveness::default(),
            stop_tx: Mutex::new(None),
            tasks: Mutex::new(Vec::new()),
        }
    }

    fn svc(&self) -> Arc<Service> {
        self.svc
            .get()
            .expect("scheduler.register must run before init")
            .clone()
    }

    /// Signals the loop and awaits its exit, bounded by [`STOP_GRACE`] per task
    /// (mirrors `invalidation::Plane::stop`). On the graceful path the loop observes
    /// the signal at the next schedule boundary (the tick loop re-checks it between
    /// fires) and exits with every advisory unlock run inline. A loop still wedged
    /// mid-fire past the grace is ABORTED — SAFE because every `fire` runs on a
    /// dedicated, per-fire connection: dropping the fire future drops that connection,
    /// the socket closes, and Postgres releases the session advisory lock and rolls
    /// back the in-flight tx (the `last_fired` bump and the durable emit share that tx,
    /// so exactly-once holds). Extracted from `Module::stop` (which only adds the
    /// `set_stopping` bookkeeping) so the stuck-fire shutdown path is testable without
    /// a `lifecycle::Context`.
    async fn stop_tasks(&self) {
        if let Some(tx) = self.stop_tx.lock().unwrap().take() {
            let _ = tx.send(true);
        }
        let tasks = std::mem::take(&mut *self.tasks.lock().unwrap());
        for mut t in tasks {
            match tokio::time::timeout(STOP_GRACE, &mut t).await {
                Ok(Ok(())) => {}
                // The supervision wrapper catches tick panics, so a JoinError here
                // means the wrapper itself died — never swallow it silently.
                Ok(Err(e)) => {
                    tracing::error!(error = %e, "scheduler emission loop task terminated abnormally");
                }
                Err(_) => {
                    tracing::error!(
                        grace_secs = STOP_GRACE.as_secs(),
                        "scheduler emission loop did not exit within the stop grace; aborting it \
                         (safe: fires run on dedicated connections — the advisory lock dies with the session)"
                    );
                    t.abort();
                }
            }
        }
    }
}

#[async_trait]
impl Module for Scheduler {
    fn name(&self) -> &str {
        "scheduler"
    }

    /// Phase 1, BEFORE any `init`: builds the pool-backed service (needed by the admin
    /// face + render) and captures the pool for the emission loop. No subscriptions.
    fn register(&self, ctx: &Context) -> anyhow::Result<()> {
        let pool = ctx
            .db()
            .ok_or_else(|| anyhow::anyhow!("scheduler requires a DB pool"))?
            .clone();
        self.svc
            .set(Arc::new(Service { pool: pool.clone() }))
            .map_err(|_| anyhow::anyhow!("scheduler.register ran twice"))?;
        self.pool
            .set(pool)
            .map_err(|_| anyhow::anyhow!("scheduler.register ran twice"))?;
        Ok(())
    }

    /// Creates this module's own schema and seeds the bootstrap row. Idempotent.
    async fn migrate(&self, ctx: &Context) -> anyhow::Result<()> {
        let pool = ctx
            .db()
            .ok_or_else(|| anyhow::anyhow!("scheduler requires a DB pool"))?;
        sqlx::raw_sql(SCHEMA_DDL).execute(pool).await?;
        Ok(())
    }

    /// Only wires up — no DB I/O (#8). Captures the bus, reads the enable gate, and
    /// contributes the read-only "Schedules" admin item + its `admin.adminData` edge
    /// face (topology-blind; applied by `app::run` iff this process serves an edge).
    fn init(&self, ctx: &Context) -> anyhow::Result<()> {
        self.bus
            .set(ctx.bus().clone())
            .map_err(|_| anyhow::anyhow!("scheduler.init ran twice"))?;

        let enabled = env_bool("SCHEDULER_ENABLED", true);
        let _ = self.enabled.set(enabled);
        if !enabled {
            tracing::warn!("scheduler DISABLED (SCHEDULER_ENABLED=false) — no schedules will fire");
        }

        // Loop liveness → `/readyz`, contributed only when the loop will actually run
        // (a deliberately disabled scheduler must not surface a readiness check). The
        // probe is dark until `start` seeds the stamp — an unstarted loop reads ready.
        if enabled {
            let liveness = self.liveness.clone();
            ctx.contribute(
                httpmw::READINESS_SLOT,
                httpmw::ReadyCheck::new("scheduler", move || {
                    let liveness = liveness.clone();
                    async move { liveness.check(TICK_STALL_MAX) }
                }),
            );
        }

        // The local admin page. RenderFn is synchronous, but the store read is async; the
        // closure bridges via block_in_place (requires the multi-thread runtime the app
        // boots on) — the same pattern audit/characters/inventory use.
        let render_svc = self.svc();
        ctx.contribute(
            adminapi::SLOT,
            adminapi::Item::local(
                ADMIN_ITEM_ID,
                ADMIN_SECTION,
                ADMIN_LABEL,
                Arc::new(move |_params: &adminapi::Params| {
                    let svc = render_svc.clone();
                    tokio::task::block_in_place(|| {
                        tokio::runtime::Handle::current().block_on(svc.admin_content())
                    })
                }),
            ),
        );

        // Edge exposure, contributed UNCONDITIONALLY — topology-blind (Step 3 seam):
        // `app::run` applies this iff the entrypoint stood up an internal edge server
        // (then a remote admin pulls the "Schedules" page over QUIC); in the monolith it
        // is never applied. Registered through scheduler's OWN glue crate's re-export so
        // no foreign rpc is imported.
        let admin_svc = self.svc();
        ctx.contribute(
            edge::EDGE_SLOT,
            edge::EdgeReg::new(move |server| {
                schedulerrpc::register_admin(server, admin_svc.clone());
            }),
        );
        Ok(())
    }

    /// Launches (unless disabled) the emission loop on a FRESH `tokio::spawn` task (not
    /// tied to the `start` ctx), so a short start deadline can't kill the loop; `stop`
    /// cancels it. The loop's cancellation-safe structure is documented at [`run_loop`].
    async fn start(&self, _ctx: &Context) -> anyhow::Result<()> {
        if !*self.enabled.get().unwrap_or(&true) {
            return Ok(());
        }
        let pool = self
            .pool
            .get()
            .expect("scheduler.register must run before start")
            .clone();
        let bus = self
            .bus
            .get()
            .expect("scheduler.init must run before start")
            .clone();

        let (stop_tx, stop_rx) = watch::channel(false);
        // Supervision wrapper (mirrors the asyncevents worker wrapper): a panic inside
        // a tick, or the loop exiting while the module is running, flips `Liveness::
        // dead` so `/readyz` goes red instead of the loop dying silently.
        let liveness = self.liveness.clone();
        let cfg = LoopCfg {
            tick_interval: TICK_INTERVAL,
            tick_deadline: TICK_DEADLINE,
        };
        let task = tokio::spawn(async move {
            let result =
                std::panic::AssertUnwindSafe(run_loop(pool, bus, liveness.clone(), cfg, stop_rx))
                    .catch_unwind()
                    .await;
            if !liveness.stopping.load(Ordering::SeqCst) {
                if result.is_err() {
                    tracing::error!("scheduler emission loop panicked while the module was running");
                } else {
                    tracing::error!("scheduler emission loop exited while the module was running");
                }
                liveness.dead.store(true, Ordering::SeqCst);
            }
        });
        *self.stop_tx.lock().unwrap() = Some(stop_tx);
        self.tasks.lock().unwrap().push(task);
        Ok(())
    }

    /// Signals the loop and awaits its exit, bounded by [`STOP_GRACE`] with an abort
    /// fallback — see [`Scheduler::stop_tasks`] for the full choreography and why the
    /// abort cannot strand an advisory lock.
    async fn stop(&self, _ctx: &Context) -> anyhow::Result<()> {
        // Before signaling, so the supervision wrapper reads a controlled exit and the
        // readiness probe never counts a stopping process as stalled.
        self.liveness.set_stopping();
        self.stop_tasks().await;
        Ok(())
    }
}

/// The loop's timing knobs, threaded as data so the live-DB hang test can run the REAL
/// [`run_loop`] with millisecond intervals; production (`start`) passes
/// [`TICK_INTERVAL`]/[`TICK_DEADLINE`].
struct LoopCfg {
    tick_interval: Duration,
    tick_deadline: Duration,
}

/// Scans for due schedules every `cfg.tick_interval` until `stop` flips. Cancellation
/// behavior: the `select!` races ONLY the stop signal against the ticker; the actual
/// `tick` (hence every `fire`, with its advisory unlock) runs OUTSIDE the `select!`,
/// and `tick` re-checks the signal between fires — so on the graceful path a stop is
/// observed at a schedule boundary and every unlock runs inline. `stop` awaits this
/// task bounded by [`STOP_GRACE`] and ABORTS it on elapse; the abort may drop a `fire`
/// future mid-`await`, which is safe with dedicated per-fire connections (module docs).
/// Each tick gets ONE aggregate deadline (`Instant::now() + cfg.tick_deadline`) that
/// bounds all its fires at the DB layer; a wedged statement lands in the error arm
/// below, and only a FULLY-healthy tick refreshes the [`Liveness`] stamp.
async fn run_loop(
    pool: PgPool,
    bus: Arc<Bus>,
    liveness: Liveness,
    cfg: LoopCfg,
    mut stop: watch::Receiver<bool>,
) {
    // Seed the staleness clock: HTTP serves before the first tick on a cold boot —
    // the stamp's age must start at 0, not read as an infinite stall.
    liveness.mark_tick_ok();
    let mut ticker = tokio::time::interval(cfg.tick_interval);
    loop {
        tokio::select! {
            _ = stop.changed() => break,
            _ = ticker.tick() => {}
        }
        if *stop.borrow() {
            break;
        }
        let tick_deadline = Instant::now() + cfg.tick_deadline;
        match tick(&pool, &bus, cfg.tick_deadline, tick_deadline, &stop).await {
            Ok(()) => liveness.mark_tick_ok(),
            Err(e) => tracing::error!(error = %e, "scheduler tick failed"),
        }
    }
}

// ============================================================================
// Tests. The exactly-once concurrency test drives `fire` directly against the live
// local Postgres (advisory lock + stillDue re-check) with a fake bus transport that
// records the durable emits — the producer side, without the messaging internals.
// Live-Postgres tests SKIP cleanly (early return) when the DB is unreachable.
// ============================================================================
#[cfg(test)]
mod tests;
