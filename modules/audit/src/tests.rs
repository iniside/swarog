//! audit tests. The durable handlers are driven directly against a real sqlx tx (the
//! same shape the asyncevents plane's `consume` uses — an insert/prune inside a tx that commits),
//! so they exercise the ledger SQL + tx atomicity without pulling in the transport
//! internals (asyncevents' own tests cover the delivery-tx checkpointing). The anti-drift topic-set
//! test needs no DB. Live-Postgres tests SKIP cleanly (early-return) when the local DB
//! is unreachable, so `cargo test` never hard-fails on a machine without it.

use std::collections::HashSet;

use super::*;

const DEFAULT_DSN: &str =
    "postgres://gamebackend:gamebackend@localhost:5432/gamebackend?sslmode=disable";

/// Connects to the test DB and ensures the schema; `None` (with a printed SKIP) when
/// Postgres is unreachable, so the live tests early-return instead of failing.
async fn test_pool() -> Option<PgPool> {
    let dsn = std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DSN.to_string());
    match PgPool::connect(&dsn).await {
        Ok(pool) => {
            sqlx::raw_sql(SCHEMA_DDL)
                .execute(&pool)
                .await
                .expect("migrate audit schema");
            Some(pool)
        }
        Err(e) => {
            eprintln!("SKIP audit live test: postgres unreachable: {e}");
            None
        }
    }
}

/// A run-unique marker topic so assertions/cleanup never collide on the shared DB.
async fn unique_topic(pool: &PgPool) -> String {
    let (s,): (String,) =
        sqlx::query_as("SELECT 'test.' || replace(gen_random_uuid()::text, '-', '')")
            .fetch_one(pool)
            .await
            .unwrap();
    s
}

async fn count_topic(pool: &PgPool, topic: &str) -> i64 {
    let (n,): (i64,) = sqlx::query_as("SELECT count(*)::int8 FROM audit.log WHERE topic = $1")
        .bind(topic)
        .fetch_one(pool)
        .await
        .unwrap();
    n
}

async fn insert_aged(pool: &PgPool, topic: &str, age_days: i32) {
    sqlx::query(
        "INSERT INTO audit.log (topic, payload, at) \
         VALUES ($1, '{}'::jsonb, now() - make_interval(days => $2))",
    )
    .bind(topic)
    .bind(age_days)
    .execute(pool)
    .await
    .unwrap();
}

/// Drives the prune handler with a `scheduler.fired{name}` payload inside a committed
/// tx — the same shape the asyncevents plane's consume runs the handler in.
async fn deliver_prune(pool: &PgPool, handler: &PruneHandler, name: &str) {
    let payload = serde_json::to_vec(&serde_json::json!({ "name": name })).unwrap();
    let mut tx = pool.begin().await.unwrap();
    let delivery = Delivery {
        event_id: "audit:test",
        tx: bus::AnyTx::new(&mut *tx),
    };
    handler.call(delivery, payload).await.unwrap();
    tx.commit().await.unwrap();
}

// --- anti-drift (no DB) -----------------------------------------------------

/// The anti-drift guard: [`DURABLE_TOPICS`] must equal EXACTLY the producers' declared
/// topics (with no duplicates). Imports the domain events crates and diffs the sets, so
/// a topic rename on either side fails the build (Go's `TestDurableTopicsMatchEvents`,
/// adjusted to the single durable list).
#[test]
fn durable_topics_match_events() {
    let got: HashSet<&str> = DURABLE_TOPICS.iter().copied().collect();
    assert_eq!(
        got.len(),
        DURABLE_TOPICS.len(),
        "duplicate topic in DURABLE_TOPICS"
    );

    let want: HashSet<&str> = [
        charactersevents::CREATED.topic(),
        charactersevents::DELETED.topic(),
        accountsevents::PLAYER_REGISTERED.topic(),
        configevents::CHANGED.topic(),
        matchevents::FINISHED.topic(),
        adminevents::ACTION.topic(),
    ]
    .into_iter()
    .collect();

    assert_eq!(
        got, want,
        "audited durable topic set drifted from the producers' declared event topics \
         (rename? stray topic? missing producer?)"
    );
}

