//! scheduler tests. The exactly-once concurrency test stands up TWO independent
//! replicas (two pools, two buses, two fake transports) against the same DB and drives
//! `fire` concurrently — the advisory lock + `still_due` re-check must yield exactly ONE
//! durable emit and one `last_fired` bump. A second test proves a schedule re-arms after
//! its interval. Both target the live local Postgres (the test DB) and SKIP cleanly
//! (early return + printed message) when it is unreachable, so `cargo test` never
//! hard-fails on a machine without it. In-crate so they can drive the private `fire` /
//! `due_schedules` / `lock_key`.

use std::sync::{Arc, Mutex};

use bus::{AnyTx, Bus, Error as BusError, Transport, TxHandler};

use super::*;

const DEFAULT_DSN: &str =
    "postgres://gamebackend:gamebackend@localhost:5432/gamebackend?sslmode=disable";

fn dsn() -> String {
    std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DSN.to_string())
}

/// Connects to the test DB and ensures the schema; `None` (with a printed SKIP) when
/// Postgres is unreachable, so the live tests early-return instead of failing.
async fn test_pool() -> Option<PgPool> {
    match PgPool::connect(&dsn()).await {
        Ok(pool) => {
            sqlx::raw_sql(SCHEMA_DDL)
                .execute(&pool)
                .await
                .expect("migrate scheduler schema");
            Some(pool)
        }
        Err(e) => {
            eprintln!("SKIP scheduler live test: postgres unreachable: {e}");
            None
        }
    }
}

/// A run-unique schedule name so assertions/cleanup never collide on the shared DB.
async fn unique_name(pool: &PgPool) -> String {
    let (s,): (String,) = sqlx::query_as("SELECT 'test-' || gen_random_uuid()::text")
        .fetch_one(pool)
        .await
        .unwrap();
    s
}

/// Inserts (or resets) a schedule with `last_fired` at the epoch, so it is immediately
/// due (Go's `seedSchedule`).
async fn seed_schedule(pool: &PgPool, name: &str, interval_seconds: i64) {
    sqlx::query(
        "INSERT INTO scheduler.schedules (name, interval_seconds, last_fired) \
         VALUES ($1, $2, to_timestamp(0)) \
         ON CONFLICT (name) DO UPDATE SET interval_seconds = $2, last_fired = to_timestamp(0)",
    )
    .bind(name)
    .bind(interval_seconds)
    .execute(pool)
    .await
    .expect("seed schedule");
}

async fn cleanup(pool: &PgPool, name: &str) {
    let _ = sqlx::query("DELETE FROM scheduler.schedules WHERE name = $1")
        .bind(name)
        .execute(pool)
        .await;
}

/// A minimal in-memory `bus::Transport` standing in for the asyncevents plane: it only
/// RECORDS enqueued durable emits (Go's `fakeTransport`), so `fire`'s `emit_tx` has a
/// transport to write into without a live durable plane (which would need a DB these
/// tests shouldn't need). Durable delivery is the asyncevents plane's own concern.
struct FakeTransport {
    rows: Mutex<Vec<(String, Vec<u8>)>>,
}

impl FakeTransport {
    fn new() -> Arc<FakeTransport> {
        Arc::new(FakeTransport {
            rows: Mutex::new(Vec::new()),
        })
    }

    /// How many enqueued rows carry the given schedule name — the fake-transport-backed
    /// stand-in for the old `outboxCount(db, name)` query.
    fn count(&self, name: &str) -> usize {
        self.rows
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, payload)| {
                serde_json::from_slice::<schedulerevents::Fired>(payload)
                    .map(|f| f.name == name)
                    .unwrap_or(false)
            })
            .count()
    }
}

