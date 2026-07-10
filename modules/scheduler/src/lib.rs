//! `scheduler` — a data-driven, durable event SOURCE (port of Go's `modules/scheduler`).
//! It owns a catalogue of named schedules (`name` + `interval_seconds`) in schema
//! `scheduler`, and on each 1s tick emits `scheduler.fired{name}` for every schedule
//! whose interval has elapsed. It runs NO job closures — a closure can't cross a process
//! boundary, which would make the scheduler the one module that couldn't be split out.
//! Instead it publishes through the same bus → outbox → sink seam every domain module
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
//! taken on ONE DEDICATED pooled connection (a session lock is held only by the
//! connection that took it, and the tx that relies on it must run on that same session).
//! Under the lock it RE-CHECKS `still_due` (a replica that held the lock just before us
//! may already have fired), then bumps `last_fired` and `emit_tx`s `scheduler.fired`
//! in ONE tx, COMMITs, and only THEN unlocks — so the next winner always observes the
//! moved `last_fired`.
//!
//! ## sqlx connection / lock / cancellation mechanics (Go NOTE #10)
//! - The advisory lock, the re-check, the tx, and the unlock ALL run on the SAME
//!   `PoolConnection` (`pool.acquire()`), because the lock is connection-scoped.
//! - The tx is opened with `Connection::begin(&mut conn)` (borrows the connection), so
//!   after `tx.commit()` the borrow ends and the SAME connection performs the unlock.
//! - The unlock MUST run even on an error path. A dropped `PoolConnection` returns to
//!   the pool WITHOUT dropping its session advisory locks, so [`fire`] captures the
//!   guarded work's `Result`, ALWAYS runs `pg_advisory_unlock` on the connection, then
//!   propagates — the Rust analogue of Go's `defer pg_advisory_unlock`. On an unlock
//!   FAILURE it `detach()`+`close()`s the physical connection so PG releases the lock
//!   server-side rather than stranding it on a pooled connection.
//! - Cancellation safety: [`run_loop`] runs `tick` (hence every `fire`) OUTSIDE the
//!   stop-vs-tick `select!`, so a shutdown signal is only ever observed BETWEEN fires,
//!   never mid-fire. `stop` sends the signal and AWAITS the loop task, so the in-flight
//!   tick (with its unlock) always completes — no `fire` future is ever dropped
//!   mid-`await`. This is why no `Drop`-guard/`tokio::spawn` unlock is needed: the loop
//!   structure guarantees the inline unlock always executes.

use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use bus::{AnyTx, Bus};
use lifecycle::{Context, Module};
use sqlx::{Connection, PgConnection, PgPool};
use tokio::sync::watch;
use tokio::task::JoinHandle;

/// How often the emission loop scans for due schedules. It bounds firing latency (a
/// schedule fires within ~1s of becoming due), not accuracy — `last_fired` is
/// authoritative, so a slow tick never double-fires.
const TICK_INTERVAL: Duration = Duration::from_secs(1);

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
	interval_seconds bigint      NOT NULL,
	last_fired       timestamptz NOT NULL DEFAULT to_timestamp(0)
);
INSERT INTO scheduler.schedules (name, interval_seconds)
	VALUES ('audit-prune', 86400)
	ON CONFLICT (name) DO NOTHING;
INSERT INTO scheduler.schedules (name, interval_seconds)
	VALUES ('accounts-sessions-prune', 86400)
	ON CONFLICT (name) DO NOTHING;"#;

// ============================================================================
// The firing logic — free functions so the exactly-once test can drive them
// directly against the live DB with a fake bus transport.
// ============================================================================

/// The names whose interval has elapsed. `last_fired` is the authority: a name reported
/// here may still turn out not-due once [`fire`] re-checks under the advisory lock
/// (another replica fired it between this scan and the lock), which is exactly why
/// [`fire`] double-checks.
async fn due_schedules(pool: &PgPool) -> anyhow::Result<Vec<String>> {
    let rows: Vec<(String,)> = sqlx::query_as(
        "SELECT name FROM scheduler.schedules \
         WHERE now() - last_fired >= make_interval(secs => interval_seconds)",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(|(n,)| n).collect())
}

