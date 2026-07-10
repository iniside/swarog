use super::*;
use bus::HistoryPolicy;
use sqlx::{PgPool, Row};
use std::time::Duration;

/// Fallback DSN when `DATABASE_URL` is unset — the same default `core/app` uses.
const DEFAULT_DSN: &str =
    "postgres://gamebackend:gamebackend@localhost:5432/gamebackend?sslmode=disable";

/// Serializes the two tests that choreograph the WRITER advisory lock with an
/// open transaction held across awaits. If they interleave, Postgres lock
/// fairness deadlocks them: the frontier test holds tx A's SHARED lock while
/// awaiting tx B's append; the bump test's pending EXCLUSIVE makes B queue
/// behind it; A never commits, so the exclusive never grants — a Rust-await ↔
/// DB-lock cycle Postgres cannot detect. Any new test that holds an appended
/// tx open across further lock-taking awaits must take this guard too.
static WRITER_LOCK_CHOREOGRAPHY: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Opens the local Postgres and ensures the V2 schema; returns `None` (printing a
/// skip line) when it's unreachable, so the suite degrades to a no-op without a DB.
/// Guards run only in their dedicated test — a guard failure mid-suite must not
/// silently skip unrelated tests.
async fn test_pool() -> Option<PgPool> {
    let dsn = std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DSN.to_string());
    let pool = match tokio::time::timeout(Duration::from_secs(3), PgPool::connect(&dsn)).await {
        Ok(Ok(p)) => p,
        _ => {
            eprintln!("SKIP: postgres unreachable at {dsn} — asyncevents store tests skipped");
            return None;
        }
    };
    if let Err(err) = ensure_schema(&pool).await {
        eprintln!("SKIP: asyncevents V2 migrate failed: {err}");
        return None;
    }
    Some(pool)
}

/// A unique, leaked topic so a rerun's rows never collide with a previous run's
/// (contracts require `&'static str`; leaking in a test binary is fine).
fn unique_topic(prefix: &str) -> &'static str {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    Box::leak(format!("{prefix}.{}-{}", std::process::id(), nanos).into_boxed_str())
}

fn contract(topic: &'static str) -> EventContract {
    EventContract {
        topic,
        version: 1,
        history: HistoryPolicy::MinRetention { days: 7 },
    }
}

/// The reader's frontier rule under test everywhere below: only rows whose
/// producer xid is strictly below the current snapshot's xmin are eligible.
async fn eligible_count(pool: &PgPool, topic: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM asyncevents.events \
         WHERE topic = $1 AND producer_xid < pg_snapshot_xmin(pg_current_snapshot())",
    )
    .bind(topic)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// Frontier advance depends on UNRELATED in-flight transactions (other tests in
