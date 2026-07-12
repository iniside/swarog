//! Live-Postgres retention tests: floor math over active/paused/retired/Genesis
//! subscription mixes, the `min_retention_days` lower bound, `keep_forever`, and the
//! conservative "no `history_contracts` row = never delete" rule. Positions come
//! from real [`crate::store::append`] calls; `created_at` is backdated with an
//! UPDATE so the day bound is exercisable without waiting.

use super::*;
use crate::store;
use crate::transport::test_contract;
use sqlx::PgPool;

const DEFAULT_DSN: &str =
    "postgres://gamebackend:gamebackend@localhost:5432/gamebackend?sslmode=disable";

/// The sweep interval and readiness threshold are one checked configuration:
/// malformed input falls back, while zero, overflow, and clock-unobservable
/// thresholds fail startup.
#[test]
fn housekeep_config_is_strict_checked_and_authoritative() {
    let default = Config::from_value(None).unwrap();
    assert_eq!(default.interval, DEFAULT_INTERVAL);
    assert_eq!(default.stall_after, Duration::from_secs(3 * 3600));

    for (value, interval, stall_after) in [
        ("500ms", Duration::from_millis(500), Duration::from_millis(1500)),
        ("5m", Duration::from_secs(300), Duration::from_secs(900)),
        ("4h", Duration::from_secs(4 * 3600), Duration::from_secs(12 * 3600)),
    ] {
        assert_eq!(Config::from_value(Some(value)).unwrap(), Config { interval, stall_after });
    }

    for garbage in ["nonsense", "", "  ", "-1", "1.5h", "5 hours"] {
        assert_eq!(
            Config::from_value(Some(garbage)).unwrap(),
            default,
            "malformed {garbage:?} must retain the default fallback"
        );
    }
    for zero in ["0", "0s", "0ms", "0m", "0h", " 0s "] {
        let err = Config::from_value(Some(zero)).unwrap_err().to_string();
        assert!(err.contains("EVENTS_HOUSEKEEP_INTERVAL"), "{zero:?}: {err}");
    }
    for overflow in [format!("{}h", u64::MAX), format!("{}m", u64::MAX)] {
        let err = Config::from_value(Some(&overflow)).unwrap_err().to_string();
        assert!(err.contains("EVENTS_HOUSEKEEP_INTERVAL"), "{overflow:?}: {err}");
    }
    let triple_overflow = format!("{}s", u64::MAX / 3 + 1);
    let err = Config::from_value(Some(&triple_overflow)).unwrap_err().to_string();
    assert!(err.contains("EVENTS_HOUSEKEEP_INTERVAL"), "{err}");

    let first_unobservable_interval = u64::MAX / 3;
    let max_clock_interval = first_unobservable_interval - 1;
    let max_clock = Config::from_value(Some(&format!("{max_clock_interval}ms"))).unwrap();
    assert_eq!(max_clock.stall_after.as_millis(), u128::from(u64::MAX - 3));
    let unobservable = format!("{first_unobservable_interval}ms");
    let err = Config::from_value(Some(&unobservable)).unwrap_err().to_string();
    assert!(err.contains("less than u64::MAX - 1 milliseconds"), "{err}");

    assert_eq!(
        Config::from_var_result(Err(std::env::VarError::NotPresent)).unwrap(),
        default
    );
    assert_eq!(
        Config::from_var_result(Err(std::env::VarError::NotUnicode("present".into()))).unwrap(),
        default,
        "present non-Unicode input is malformed and follows the default fallback"
    );
}

async fn test_pool() -> Option<PgPool> {
    let dsn = std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DSN.to_string());
    let pool = match tokio::time::timeout(Duration::from_secs(3), PgPool::connect(&dsn)).await {
        Ok(Ok(p)) => p,
        _ => {
            eprintln!("SKIP: postgres unreachable at {dsn} — asyncevents retention tests skipped");
            return None;
        }
    };
    if let Err(err) = crate::Plane::new(pool.clone(), dsn).unwrap().migrate().await {
        eprintln!("SKIP: asyncevents migrate failed: {err}");
        return None;
    }
    Some(pool)
}

