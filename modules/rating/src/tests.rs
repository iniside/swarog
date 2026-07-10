//! rating tests. Live-Postgres: they drive the private `Service` + `apply_result`
//! directly. `apply_result` runs inside a committed tx (the same shape the asyncevents
//! plane's delivery runs the handler in — the projection + checkpoint commit together),
//! and `mmr` reads the projection back. Live tests SKIP cleanly when the local DB is
//! unreachable.

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
            eprintln!("SKIP: postgres unreachable at {dsn} — rating DB tests skipped");
            return None;
        }
    };
    // `CREATE SCHEMA IF NOT EXISTS` is not atomic against concurrent creation — parallel
    // tests can race to a unique-violation. One retry suffices: the loser re-runs the
    // fully-idempotent DDL once the winner has created the schema.
    if sqlx::raw_sql(SCHEMA_DDL).execute(&pool).await.is_err() {
        sqlx::raw_sql(SCHEMA_DDL)
            .execute(&pool)
            .await
            .expect("migrate rating schema");
    }
    Some(pool)
}

/// A run-unique player id so parallel test runs never collide on the shared DB.
async fn unique_player(pool: &PgPool) -> String {
    let (s,): (String,) = sqlx::query_as("SELECT 'r.' || replace(gen_random_uuid()::text, '-', '')")
        .fetch_one(pool)
        .await
        .unwrap();
    s
}

/// Delivers one finished match through `apply_result` inside a committed tx — the same
/// shape the plane's delivery uses (handler runs on the tx connection, then commit).
async fn deliver_match(pool: &PgPool, winner: &str, loser: &str) {
    let mut tx = pool.begin().await.unwrap();
    apply_result(&mut tx, winner, loser).await.unwrap();
    tx.commit().await.unwrap();
}

async fn cleanup(pool: &PgPool, players: &[&str]) {
    for p in players {
        let _ = sqlx::query("DELETE FROM rating.ratings WHERE player = $1")
            .bind(p)
            .execute(pool)
            .await;
    }
}

/// An unseen player reads the 1000 default from the projection; `MmrReader` returns it too.
#[tokio::test]
async fn unseen_player_defaults_to_1000() {
    let Some(pool) = test_pool().await else { return };
    let nobody = unique_player(&pool).await;
    let svc = Service { pool: pool.clone() };
    assert_eq!(svc.mmr(nobody).await.unwrap(), 1000);
}

/// Two reports for the same winner ACCUMULATE in the projection: winner 1030 after two
/// wins, loser 970. Proves the handler reads-then-writes each player's live persisted value.
#[tokio::test]
async fn ratings_accumulate_across_matches() {
    let Some(pool) = test_pool().await else { return };
    let winner = unique_player(&pool).await;
    let loser = unique_player(&pool).await;

    deliver_match(&pool, &winner, &loser).await;
    let svc = Service { pool: pool.clone() };
    assert_eq!(svc.mmr(winner.clone()).await.unwrap(), 1015);
    assert_eq!(svc.mmr(loser.clone()).await.unwrap(), 985);

    deliver_match(&pool, &winner, &loser).await;
    assert_eq!(svc.mmr(winner.clone()).await.unwrap(), 1030);
    assert_eq!(svc.mmr(loser.clone()).await.unwrap(), 970);

    cleanup(&pool, &[&winner, &loser]).await;
}

/// A restart no longer resets MMR: a FRESH `Service` over the same pool (the module
/// reconstructed on restart) reads the persisted value, not the 1000 default.
#[tokio::test]
async fn restart_preserves_mmr() {
    let Some(pool) = test_pool().await else { return };
    let winner = unique_player(&pool).await;
    let loser = unique_player(&pool).await;

    let before = Service { pool: pool.clone() };
    deliver_match(&pool, &winner, &loser).await;
    assert_eq!(before.mmr(winner.clone()).await.unwrap(), 1015);
    drop(before);

    // Restart: a new Service instance over the same DB.
    let after = Service { pool: pool.clone() };
    assert_eq!(after.mmr(winner.clone()).await.unwrap(), 1015);
    assert_eq!(after.mmr(loser.clone()).await.unwrap(), 985);

    cleanup(&pool, &[&winner, &loser]).await;
}

/// Both players are upserted within ONE delivery: after a single `apply_result` tx commits,
/// the winner AND the loser rows both exist and moved. Crash-safety of the pair comes free
/// from the delivery tx — this asserts the handler writes both rows atomically.
#[tokio::test]
async fn both_players_upserted_in_one_delivery() {
    let Some(pool) = test_pool().await else { return };
    let winner = unique_player(&pool).await;
    let loser = unique_player(&pool).await;

    deliver_match(&pool, &winner, &loser).await;

    let rows: Vec<(String, i32)> =
        sqlx::query_as("SELECT player, mmr FROM rating.ratings WHERE player = $1 OR player = $2")
            .bind(&winner)
            .bind(&loser)
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(rows.len(), 2, "one delivery must persist BOTH players");

    cleanup(&pool, &[&winner, &loser]).await;
}