#[async_trait::async_trait]
impl Transport for FakeTransport {
    async fn enqueue_tx(
        &self,
        _tx: AnyTx<'_>,
        contract: &bus::EventContract,
        payload: &[u8],
    ) -> Result<(), BusError> {
        self.rows
            .lock()
            .unwrap()
            .push((contract.topic.to_string(), payload.to_vec()));
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

/// A bus with a fake transport installed, plus a handle to that transport for assertions.
fn bus_with_fake() -> (Arc<Bus>, Arc<FakeTransport>) {
    let ft = FakeTransport::new();
    let bus = Bus::with_transport(ft.clone() as Arc<dyn Transport>);
    (Arc::new(bus), ft)
}

// --- no DB ------------------------------------------------------------------

/// `lock_key` is stable per name (so two replicas derive the SAME advisory key and
/// contend) and the FNV-1a wrap matches Go's `int64(fnv64a(name))` for a known input.
#[test]
fn lock_key_is_stable_and_fnv1a() {
    assert_eq!(lock_key("audit-prune"), lock_key("audit-prune"));
    assert_ne!(lock_key("a"), lock_key("b"));
    // FNV-1a 64-bit of "audit-prune" as u64, reinterpreted as i64.
    let expected = {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for b in "audit-prune".bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        h as i64
    };
    assert_eq!(lock_key("audit-prune"), expected);
}

/// Links the seed DDL's literal `'audit-prune'` tuple to the shared contract const —
/// a rename on either side (the seed literal or `schedulerevents::schedule_names::
/// AUDIT_PRUNE`) fails this build/test instead of drifting into a silent no-op prune.
#[test]
fn seeded_schedule_names_are_contract() {
    assert!(
        SCHEMA_DDL.contains(&format!(
            "('{}',",
            schedulerevents::schedule_names::AUDIT_PRUNE
        )),
        "seed DDL no longer contains the schedule row for {}",
        schedulerevents::schedule_names::AUDIT_PRUNE
    );
}

// --- live Postgres ----------------------------------------------------------

/// Two concurrent `fire` attempts (two replicas: two pools, two buses) against one due
/// schedule must yield EXACTLY ONE durable emit and one `last_fired` bump — the advisory
/// lock + `still_due` re-check at work (Go's `TestFireExactlyOnceUnderConcurrency`).
#[tokio::test(flavor = "multi_thread")]
async fn fire_exactly_once_under_concurrency() {
    let Some(pool1) = test_pool().await else {
        return;
    };
    let Ok(pool2) = PgPool::connect(&dsn()).await else {
        return;
    };

    let name = unique_name(&pool1).await;
    seed_schedule(&pool1, &name, 3600).await; // due (epoch), won't re-arm within the test

    let (bus1, ft1) = bus_with_fake();
    let (bus2, ft2) = bus_with_fake();

    let (p1, b1, n1) = (pool1.clone(), bus1.clone(), name.clone());
    let (p2, b2, n2) = (pool2.clone(), bus2.clone(), name.clone());
    let h1 = tokio::spawn(async move { fire(&p1, &b1, &n1).await });
    let h2 = tokio::spawn(async move { fire(&p2, &b2, &n2).await });
    h1.await.unwrap().expect("replica 1 fire");
    h2.await.unwrap().expect("replica 2 fire");

    assert_eq!(
        ft1.count(&name) + ft2.count(&name),
        1,
        "expected exactly 1 durable emit across two concurrent replicas"
    );

    // last_fired moved off the epoch exactly once (now not due).
    let due = due_schedules(&pool1).await.unwrap();
    assert!(
        !due.contains(&name),
        "schedule {name:?} still due after firing"
    );

    cleanup(&pool1, &name).await;
}

/// A schedule re-arms: an immediate second fire is a no-op (not due), but after the
/// interval elapses it fires again (Go's `TestFiresAgainAfterInterval`).
#[tokio::test(flavor = "multi_thread")]
async fn fires_again_after_interval() {
    let Some(pool) = test_pool().await else {
        return;
    };

    let name = unique_name(&pool).await;
    seed_schedule(&pool, &name, 1).await; // 1s interval

    let (bus, ft) = bus_with_fake();

    fire(&pool, &bus, &name).await.expect("first fire");
    assert_eq!(ft.count(&name), 1, "after first fire want 1 durable emit");

    // Immediately not due — second fire is a no-op.
    fire(&pool, &bus, &name).await.expect("second (immediate) fire");
    assert_eq!(
        ft.count(&name),
        1,
        "immediate refire should be a no-op durable-emit-wise"
    );

    // After the interval it is due again.
    tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
    fire(&pool, &bus, &name).await.expect("third fire");
    assert_eq!(ft.count(&name), 2, "after interval want 2 durable emits");

    cleanup(&pool, &name).await;
}