fn unique(prefix: &str) -> &'static str {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    Box::leak(format!("{prefix}.{}-{}", std::process::id(), nanos).into_boxed_str())
}

/// A per-call unique `tie_breaker` (nanos + pid derived) so concurrent tests seeding
/// synthetic events at the same `(generation, producer_xid)` never collide on the
/// events PK. Positive by construction (sign bit masked off).
fn unique_tie() -> i64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64;
    ((nanos ^ ((std::process::id() as u64) << 40)) as i64) & i64::MAX
}

/// Inserts one synthetic event directly at an explicit `(generation=0, producer_xid,
/// tie_breaker)` position, backdated `age_days`. `OVERRIDING SYSTEM VALUE` is required
/// because `tie_breaker` is `GENERATED ALWAYS`. Pinning `generation = 0` (< the seeded
/// `plane_meta.generation` of 1) makes the row frontier-eligible deterministically,
/// independent of the live cluster XID counter.
async fn insert_synthetic_event(
    pool: &PgPool,
    topic: &str,
    producer_xid: &str,
    tie_breaker: i64,
    age_days: i64,
) {
    sqlx::query(
        "INSERT INTO asyncevents.events \
           (generation, producer_xid, tie_breaker, topic, contract_version, payload, created_at) \
         OVERRIDING SYSTEM VALUE \
         VALUES (0, $1::xid8, $2, $3, 1, $4::jsonb, now() - make_interval(days => $5))",
    )
    .bind(producer_xid)
    .bind(tie_breaker)
    .bind(topic)
    .bind(format!(r#"{{"xid":{producer_xid}}}"#))
    .bind(age_days as i32)
    .execute(pool)
    .await
    .unwrap();
}

/// Appends `n` events on `topic` (one committed tx) and returns their positions in
/// log order as `(generation, producer_xid_text, tie_breaker)`.
async fn append_positions(pool: &PgPool, topic: &'static str, n: usize) -> Vec<(i64, String, i64)> {
    let mut tx = pool.begin().await.unwrap();
    for i in 0..n {
        let payload = format!(r#"{{"n":{i}}}"#);
        store::append(&mut tx, &test_contract(topic), payload.as_bytes())
            .await
            .unwrap();
    }
    tx.commit().await.unwrap();
    sqlx::query_as(
        // alias must NOT equal the column name: a bare ORDER BY prefers the output
        // alias (text sort) over the xid8 column. Positional read, so the alias name
        // is irrelevant to decoding.
        "SELECT generation, producer_xid::text AS producer_xid_text, tie_breaker FROM asyncevents.events \
         WHERE topic = $1 ORDER BY generation, producer_xid, tie_breaker",
    )
    .bind(topic)
    .fetch_all(pool)
    .await
    .unwrap()
}

/// Backdates every event on `topic` so it clears a `min_retention_days` bound.
async fn backdate(pool: &PgPool, topic: &str, days: i64) {
    sqlx::query(
        "UPDATE asyncevents.events SET created_at = now() - make_interval(days => $2) WHERE topic = $1",
    )
    .bind(topic)
    .bind(days as i32)
    .execute(pool)
    .await
    .unwrap();
}

async fn insert_contract(pool: &PgPool, topic: &str, policy: &str, days: i32) {
    sqlx::query(
        "INSERT INTO asyncevents.history_contracts (topic, contract_version, policy, min_retention_days) \
         VALUES ($1, 1, $2, $3) ON CONFLICT (topic, contract_version) DO UPDATE \
         SET policy = EXCLUDED.policy, min_retention_days = EXCLUDED.min_retention_days",
    )
    .bind(topic)
    .bind(policy)
    .bind(days)
    .execute(pool)
    .await
    .unwrap();
}

/// Inserts a subscription row at an explicit cursor. `cursor = None` materializes the
/// Genesis `(0, '0', 0)` never-run cursor that pins everything.
async fn insert_sub(
    pool: &PgPool,
    id: &str,
    topic: &str,
    state: &str,
    cursor: Option<&(i64, String, i64)>,
) {
    let (g, x, t) = match cursor {
        Some((g, x, t)) => (*g, x.clone(), *t),
        None => (0, "0".to_string(), 0),
    };
    sqlx::query(
        "INSERT INTO asyncevents.subscriptions \
           (subscription_id, topic, contract_version, state, \
            cursor_generation, cursor_xid, cursor_tie, spec_hash, start_kind, updated_at) \
         VALUES ($1, $2, 1, $3, $4, $5::xid8, $6, 'test', 'explicit', now())",
    )
    .bind(id)
    .bind(topic)
    .bind(state)
    .bind(g)
    .bind(x)
    .bind(t)
    .execute(pool)
    .await
    .unwrap();
}

async fn count_events(pool: &PgPool, topic: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM asyncevents.events WHERE topic = $1")
        .bind(topic)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn cleanup(pool: &PgPool, topic: &str) {
    let _ = sqlx::query("DELETE FROM asyncevents.events WHERE topic = $1")
        .bind(topic)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM asyncevents.subscriptions WHERE topic = $1")
        .bind(topic)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM asyncevents.history_contracts WHERE topic = $1")
        .bind(topic)
        .execute(pool)
        .await;
}

/// Floor = the active subscription's cursor: events strictly below it AND older than
/// `min_retention_days` are deleted; events at/above the cursor stay.
#[tokio::test]
async fn deletes_below_active_floor_only() {
    let Some(pool) = test_pool().await else { return };
    let topic = unique("ret.active");
    let pos = append_positions(&pool, topic, 5).await;
    backdate(&pool, topic, 40).await;
    insert_contract(&pool, topic, "min_retention", 30).await;
    // Cursor at pos[3]: events 0..2 are strictly below, events 3,4 are at/above.
    insert_sub(&pool, unique("sub.a"), topic, "active", Some(&pos[3])).await;

    gc_topic(&pool, topic, 1, 30).await.unwrap();

    assert_eq!(count_events(&pool, topic).await, 2, "only sub-floor events deleted");
    cleanup(&pool, topic).await;
}

/// A never-run Genesis subscription's `(0, '0', 0)` cursor pins EVERYTHING — no real
/// position (generation ≥ 1) is strictly below it, so GC deletes nothing.
#[tokio::test]
async fn genesis_never_run_pins_everything() {
    let Some(pool) = test_pool().await else { return };
    let topic = unique("ret.genesis");
    append_positions(&pool, topic, 4).await;
    backdate(&pool, topic, 90).await;
    insert_contract(&pool, topic, "min_retention", 7).await;
    insert_sub(&pool, unique("sub.g"), topic, "active", None).await; // Genesis cursor

    gc_topic(&pool, topic, 1, 7).await.unwrap();

    assert_eq!(count_events(&pool, topic).await, 4, "Genesis floor must retain all");
    cleanup(&pool, topic).await;
}

/// A PAUSED subscription's low cursor is part of the floor — it blocks GC exactly as
/// an active one would, and raises the blocked-age gauge.
#[tokio::test]
async fn paused_subscription_blocks_gc_and_raises_gauge() {
    let Some(pool) = test_pool().await else { return };
    let topic = unique("ret.paused");
    let pos = append_positions(&pool, topic, 5).await;
    backdate(&pool, topic, 40).await;
    insert_contract(&pool, topic, "min_retention", 30).await;
    // Active cursor is far ahead (pos[4]); a PAUSED sub sits at pos[1], pinning the
    // floor low so events 0 stays retained (below pos[1]) — the pause blocks GC.
    insert_sub(&pool, unique("sub.act"), topic, "active", Some(&pos[4])).await;
    insert_sub(&pool, unique("sub.pause"), topic, "paused", Some(&pos[1])).await;

    gc_topic(&pool, topic, 1, 30).await.unwrap();

    // Floor is min(pos[4], pos[1]) = pos[1]: only event 0 (strictly below pos[1]) is
    // deleted; events 1..4 stay, held back by the paused cursor.
    assert_eq!(count_events(&pool, topic).await, 4, "paused cursor must pin the floor");

    refresh_blocked_gauge(&pool).await.unwrap();
    assert!(
        blocked_gauge().get() > 0.0,
        "a paused subscription holding back a GC-eligible event must raise the alarm"
    );
    cleanup(&pool, topic).await;
}

/// A RETIRED subscription is excluded from the floor: its low cursor does NOT block
/// GC, so the active subscription's cursor alone governs deletion.
#[tokio::test]
async fn retired_subscription_does_not_block_gc() {
    let Some(pool) = test_pool().await else { return };
    let topic = unique("ret.retired");
    let pos = append_positions(&pool, topic, 5).await;
    backdate(&pool, topic, 40).await;
    insert_contract(&pool, topic, "min_retention", 30).await;
    insert_sub(&pool, unique("sub.act"), topic, "active", Some(&pos[4])).await;
    insert_sub(&pool, unique("sub.ret"), topic, "retired", None).await; // Genesis, retired

    gc_topic(&pool, topic, 1, 30).await.unwrap();

    // Floor = active's pos[4] (retired excluded); events 0..3 deleted, event 4 stays.
    assert_eq!(count_events(&pool, topic).await, 1, "retired cursor must not pin the floor");
    cleanup(&pool, topic).await;
}

/// The `min_retention_days` lower bound protects RECENT events even when they are
/// below the checkpoint floor.
#[tokio::test]
async fn min_retention_protects_recent_events() {
    let Some(pool) = test_pool().await else { return };
    let topic = unique("ret.recent");
    let pos = append_positions(&pool, topic, 4).await;
    // NOT backdated: created_at is ~now, so the 30-day bound protects all of them.
    insert_contract(&pool, topic, "min_retention", 30).await;
    insert_sub(&pool, unique("sub.a"), topic, "active", Some(&pos[3])).await;

    gc_topic(&pool, topic, 1, 30).await.unwrap();

    assert_eq!(count_events(&pool, topic).await, 4, "recent events survive the day bound");
    cleanup(&pool, topic).await;
}

/// `keep_forever` topics are never swept: `sweep` only enumerates `min_retention`
/// contracts, so an old below-floor event on a `keep_forever` topic stays.
#[tokio::test]
async fn keep_forever_never_deletes() {
    let Some(pool) = test_pool().await else { return };
    let topic = unique("ret.keep");
    let pos = append_positions(&pool, topic, 4).await;
    backdate(&pool, topic, 400).await;
    insert_contract(&pool, topic, "keep_forever", 7).await;
    insert_sub(&pool, unique("sub.a"), topic, "active", Some(&pos[3])).await;

    sweep(&pool).await.unwrap();

    assert_eq!(count_events(&pool, topic).await, 4, "keep_forever must never delete");
    cleanup(&pool, topic).await;
}

/// Conservative GC: a topic with NO `history_contracts` row is never deleted from,
/// even with old below-floor events and an advanced cursor.
#[tokio::test]
async fn topic_without_contract_is_never_deleted() {
    let Some(pool) = test_pool().await else { return };
    let topic = unique("ret.nocontract");
    let pos = append_positions(&pool, topic, 4).await;
    backdate(&pool, topic, 400).await;
    // No insert_contract: the topic carries no retention promise.
    insert_sub(&pool, unique("sub.a"), topic, "active", Some(&pos[3])).await;

    sweep(&pool).await.unwrap();

    assert_eq!(count_events(&pool, topic).await, 4, "unknown-policy topic must be kept");
    cleanup(&pool, topic).await;
}

/// A failed topic must not short-circuit the pass or be hidden as success. A
/// topic-specific DELETE trigger injects one failure while another contract is
/// still swept; the aggregate error identifies the failed topic.
#[tokio::test]
async fn sweep_continues_after_topic_failure_and_returns_contextual_error() {
    let Some(pool) = test_pool().await else { return };
    let bad_topic = unique("ret.fail.00-bad");
    let good_topic = unique("ret.fail.01-good");
    assert!(bad_topic < good_topic, "fixture must place the failing contract first");
    insert_synthetic_event(&pool, bad_topic, "7001", unique_tie(), 40).await;
    insert_synthetic_event(&pool, good_topic, "7002", unique_tie(), 40).await;
    insert_contract(&pool, bad_topic, "min_retention", 30).await;
    insert_contract(&pool, good_topic, "min_retention", 30).await;

    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let function = format!("retention_test_fail_{}_{}", std::process::id(), stamp);
    let trigger = format!("retention_test_trigger_{}_{}", std::process::id(), stamp);
    sqlx::raw_sql(&format!(
        "CREATE FUNCTION asyncevents.{function}() RETURNS trigger LANGUAGE plpgsql AS $$ \
         BEGIN \
           IF OLD.topic = TG_ARGV[0] THEN \
             IF pg_backend_pid() = TG_ARGV[2]::int THEN \
               RAISE EXCEPTION 'retention test failure for %', OLD.topic; \
             END IF; \
             RETURN NULL; \
           END IF; \
           IF OLD.topic = TG_ARGV[1] AND pg_backend_pid() <> TG_ARGV[2]::int THEN \
             RETURN NULL; \
           END IF; \
           RETURN OLD; \
         END $$"
    ))
    .execute(&pool)
    .await
    .unwrap();
    // A one-connection pool confines the trigger fault to this test's sweep;
    // concurrent retention tests use different PostgreSQL backend PIDs.
    let dsn = std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DSN.to_string());
    let fault_pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&dsn)
        .await
        .unwrap();
    let backend_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&fault_pool)
        .await
        .unwrap();
    sqlx::raw_sql(&format!(
        "CREATE TRIGGER {trigger} BEFORE DELETE ON asyncevents.events \
         FOR EACH ROW EXECUTE FUNCTION asyncevents.{function}(\
           '{bad_topic}', '{good_topic}', '{backend_pid}')"
    ))
    .execute(&pool)
    .await
    .unwrap();
    let result = sweep(&fault_pool).await;
    let bad_remaining = count_events(&pool, bad_topic).await;
    let good_remaining = count_events(&pool, good_topic).await;
    fault_pool.close().await;

    // Remove the fault injection and all test data before asserting, so a failed
    // assertion cannot poison later retention tests sharing this database.
    sqlx::raw_sql(&format!("DROP TRIGGER {trigger} ON asyncevents.events"))
        .execute(&pool)
        .await
        .unwrap();
    sqlx::raw_sql(&format!("DROP FUNCTION asyncevents.{function}()"))
        .execute(&pool)
        .await
        .unwrap();
    cleanup(&pool, bad_topic).await;
    cleanup(&pool, good_topic).await;

    let err = result.expect_err("one failed topic must fail the whole retention pass").to_string();
    assert!(err.contains(bad_topic), "aggregate error omitted bad topic: {err}");
    assert_eq!(bad_remaining, 1, "the trigger must preserve the bad topic's event");
    assert_eq!(good_remaining, 0, "a bad topic must not prevent later topic GC");
}

/// Regression: the GC floor must order candidate cursors by NUMERIC xid8, not by the
/// text alias. A paused sub at producer_xid 999 and an active sub at 1000: numerically
/// 999 < 1000, so the floor is the paused `(0,999,·)` cursor and GC deletes nothing
/// (both events sit AT a cursor, not below one). The old `cursor_xid::text AS
/// cursor_xid` + bare `ORDER BY` sorted lexicographically (`'1000' < '999'`), picked
/// the active 1000 cursor as the floor, and deleted the still-needed 999 event. Asserts
/// both events survive (buggy floor would leave 1).
#[tokio::test]
async fn floor_uses_numeric_xid_order_not_text() {
    let Some(pool) = test_pool().await else { return };
    let topic = unique("ret.xidorder");
    let t999 = unique_tie();
    let t1000 = unique_tie();
    // Two synthetic events, generation 0 (frontier-eligible vs plane_meta generation 1),
    // backdated 40d so the 30d retention bound is cleared and only the floor governs.
    insert_synthetic_event(&pool, topic, "999", t999, 40).await;
    insert_synthetic_event(&pool, topic, "1000", t1000, 40).await;
    insert_contract(&pool, topic, "min_retention", 30).await;
    // Each cursor points exactly AT its event (cursor tie == the event's tie_breaker).
    let paused_cursor = (0i64, "999".to_string(), t999);
    let active_cursor = (0i64, "1000".to_string(), t1000);
    insert_sub(&pool, unique("sub.paused"), topic, "paused", Some(&paused_cursor)).await;
    insert_sub(&pool, unique("sub.active"), topic, "active", Some(&active_cursor)).await;

    gc_topic(&pool, topic, 1, 30).await.unwrap();

    // Numeric floor = (0,999,t999): nothing is strictly below it, so both survive.
    // (Text floor picks (0,1000,·), deletes the below-it 999 event → 1 left.)
    assert_eq!(count_events(&pool, topic).await, 2, "numeric floor must retain both events");
    cleanup(&pool, topic).await;
}

/// Step 6: a retention task alive but whose sweeps persistently FAIL must flip
/// `Liveness::retention_stalled` within budget. Failure is injected by closing the
/// pool before `run` spawns: every `sweep` then errors (a closed pool is a
/// deterministic stand-in for a revoked function / broken query), so the clock —
/// seeded like `Plane::start` does — never advances and ages out.
#[tokio::test]
async fn persistent_sweep_failure_flips_retention_stalled() {
    let Some(pool) = test_pool().await else { return };
    pool.close().await; // every subsequent query errors
    let liveness = crate::Liveness::default();
    liveness.mark_retention_ok(); // seed at t0, exactly as Plane::start does
    assert!(
        !liveness.retention_stalled(Duration::from_secs(1)),
        "freshly seeded clock must not read as stalled"
    );

    let (_stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    let task = tokio::spawn(run(pool, Duration::from_millis(50), liveness.clone(), stop_rx));

    // The seed never advances (all sweeps fail), so once >1s of coarse time has
    // elapsed the check flips. Poll up to a generous budget to avoid clock races.
    let mut stalled = false;
    for _ in 0..80 {
        if liveness.retention_stalled(Duration::from_secs(1)) {
            stalled = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    task.abort();
    assert!(stalled, "persistently failing sweeps must flip retention_stalled within budget");
}

/// Step 6: healthy sweeps keep the retention clock fresh — `retention_stalled`
/// stays false. An idle pool's `sweep` is a no-op success (no `min_retention`
/// contracts), so the task marks progress every interval.
#[tokio::test]
async fn healthy_sweeps_keep_retention_unstalled() {
    let Some(pool) = test_pool().await else { return };
    let liveness = crate::Liveness::default();
    liveness.mark_retention_ok();

    let (_stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    let task = tokio::spawn(run(pool, Duration::from_millis(50), liveness.clone(), stop_rx));

    // Over several intervals the continuously-marked clock must never read stalled
    // against a 2s window (marks land every ~50ms).
    for _ in 0..30 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            !liveness.retention_stalled(Duration::from_secs(2)),
            "a continuously succeeding sweep must keep retention un-stalled"
        );
    }
    task.abort();
}

/// Step 6: a failing top-level sweep increments `asyncevents_retention_sweep_errors_total`.
/// The counter is process-global (OnceLock), so assert on the delta, not the value.
#[tokio::test]
async fn sweep_failure_increments_error_counter() {
    let Some(pool) = test_pool().await else { return };
    pool.close().await;
    let before = sweep_errors().get();

    let liveness = crate::Liveness::default();
    let (_stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    let task = tokio::spawn(run(pool, Duration::from_millis(20), liveness, stop_rx));
    // Give the ticker time for at least one failing sweep past the first-tick skip.
    tokio::time::sleep(Duration::from_millis(300)).await;
    task.abort();

    assert!(
        sweep_errors().get() > before,
        "a failing sweep must increment the sweep-error counter (before={before}, after={})",
        sweep_errors().get()
    );
}

/// Interval parsing: Go-style units, bare seconds, and default fallback.
#[test]
fn interval_parses_go_durations() {
    assert_eq!(parse_go_duration("1h"), Ok(Some(Duration::from_secs(3600))));
    assert_eq!(parse_go_duration("30m"), Ok(Some(Duration::from_secs(1800))));
    assert_eq!(parse_go_duration("45s"), Ok(Some(Duration::from_secs(45))));
    assert_eq!(parse_go_duration("500ms"), Ok(Some(Duration::from_millis(500))));
    assert_eq!(parse_go_duration("90"), Ok(Some(Duration::from_secs(90))));
    assert_eq!(parse_go_duration("nonsense"), Ok(None));
}
