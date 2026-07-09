//! match tests. Unit tests validate `report`'s durable write shape; the live-Postgres
//! integration tests target the local Postgres (the test DB) and SKIP cleanly when it is
//! unreachable. The `validate_requires` test proves match fails loud without `rating`.
//! In-crate so they can drive the private `Service` directly.

use std::time::Duration;

use super::*;
use rating::Rating;

const DEFAULT_DSN: &str =
    "postgres://gamebackend:gamebackend@localhost:5432/gamebackend?sslmode=disable";

/// Opens the local Postgres; returns `None` (printing a skip line) when unreachable, so
/// the suite RUNS but SKIPs cleanly with no DB.
async fn test_pool() -> Option<PgPool> {
    let dsn = std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DSN.to_string());
    let pool = match tokio::time::timeout(Duration::from_secs(3), PgPool::connect(&dsn)).await {
        Ok(Ok(p)) => p,
        _ => {
            eprintln!("SKIP: postgres unreachable at {dsn} — match DB tests skipped");
            return None;
        }
    };
    Some(pool)
}

/// Migrates BOTH the asyncevents (durable plane's outbox) and match schemas EXACTLY
/// ONCE per test binary — concurrent idempotent DDL across parallel tests can deadlock on
/// catalog locks.
static SCHEMA_READY: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();

async fn ensure_schema(pool: &PgPool) {
    SCHEMA_READY
        .get_or_init(|| async {
            let dsn = std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DSN.to_string());
            asyncevents::Plane::new(pool.clone(), dsn)
                .unwrap()
                .migrate()
                .await
                .unwrap();
            let ctx = Context::with_db(pool.clone());
            let mm = MatchModule::new();
            mm.register(&ctx).unwrap();
            mm.migrate(&ctx).await.unwrap();
        })
        .await;
}

/// Builds a real durable plane over the live pool with a REAL in-memory `rating` module
/// filling match's `MmrReader` dependency: schemas migrated once, the asyncevents
/// transport is injected at `Context` construction, rating provides `MmrReader`, then
/// match registers/inits against the same ctx. Returns the ctx (owns the bus + registry)
/// and the wired match service.
async fn wired(pool: &PgPool) -> (Context, Arc<Service>) {
    ensure_schema(pool).await;
    let transport = asyncevents::transport(pool.clone(), "test-origin");
    let ctx = Context::with_db_and_transport(pool.clone(), transport);

    let rating = Rating::new();
    rating.register(&ctx).unwrap();
    rating.init(&ctx).unwrap();

    let mm = MatchModule::new();
    mm.register(&ctx).unwrap();
    mm.init(&ctx).unwrap();

    let svc = mm.svc();
    (ctx, svc)
}

async fn cleanup(pool: &PgPool, match_id: &str) {
    let _ = sqlx::query("DELETE FROM match.matches WHERE id = $1::uuid")
        .bind(match_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM asyncevents.outbox WHERE payload->>'match_id' = $1")
        .bind(match_id)
        .execute(pool)
        .await;
}

async fn match_count(pool: &PgPool, id: &str) -> i64 {
    let (n,): (i64,) = sqlx::query_as("SELECT count(*) FROM match.matches WHERE id = $1::uuid")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap();
    n
}

async fn outbox_count(pool: &PgPool, match_id: &str) -> i64 {
    let (n,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM asyncevents.outbox WHERE topic = 'match.finished' AND payload->>'match_id' = $1",
    )
    .bind(match_id)
    .fetch_one(pool)
    .await
    .unwrap();
    n
}

/// THE ATOMIC EMIT PROOF: `report` writes BOTH a `match.matches` row AND an
/// `asyncevents.outbox` row (topic `match.finished`) in one tx — proving `emit_tx` rode
/// the domain transaction. The sync `rating.mmr` read happened first (a real `rating`
/// module backs it), so a 200 also proves the sync seam resolved in-process.
#[tokio::test]
async fn report_persists_match_and_outbox_event_atomically() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;

    svc.report("alice".into(), "bob".into()).await.unwrap();

    let (id,): (String,) = sqlx::query_as(
        "SELECT id::text FROM match.matches WHERE winner = 'alice' AND loser = 'bob' ORDER BY at DESC LIMIT 1",
    )
    .fetch_one(&pool)
    .await
    .unwrap();

    assert_eq!(match_count(&pool, &id).await, 1, "match row must exist");
    assert_eq!(
        outbox_count(&pool, &id).await,
        1,
        "outbox row (match.finished) must exist — atomic emit_tx"
    );

    cleanup(&pool, &id).await;
}

/// A process module set WITHOUT `rating` must fail `validate_requires` — match declares
/// `requires(["rating"])`, and the missing sync dependency fails loud at startup
/// (CLAUDE.md's hard constraint), never a silent nil-service at report time. No DB needed
/// — this is a pure manifest check over the static module list.
#[test]
fn match_requires_rating_and_fails_validate_without_it() {
    // match alone (no rating provider): validate_requires must reject it.
    let mods: Vec<Box<dyn Module>> = vec![Box::new(MatchModule::new())];
    let err = app::validate_requires(&mods).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("rating"),
        "validate_requires should name the missing 'rating' provider, got: {msg}"
    );

    // Adding a rating provider satisfies the manifest.
    let ok: Vec<Box<dyn Module>> = vec![Box::new(MatchModule::new()), Box::new(Rating::new())];
    app::validate_requires(&ok).expect("match's requires are satisfied with rating present");
}
