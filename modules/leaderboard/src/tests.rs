//! leaderboard tests. The durable upsert is driven directly against a real sqlx tx (the
//! same shape messaging's `consume` runs the handler in — an upsert inside a tx that then
//! commits), and the top-scores read against the pool. Live-Postgres tests SKIP cleanly
//! when the local DB is unreachable. In-crate so they drive the private `Service` +
//! `record_win` directly.

use std::time::Duration;

use super::*;

const DEFAULT_DSN: &str =
    "postgres://gamebackend:gamebackend@localhost:5432/gamebackend?sslmode=disable";

/// Opens the local Postgres and ensures the schema; `None` (with a printed SKIP) when
/// unreachable, so the live tests early-return instead of failing.
async fn test_pool() -> Option<PgPool> {
    let dsn = std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DSN.to_string());
    let pool = match tokio::time::timeout(Duration::from_secs(3), PgPool::connect(&dsn)).await {
        Ok(Ok(p)) => p,
        _ => {
            eprintln!("SKIP: postgres unreachable at {dsn} — leaderboard DB tests skipped");
            return None;
        }
    };
    sqlx::raw_sql(SCHEMA_DDL)
        .execute(&pool)
        .await
        .expect("migrate leaderboard schema");
    Some(pool)
}

/// A run-unique player id so parallel test runs never collide on the shared DB.
async fn unique_player(pool: &PgPool) -> String {
    let (s,): (String,) = sqlx::query_as("SELECT 'lb.' || replace(gen_random_uuid()::text, '-', '')")
        .fetch_one(pool)
        .await
        .unwrap();
    s
}

async fn wins_of(pool: &PgPool, player: &str) -> Option<i64> {
    let row: Option<(i64,)> = sqlx::query_as("SELECT wins FROM leaderboard.scores WHERE player = $1")
        .bind(player)
        .fetch_optional(pool)
        .await
        .unwrap();
    row.map(|(w,)| w)
}

/// Runs `record_win` inside a committed tx — the same shape messaging's consume uses.
async fn deliver_win(pool: &PgPool, player: &str) {
    let mut tx = pool.begin().await.unwrap();
    record_win(&mut tx, player).await.unwrap();
    tx.commit().await.unwrap();
}

async fn cleanup(pool: &PgPool, players: &[&str]) {
    for p in players {
        let _ = sqlx::query("DELETE FROM leaderboard.scores WHERE player = $1")
            .bind(p)
            .execute(pool)
            .await;
    }
}

/// The upsert on the handed tx: the first win INSERTs wins=1, each further win ADDS one
/// (ON CONFLICT). Proves the tally accumulates exactly-once per delivered event.
#[tokio::test]
async fn record_win_inserts_then_increments() {
    let Some(pool) = test_pool().await else { return };
    let player = unique_player(&pool).await;

    deliver_win(&pool, &player).await;
    assert_eq!(wins_of(&pool, &player).await, Some(1), "first win -> wins=1");

    deliver_win(&pool, &player).await;
    assert_eq!(wins_of(&pool, &player).await, Some(2), "second win -> wins=2");

    cleanup(&pool, &[&player]).await;
}

/// `top_scores` orders by wins DESC then player ASC and reflects the tallies — the same
/// query the public `GET /leaderboard` op serves.
#[tokio::test]
async fn top_scores_orders_by_wins_desc() {
    let Some(pool) = test_pool().await else { return };
    let hi = unique_player(&pool).await;
    let lo = unique_player(&pool).await;

    deliver_win(&pool, &hi).await;
    deliver_win(&pool, &hi).await;
    deliver_win(&pool, &lo).await;

    let svc = Service { pool: pool.clone() };
    let scores = svc.top_scores().await.unwrap();

    // Filter to this run's players (the shared table may hold others).
    let mine: Vec<&Score> = scores.iter().filter(|s| s.player == hi || s.player == lo).collect();
    assert_eq!(mine.len(), 2);
    assert_eq!(mine[0].player, hi, "the 2-win player must sort before the 1-win player");
    assert_eq!(mine[0].wins, 2);
    assert_eq!(mine[1].player, lo);
    assert_eq!(mine[1].wins, 1);

    cleanup(&pool, &[&hi, &lo]).await;
}