/// Finds every due schedule and tries to fire each. A per-schedule failure is LOGGED and
/// does NOT abort the others (Go's `tick`).
async fn tick(pool: &PgPool, bus: &Bus) -> anyhow::Result<()> {
    for name in due_schedules(pool).await? {
        if let Err(e) = fire(pool, bus, &name).await {
            tracing::error!(schedule = %name, error = %e, "scheduler fire failed");
        }
    }
    Ok(())
}

/// Emits `scheduler.fired` for one due schedule EXACTLY ONCE across horizontal replicas.
/// See the module-level docs for the full connection/lock/cancellation rationale.
async fn fire(pool: &PgPool, bus: &Bus, name: &str) -> anyhow::Result<()> {
    let key = lock_key(name);

    // A DEDICATED pooled connection: the session-level advisory lock is held ONLY by the
    // connection that took it, and the tx that relies on the lock must share that session
    // — so every step below runs on `conn`.
    let mut conn = pool.acquire().await?;

    let locked: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
        .bind(key)
        .fetch_one(&mut *conn)
        .await?;
    if !locked {
        // Another replica holds this key (or a colliding one) and is firing now.
        return Ok(());
    }

    // The lock is now HELD on `conn`. Run the guarded work capturing its Result, then
    // ALWAYS unlock on the same connection before returning (Go's `defer unlock`). On an
    // unlock FAILURE, close the physical connection so PG releases the session lock
    // rather than the connection returning to the pool still holding it.
    let result = fire_locked(&mut conn, bus, name).await;

    if let Err(e) = sqlx::query("SELECT pg_advisory_unlock($1)")
        .bind(key)
        .execute(&mut *conn)
        .await
    {
        tracing::error!(
            schedule = name, error = %e,
            "scheduler advisory unlock failed; closing the connection so the lock is not stranded in the pool"
        );
        let _ = conn.detach().close().await;
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
    let still_due: Option<bool> = sqlx::query_scalar(
        "SELECT now() - last_fired >= make_interval(secs => interval_seconds) \
         FROM scheduler.schedules WHERE name = $1",
    )
    .bind(name)
    .fetch_optional(&mut *conn)
    .await?;
    let Some(true) = still_due else {
        return Ok(()); // not due, or deleted between the scan and here
    };

    // `last_fired` bump + durable outbox write commit together, on the LOCKED connection.
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
        let task = tokio::spawn(run_loop(pool, bus, stop_rx));
        *self.stop_tx.lock().unwrap() = Some(stop_tx);
        self.tasks.lock().unwrap().push(task);
        Ok(())
    }

    /// Signals the loop and AWAITS its exit (bounded by the caller). Because the loop
    /// only observes the stop signal BETWEEN fires, awaiting it here lets any in-flight
    /// tick finish — including its advisory unlock (Go NOTE #10 parity).
    async fn stop(&self, _ctx: &Context) -> anyhow::Result<()> {
        if let Some(tx) = self.stop_tx.lock().unwrap().take() {
            let _ = tx.send(true);
        }
        let tasks = std::mem::take(&mut *self.tasks.lock().unwrap());
        for t in tasks {
            let _ = t.await;
        }
        Ok(())
    }
}

/// Scans for due schedules every [`TICK_INTERVAL`] until `stop` flips. Cancellation
/// safety (Go NOTE #10): the `select!` races ONLY the stop signal against the ticker;
/// the actual `tick` (hence every `fire`, with its advisory unlock) runs OUTSIDE the
/// `select!`, so a stop is observed only BETWEEN fires — a `fire` future is never dropped
/// mid-`await`, and its unlock always runs. `stop` awaits this task, so the in-flight
/// tick completes before shutdown proceeds.
async fn run_loop(pool: PgPool, bus: Arc<Bus>, mut stop: watch::Receiver<bool>) {
    let mut ticker = tokio::time::interval(TICK_INTERVAL);
    loop {
        tokio::select! {
            _ = stop.changed() => break,
            _ = ticker.tick() => {}
        }
        if *stop.borrow() {
            break;
        }
        if let Err(e) = tick(&pool, &bus).await {
            tracing::error!(error = %e, "scheduler tick failed");
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
