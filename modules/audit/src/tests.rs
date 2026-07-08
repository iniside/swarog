//! audit tests. The durable handlers are driven directly against a real sqlx tx (the
//! same shape messaging's `consume` uses — an insert/prune inside a tx that commits),
//! so they exercise the ledger SQL + tx atomicity without pulling in the transport
//! internals (messaging's own tests cover the inbox dedup). The anti-drift topic-set
//! test needs no DB. Live-Postgres tests SKIP cleanly (early-return) when the local DB
//! is unreachable, so `cargo test` never hard-fails on a machine without it.

use std::collections::HashSet;

use super::*;

const DEFAULT_DSN: &str =
    "postgres://gamebackend:gamebackend@localhost:5432/gamebackend?sslmode=disable";

/// The `match.finished` topic literal — audit subscribes to it (Step 8) but the
/// `api/match/events` crate does not exist until Step 10, so the anti-drift test pins
/// the string here. When that crate lands, swap this for `matchevents::FINISHED.topic()`.
const MATCH_FINISHED_TOPIC: &str = "match.finished";

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
/// tx — the same shape messaging's consume runs the handler in.
async fn deliver_prune(pool: &PgPool, handler: &PruneHandler, name: &str) {
    let payload = serde_json::to_vec(&serde_json::json!({ "name": name })).unwrap();
    let mut tx = pool.begin().await.unwrap();
    handler.call(&mut tx, payload).await.unwrap();
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
        MATCH_FINISHED_TOPIC,
    ]
    .into_iter()
    .collect();

    assert_eq!(
        got, want,
        "audited durable topic set drifted from the producers' declared event topics \
         (rename? stray topic? missing producer?)"
    );
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
    handler.call(&mut tx, raw).await.unwrap();
    tx.commit().await.unwrap();

    let (n, name): (i64, String) = sqlx::query_as(
        "SELECT count(*)::int8, coalesce(max(payload->>'name'), '') FROM audit.log WHERE topic = $1",
    )
    .bind(&topic)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(n, 1, "expected exactly one ledger row for the delivered event");
    assert_eq!(name, "Test", "raw JSON payload not recorded verbatim");

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
