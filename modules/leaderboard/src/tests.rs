//! leaderboard tests. The durable upsert is driven directly against a real sqlx tx (the
//! same shape the asyncevents plane's `consume` runs the handler in — an upsert inside a
//! tx that then commits), and the top-scores read against the pool. Live-Postgres tests SKIP cleanly
//! when the local DB is unreachable. In-crate so they drive the private `Service` +
//! `record_win` directly.

use std::future::Future;
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

/// The highest tally currently in the shared table, so a test can seat its own players
/// ABOVE the whole field and stay inside `top_scores`'s `LIMIT 100` however crowded it is.
async fn max_wins(pool: &PgPool) -> i64 {
    let (m,): (Option<i64>,) = sqlx::query_as("SELECT max(wins) FROM leaderboard.scores")
        .fetch_one(pool)
        .await
        .unwrap();
    m.unwrap_or(0)
}

async fn seed(pool: &PgPool, player: &str, wins: i64) {
    sqlx::query("INSERT INTO leaderboard.scores (player, wins) VALUES ($1, $2)")
        .bind(player)
        .bind(wins)
        .execute(pool)
        .await
        .unwrap();
}

/// Runs `record_win` inside a committed tx — the same shape the asyncevents plane's consume uses.
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

/// Runs `body` on its own task and deletes `players` even when it panicked, then re-raises
/// the panic: a row leaked by a red run crowds the top-100 page and reds every later run.
async fn with_cleanup<Fut>(pool: &PgPool, players: Vec<String>, body: Fut)
where
    Fut: Future<Output = ()> + Send + 'static,
{
    let outcome = tokio::spawn(body).await;
    let refs: Vec<&str> = players.iter().map(String::as_str).collect();
    cleanup(pool, &refs).await;
    if let Err(e) = outcome {
        if e.is_panic() {
            std::panic::resume_unwind(e.into_panic());
        }
        panic!("leaderboard test task ended without completing: {e}");
    }
}

/// The upsert on the handed tx: the first win INSERTs wins=1, each further win ADDS one
/// (ON CONFLICT). Proves the tally accumulates exactly-once per delivered event.
#[tokio::test]
async fn record_win_inserts_then_increments() {
    let Some(pool) = test_pool().await else { return };
    let player = unique_player(&pool).await;

    let (p, who) = (pool.clone(), player.clone());
    with_cleanup(&pool, vec![player], async move {
        deliver_win(&p, &who).await;
        assert_eq!(wins_of(&p, &who).await, Some(1), "first win -> wins=1");

        deliver_win(&p, &who).await;
        assert_eq!(wins_of(&p, &who).await, Some(2), "second win -> wins=2");
    })
    .await;
}

/// `top_scores` — the query `GET /leaderboard` serves — reports this run's tallies and ranks
/// the 2-win-ahead player first. Both players are seeded ABOVE the whole field, so the
/// `LIMIT 100` truncation cannot drop them however crowded the shared table is.
#[tokio::test]
async fn top_scores_reports_this_runs_tallies_above_the_field() {
    let Some(pool) = test_pool().await else { return };
    let hi = unique_player(&pool).await;
    let lo = unique_player(&pool).await;
    let base = max_wins(&pool).await;

    let (p, h, l) = (pool.clone(), hi.clone(), lo.clone());
    with_cleanup(&pool, vec![hi, lo], async move {
        seed(&p, &h, base + 1).await;
        seed(&p, &l, base).await;
        deliver_win(&p, &h).await;
        deliver_win(&p, &l).await;

        let svc = Service { pool: p.clone() };
        let scores = svc.top_scores().await.unwrap();
        let hi_at = scores
            .iter()
            .position(|s| s.player == h)
            .unwrap_or_else(|| panic!("{h} absent from the {}-row page", scores.len()));
        let lo_at = scores
            .iter()
            .position(|s| s.player == l)
            .unwrap_or_else(|| panic!("{l} absent from the {}-row page", scores.len()));

        assert!(hi_at < lo_at, "the higher tally must sort before the lower one");
        assert_eq!(scores[hi_at].wins, base + 2, "the 2-win-ahead tally is projected");
        assert_eq!(scores[lo_at].wins, base + 1, "the 1-win-ahead tally is projected");
    })
    .await;
}

/// The ordering contract of the production query itself — `wins DESC, player ASC` — asserted
/// over EVERY row it returns, so it holds whichever rows are in the window. The seeded tie
/// keeps the player-ASC branch non-vacuous.
#[tokio::test]
async fn top_scores_page_is_ordered_wins_desc_then_player_asc() {
    let Some(pool) = test_pool().await else { return };
    let a = unique_player(&pool).await;
    let b = unique_player(&pool).await;
    let base = max_wins(&pool).await;

    let (p, x, y) = (pool.clone(), a.clone(), b.clone());
    with_cleanup(&pool, vec![a, b], async move {
        seed(&p, &x, base).await;
        seed(&p, &y, base).await;
        deliver_win(&p, &x).await;
        deliver_win(&p, &y).await;

        let svc = Service { pool: p.clone() };
        let scores = svc.top_scores().await.unwrap();
        for pair in scores.windows(2) {
            let (l, r) = (&pair[0], &pair[1]);
            assert!(
                l.wins > r.wins || (l.wins == r.wins && l.player < r.player),
                "page breaks wins DESC, player ASC at {l:?} then {r:?}"
            );
        }

        let x_at = scores
            .iter()
            .position(|s| s.player == x)
            .unwrap_or_else(|| panic!("{x} absent from the {}-row page", scores.len()));
        let y_at = scores
            .iter()
            .position(|s| s.player == y)
            .unwrap_or_else(|| panic!("{y} absent from the {}-row page", scores.len()));
        assert_eq!(scores[x_at].wins, scores[y_at].wins, "the seeded pair ties on wins");
        assert_eq!(x_at < y_at, x < y, "a tie is broken by player ASC");
    })
    .await;
}