/// this binary) finishing, so the post-commit assertion polls instead of reading
/// once.
async fn poll_eligible(pool: &PgPool, topic: &str, want: i64) -> i64 {
    for _ in 0..100 {
        let n = eligible_count(pool, topic).await;
        if n == want {
            return n;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    eligible_count(pool, topic).await
}

async fn count_topic(pool: &PgPool, topic: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM asyncevents.events WHERE topic = $1")
        .bind(topic)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn cleanup_topic(pool: &PgPool, topic: &str) {
    let _ = sqlx::query("DELETE FROM asyncevents.events WHERE topic = $1")
        .bind(topic)
        .execute(pool)
        .await;
}

/// The xid8-as-text codec convention round-trips exactly: a `u64` bound as text
/// with `$1::xid8` comes back out of `::text` as the same `u64`, across the full
/// unsigned range (including values above `i64::MAX`, which is why no signed
/// codec can carry it).
#[tokio::test]
async fn xid8_text_codec_round_trips() {
    let Some(pool) = test_pool().await else { return };
    for v in [1u64, 2, 12_345, i64::MAX as u64 + 7, u64::MAX] {
        let out: String = sqlx::query_scalar("SELECT ($1::xid8)::text")
            .bind(v.to_string())
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(parse_xid8(&out).unwrap(), v, "xid8 round-trip mangled {v}");
    }
}

#[test]
fn parse_xid8_rejects_non_u64() {
    assert_eq!(parse_xid8("18446744073709551615").unwrap(), u64::MAX);
    assert!(parse_xid8("-1").is_err());
    assert!(parse_xid8("nonsense").is_err());
}

/// `tie_breaker` orders multiple events appended in ONE transaction: same
/// `producer_xid`, strictly increasing ties, log order = append order.
#[tokio::test]
async fn tie_breaker_orders_events_within_one_tx() {
    let Some(pool) = test_pool().await else { return };
    let topic = unique_topic("store.tie");
    let c = contract(topic);

    let mut tx = pool.begin().await.unwrap();
    let mut appended = Vec::new();
    for n in 0..3 {
        let payload = format!(r#"{{"n":{n}}}"#);
        appended.push(append(&mut tx, &c, payload.as_bytes()).await.unwrap());
    }
    tx.commit().await.unwrap();

    let rows = sqlx::query(
        "SELECT event_id, producer_xid::text AS xid, tie_breaker, contract_version, payload->>'n' AS n \
         FROM asyncevents.events WHERE topic = $1 \
         ORDER BY generation, producer_xid, tie_breaker",
    )
    .bind(topic)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(rows.len(), 3);
    let xid0 = parse_xid8(&rows[0].get::<String, _>("xid")).unwrap();
    let mut last_tie = i64::MIN;
    for (i, row) in rows.iter().enumerate() {
        assert_eq!(row.get::<String, _>("event_id"), appended[i], "log order != append order");
        assert_eq!(row.get::<String, _>("n"), i.to_string());
        assert_eq!(row.get::<i32, _>("contract_version"), 1);
        assert_eq!(parse_xid8(&row.get::<String, _>("xid")).unwrap(), xid0, "one tx, one xid");
        let tie = row.get::<i64, _>("tie_breaker");
        assert!(tie > last_tie, "tie_breaker not strictly increasing");
        last_tie = tie;
    }

    cleanup_topic(&pool, topic).await;
}

/// XID-inversion safety: tx A takes an EARLIER xid but commits LATER than tx B.
/// While A is in flight, B's already-committed row (later xid) is NOT eligible —
/// the frontier `producer_xid < pg_snapshot_xmin(...)` holds at or below A's xid
/// — so a reader that advanced past B could never have left A's row behind as a
/// gap. Only after A commits do both become eligible.
#[tokio::test]
async fn frontier_never_exposes_gap_behind_inflight_earlier_xid() {
    let _choreo = WRITER_LOCK_CHOREOGRAPHY.lock().await;
    let Some(pool) = test_pool().await else { return };
    let topic = unique_topic("store.inversion");
    let c = contract(topic);

    // A appends first — earlier xid — and stays open.
    let mut tx_a = pool.begin().await.unwrap();
    append(&mut tx_a, &c, br#"{"who":"a"}"#).await.unwrap();
    let xid_a: String = sqlx::query_scalar("SELECT pg_current_xact_id()::text")
        .fetch_one(&mut *tx_a)
        .await
        .unwrap();

    // B appends second — later xid — and commits FIRST (the inversion).
    let mut tx_b = pool.begin().await.unwrap();
    append(&mut tx_b, &c, br#"{"who":"b"}"#).await.unwrap();
    let xid_b: String = sqlx::query_scalar("SELECT pg_current_xact_id()::text")
        .fetch_one(&mut *tx_b)
        .await
        .unwrap();
    tx_b.commit().await.unwrap();
    assert!(
        parse_xid8(&xid_a).unwrap() < parse_xid8(&xid_b).unwrap(),
        "test premise broken: A must hold the earlier xid"
    );

    // B is committed and visible to a plain read, but NOT eligible: exposing it
    // would let a cursor advance past the position A will later commit into.
    let visible: i64 = sqlx::query_scalar("SELECT count(*) FROM asyncevents.events WHERE topic = $1")
        .bind(topic)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(visible, 1, "B's committed row should be plainly visible");
    assert_eq!(
        eligible_count(&pool, topic).await,
        0,
        "frontier exposed a later-xid row while an earlier xid was still in flight"
    );

    // A commits; the frontier passes both and they become eligible together.
    tx_a.commit().await.unwrap();
    assert_eq!(poll_eligible(&pool, topic, 2).await, 2, "both rows eligible after A commits");

    cleanup_topic(&pool, topic).await;
}

/// The generation fence: [`bump_generation`]'s EXCLUSIVE advisory lock waits out
/// an in-flight SHARED writer, so no old-generation event can commit after the
/// bump does.
#[tokio::test]
async fn exclusive_bump_blocks_while_shared_writer_in_flight() {
    let _choreo = WRITER_LOCK_CHOREOGRAPHY.lock().await;
    let Some(pool) = test_pool().await else { return };
    let topic = unique_topic("store.bump");
    let c = contract(topic);

    let gen_before: i64 =
        sqlx::query_scalar("SELECT generation FROM asyncevents.plane_meta WHERE singleton")
            .fetch_one(&pool)
            .await
            .unwrap();

    // A writer holds the shared lock until its tx ends.
    let mut tx = pool.begin().await.unwrap();
    append(&mut tx, &c, br#"{"who":"writer"}"#).await.unwrap();

    let bump_pool = pool.clone();
    let bump = tokio::spawn(async move { bump_generation(&bump_pool).await });
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!bump.is_finished(), "bump completed while a shared writer was in flight");

    tx.commit().await.unwrap();
    let gen_after = bump.await.unwrap().unwrap();
    assert!(gen_after > gen_before, "bump did not advance the generation");
    let recorded: i64 =
        sqlx::query_scalar("SELECT generation FROM asyncevents.plane_meta WHERE singleton")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(recorded >= gen_after);

    cleanup_topic(&pool, topic).await;
}

/// A pre-existing `history_contracts` row with a DIFFERENT policy than the code
/// contract FAILS the emit loudly — a topic's history promise is immutable and is
/// never silently adopted. Asserted at both the `ensure_history_contract` seam and
/// through the transport's native-writer `enqueue_tx` path.
#[tokio::test]
async fn history_contract_conflict_fails_emit() {
    use bus::{AnyTx, Transport};

    let Some(pool) = test_pool().await else { return };
    let topic = unique_topic("store.histconflict");

    // Pre-seed keep_forever; the code contract below declares min_retention/7.
    sqlx::query(
        "INSERT INTO asyncevents.history_contracts (topic, contract_version, policy, min_retention_days) \
         VALUES ($1, 1, 'keep_forever', 7)",
    )
    .bind(topic)
    .execute(&pool)
    .await
    .unwrap();

    let c = contract(topic); // min_retention / 7 days — a policy mismatch
    let mut conn = pool.acquire().await.unwrap();
    let err = ensure_history_contract(&mut conn, c.topic, c.version, c.history)
        .await
        .expect_err("a policy conflict must fail loudly");
    assert!(err.to_string().contains("history_contracts"), "unexpected error: {err}");
    assert!(err.to_string().contains("immutable"), "must name the immutability rule: {err}");

    // The native-writer emit path surfaces the same failure — and does NOT append.
    let t = crate::LogTransport::new();
    let mut tx = pool.begin().await.unwrap();
    let emit_err = t
        .enqueue_tx(AnyTx::new(&mut *tx), &c, br#"{"a":1}"#)
        .await
        .expect_err("emit must fail on a drifted history policy");
    assert!(emit_err.to_string().contains("immutable"), "unexpected emit error: {emit_err}");
    tx.rollback().await.unwrap();
    assert_eq!(
        count_topic(&pool, topic).await,
        0,
        "a conflict-failed emit must not append an event"
    );

    let _ = sqlx::query("DELETE FROM asyncevents.history_contracts WHERE topic = $1")
        .bind(topic)
        .execute(&pool)
        .await;
    cleanup_topic(&pool, topic).await;
}

/// A matching (or absent) policy lets `ensure_history_contract` succeed and seeds the
/// row idempotently — the healthy native-writer / reconcile path.
#[tokio::test]
async fn history_contract_seeds_and_is_idempotent() {
    let Some(pool) = test_pool().await else { return };
    let topic = unique_topic("store.histseed");
    let c = contract(topic);
    let mut conn = pool.acquire().await.unwrap();

    ensure_history_contract(&mut conn, c.topic, c.version, c.history).await.unwrap();
    ensure_history_contract(&mut conn, c.topic, c.version, c.history).await.unwrap(); // idempotent

    let (policy, days): (String, i32) = sqlx::query_as(
        "SELECT policy, min_retention_days FROM asyncevents.history_contracts \
         WHERE topic = $1 AND contract_version = 1",
    )
    .bind(topic)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!((policy.as_str(), days), ("min_retention", 7));

    let _ = sqlx::query("DELETE FROM asyncevents.history_contracts WHERE topic = $1")
        .bind(topic)
        .execute(&pool)
        .await;
    cleanup_topic(&pool, topic).await;
}

/// The identity guard: a `system_identifier` differing from `plane_meta` fails
/// the boot with the bump remedial. Staged inside an uncommitted transaction so
/// the mismatch is never visible to concurrently running suites; the healthy
/// path is asserted on the same pool first.
#[tokio::test]
async fn identity_mismatch_guard_fails_startup() {
    let Some(pool) = test_pool().await else { return };

    startup_guards(&pool).await.expect("guards must pass on a healthy DB");

    let mut tx = pool.begin().await.unwrap();
    sqlx::query(
        "UPDATE asyncevents.plane_meta SET system_identifier = system_identifier + 1 WHERE singleton",
    )
    .execute(&mut *tx)
    .await
    .unwrap();
    let err = startup_guards_on(&mut tx)
        .await
        .expect_err("guards must fail on a cluster-identity mismatch");
    assert!(
        err.to_string().contains("system_identifier"),
        "unexpected guard error: {err}"
    );
    assert!(
        err.to_string().contains("bump-generation"),
        "guard error must name the remedial: {err}"
    );
    tx.rollback().await.unwrap();
}