/// The zip guard: [`DURABLE_SPEC_IDS`] pairs positionally with [`DURABLE_TOPICS`]
/// (a length mismatch would silently truncate the zip in `init`), and each id
/// follows the `audit.<topic-kebab>.v1` checkpoint convention so a topic rename
/// forces a conscious decision about its checkpoint.
#[test]
fn durable_spec_ids_zip_with_topics() {
    assert_eq!(
        DURABLE_SPEC_IDS.len(),
        DURABLE_TOPICS.len(),
        "DURABLE_SPEC_IDS and DURABLE_TOPICS must pair positionally"
    );
    for (topic, id) in DURABLE_TOPICS.iter().zip(DURABLE_SPEC_IDS) {
        let want = format!("audit.{}.v1", topic.replace('.', "-"));
        assert_eq!(*id, want, "spec id for {topic:?} broke the naming convention");
    }
}

/// `scheduler.fired` is CONSUMED (prune), never LOGGED — it must not be in the audited
/// set, or the anti-drift test would demand a matching producer event. Uses the
/// `schedulerevents::FIRED` descriptor's topic const (the same one `init` subscribes
/// with), so this guard tracks the contract, not a re-pinned literal.
#[test]
fn scheduler_fired_is_not_a_logged_topic() {
    assert!(
        !DURABLE_TOPICS.contains(&schedulerevents::FIRED.topic()),
        "scheduler.fired is reactive (prune), not a logged topic"
    );
}

// --- fail-closed retention gate (Audit::init) --------------------------------

/// Serializes the three env-mutating tests below — `AUDIT_RETENTION_DAYS` is
/// process-global, so they must not race each other (or observe each other's
/// writes). Same shape as admin's `ENV_LOCK`/`with_admin_env` harness (admin
/// `tests.rs`).
static RETENTION_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Runs `f` with `AUDIT_RETENTION_DAYS` set to `val` (`None` = unset), restoring the
/// prior value afterwards. Serialized so the three gate tests below never interleave
/// their env writes.
fn with_retention_env(val: Option<&str>, f: impl FnOnce()) {
    let _guard = RETENTION_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let prev = std::env::var("AUDIT_RETENTION_DAYS").ok();
    match val {
        Some(v) => std::env::set_var("AUDIT_RETENTION_DAYS", v),
        None => std::env::remove_var("AUDIT_RETENTION_DAYS"),
    }
    f();
    match prev {
        Some(v) => std::env::set_var("AUDIT_RETENTION_DAYS", v),
        None => std::env::remove_var("AUDIT_RETENTION_DAYS"),
    }
}

/// `AUDIT_RETENTION_DAYS=-1` FAILS startup — a negative retention would delete the
/// whole ledger on the next prune tick. The bail fires BEFORE `init` ever touches
/// `self.svc()`, so this needs no `register()` call and no live DB: `PgPool::
/// connect_lazy` never dials Postgres (it only needs a Tokio context to spawn its
/// background task, hence `#[tokio::test]`).
#[tokio::test(flavor = "multi_thread")]
async fn init_bails_on_negative_retention_days() {
    with_retention_env(Some("-1"), || {
        let ctx = Context::with_db(PgPool::connect_lazy(DEFAULT_DSN).unwrap());
        let err = Audit::new()
            .init(&ctx)
            .expect_err("negative AUDIT_RETENTION_DAYS must fail startup");
        assert!(
            err.to_string().contains("AUDIT_RETENTION_DAYS"),
            "bail message should name the offending knob: {err}"
        );
    });
}

/// `AUDIT_RETENTION_DAYS=0` FAILS startup too — a zero retention would also wipe every
/// row on the next prune tick, not merely truncate history.
#[tokio::test(flavor = "multi_thread")]
async fn init_bails_on_zero_retention_days() {
    with_retention_env(Some("0"), || {
        let ctx = Context::with_db(PgPool::connect_lazy(DEFAULT_DSN).unwrap());
        let err = Audit::new()
            .init(&ctx)
            .expect_err("zero AUDIT_RETENTION_DAYS must fail startup");
        assert!(
            err.to_string().contains("AUDIT_RETENTION_DAYS"),
            "bail message should name the offending knob: {err}"
        );
    });
}

/// An unset `AUDIT_RETENTION_DAYS` boots normally at the compiled default —
/// `env_int` falls back silently, so `init` must fully succeed (this test runs
/// `register` first so `self.svc()` resolves, exercising the real boot order).
/// `init` also subscribes durable handlers via `ctx.bus().on_tx_raw`, which panics
/// on a transport-less bus — so this needs `Context::with_db_and_transport` with the
/// `asyncevents::testing` in-memory transport handle (subscribing is pure
/// bookkeeping, no I/O, so a lazy pool is enough — no live Postgres needed).
#[tokio::test(flavor = "multi_thread")]
async fn init_ok_when_retention_unset() {
    with_retention_env(None, || {
        let pool = PgPool::connect_lazy(DEFAULT_DSN).unwrap();
        let transport = asyncevents::testing::transport(pool.clone());
        let ctx = Context::with_db_and_transport(pool, transport.handle());
        let audit = Audit::new();
        audit.register(&ctx).expect("register");
        audit
            .init(&ctx)
            .expect("unset AUDIT_RETENTION_DAYS must not fail startup");
    });
}

// --- live Postgres ----------------------------------------------------------

/// A durable event delivered through the record handler is written to the ledger
/// verbatim (topic + raw JSON), on the handed tx — no producer `*events` import needed
/// (Go's `TestDurableCharacterEventsAreLogged`, at the handler boundary).
#[tokio::test(flavor = "multi_thread")]
async fn record_handler_inserts_raw_json() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let topic = unique_topic(&pool).await;

    let handler = RecordHandler {
        topic: topic.clone(),
    };
    let raw = br#"{"character_id":"abc","name":"Test","class":"novice"}"#.to_vec();
    let mut tx = pool.begin().await.unwrap();
    let delivery = Delivery {
        event_id: "audit:test",
        tx: bus::AnyTx::new(&mut *tx),
    };
    handler.call(delivery, raw).await.unwrap();
    tx.commit().await.unwrap();

    let (n, name, event_id): (i64, String, String) = sqlx::query_as(
        "SELECT count(*)::int8, coalesce(max(payload->>'name'), ''), \
         coalesce(max(event_id), '') FROM audit.log WHERE topic = $1",
    )
    .bind(&topic)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(n, 1, "expected exactly one ledger row for the delivered event");
    assert_eq!(name, "Test", "raw JSON payload not recorded verbatim");
    assert_eq!(
        event_id, "audit:test",
        "ledger row did not carry the delivery's event_id"
    );

    sqlx::query("DELETE FROM audit.log WHERE topic = $1")
        .bind(&topic)
        .execute(&pool)
        .await
        .unwrap();
}

/// The prune reaction deletes rows past the retention window ONLY for the
/// `audit-prune` schedule name; any other name is a no-op, and fresh rows survive
/// (Go's `TestPruneViaDurable`, at the handler boundary).
#[tokio::test(flavor = "multi_thread")]
async fn prune_deletes_aged_rows_only_for_prune_name() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let old_topic = unique_topic(&pool).await;
    let fresh_topic = unique_topic(&pool).await;
    insert_aged(&pool, &old_topic, 60).await; // past the default 30d retention
    insert_aged(&pool, &fresh_topic, 0).await; // now — safe

    let handler = PruneHandler { retention_days: 30 };

    // A non-prune schedule name must NOT prune (proves the name filter).
    deliver_prune(&pool, &handler, "some-other-job").await;
    assert_eq!(
        count_topic(&pool, &old_topic).await,
        1,
        "non-prune schedule name pruned rows"
    );

    // audit-prune: the aged row goes, the fresh one stays.
    deliver_prune(&pool, &handler, PRUNE_SCHEDULE_NAME).await;
    assert_eq!(
        count_topic(&pool, &old_topic).await,
        0,
        "old row survived prune"
    );
    assert_eq!(
        count_topic(&pool, &fresh_topic).await,
        1,
        "fresh row was pruned"
    );

    sqlx::query("DELETE FROM audit.log WHERE topic IN ($1, $2)")
        .bind(&old_topic)
        .bind(&fresh_topic)
        .execute(&pool)
        .await
        .unwrap();
}
