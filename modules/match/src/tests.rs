//! match tests. Unit tests validate `report`'s durable write shape; the live-Postgres
//! integration tests target the local Postgres (the test DB) and SKIP cleanly when it is
//! unreachable. The `validate_requires` test proves match fails loud without `rating`.
//! In-crate so they can drive the private `Service` directly.

use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

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

/// Migrates BOTH the asyncevents (durable plane's event log) and match schemas EXACTLY
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
    let transport = asyncevents::testing::transport(pool.clone());
    let ctx = Context::with_db_and_transport(pool.clone(), transport.handle());

    let rating = Rating::new();
    rating.register(&ctx).unwrap();
    rating.init(&ctx).unwrap();

    let mm = MatchModule::new();
    mm.register(&ctx).unwrap();
    mm.init(&ctx).unwrap();

    let svc = mm.svc();
    (ctx, svc)
}

async fn service_with_reader(pool: &PgPool, reader: Arc<dyn MmrReader>) -> (Context, Arc<Service>) {
    ensure_schema(pool).await;
    let transport = asyncevents::testing::transport(pool.clone());
    let ctx = Context::with_db_and_transport(pool.clone(), transport.handle());
    let svc = Arc::new(Service {
        pool: pool.clone(),
        bus: ctx.bus().clone(),
        rating: OnceLock::new(),
    });
    assert!(svc.rating.set(reader).is_ok(), "reader set once");
    (ctx, svc)
}

struct CountingReader {
    calls: AtomicUsize,
    failure: Option<Error>,
    first_call_barrier: Option<Arc<tokio::sync::Barrier>>,
}

#[async_trait]
impl MmrReader for CountingReader {
    async fn mmr(&self, _player_id: String) -> Result<i64, Error> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if call < 2 {
            if let Some(barrier) = &self.first_call_barrier {
                barrier.wait().await;
            }
        }
        if let Some(error) = &self.failure {
            Err(error.clone())
        } else {
            Ok(1000)
        }
    }
}

/// A per-call-unique ReportId (nanos-suffixed) — `match.matches` has `UNIQUE (report_id)`
/// and test rows from aborted runs may survive, so a constant id would dedup.
fn rid(tag: &str) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("{tag}-{nanos}")
}

async fn cleanup(pool: &PgPool, match_id: &str) {
    let _ = sqlx::query("DELETE FROM match.matches WHERE id = $1::uuid")
        .bind(match_id)
        .execute(pool)
        .await;
    let _ = asyncevents::testing::cleanup_events(pool, "match_id", match_id).await;
}

async fn match_count(pool: &PgPool, id: &str) -> i64 {
    let (n,): (i64,) = sqlx::query_as("SELECT count(*) FROM match.matches WHERE id = $1::uuid")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap();
    n
}

async fn event_count(pool: &PgPool, match_id: &str) -> i64 {
    asyncevents::testing::events_count(pool, "match.finished", "match_id", match_id)
        .await
        .unwrap()
}

/// THE ATOMIC EMIT PROOF: `report` writes BOTH a `match.matches` row AND an
/// `asyncevents.events` row (topic `match.finished`) in one tx — proving `emit_tx` rode
/// the domain transaction. The sync `rating.mmr` read happened first (a real `rating`
/// module backs it), so a 200 also proves the sync seam resolved in-process.
#[tokio::test]
async fn report_persists_match_and_durable_event_atomically() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;

    let report_id = rid("atomic");
    svc.report(report_id.clone(), "alice".into(), "bob".into()).await.unwrap();

    let (id,): (String,) = sqlx::query_as(
        "SELECT id::text FROM match.matches WHERE report_id = $1",
    )
    .bind(&report_id)
    .fetch_one(&pool)
    .await
    .unwrap();

    assert_eq!(match_count(&pool, &id).await, 1, "match row must exist");
    assert_eq!(
        event_count(&pool, &id).await,
        1,
        "log event (match.finished) must exist — atomic emit_tx"
    );

    cleanup(&pool, &id).await;
}

/// THE IDEMPOTENCY PROOF: two `report`s with the SAME ReportId (the split stub
/// auto-retries a failed RPC, so this happens in production) leave exactly ONE
/// `match.matches` row and exactly ONE `match.finished` event — the duplicate hits
/// `ON CONFLICT (report_id) DO NOTHING`, skips the emit, and still returns Ok.
#[tokio::test]
async fn duplicate_report_id_records_and_emits_once() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;

    let report_id = rid("dup");
    svc.report(report_id.clone(), "carol".into(), "dave".into()).await.unwrap();
    svc.report(report_id.clone(), "carol".into(), "dave".into())
        .await
        .expect("a duplicate report is an Ok no-op, not an error");

    let (rows, id): (i64, String) = {
        let (n,): (i64,) = sqlx::query_as("SELECT count(*) FROM match.matches WHERE report_id = $1")
            .bind(&report_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        let (id,): (String,) =
            sqlx::query_as("SELECT id::text FROM match.matches WHERE report_id = $1")
                .bind(&report_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        (n, id)
    };
    assert_eq!(rows, 1, "duplicate report_id must not insert a second match row");
    assert_eq!(
        event_count(&pool, &id).await,
        1,
        "duplicate report_id must not emit a second match.finished"
    );

    cleanup(&pool, &id).await;
}

#[tokio::test]
async fn duplicate_same_payload_skips_failing_rating_dependency() {
    let Some(pool) = test_pool().await else { return };
    let report_id = rid("replay-no-rating");

    let (_ctx, initial) = wired(&pool).await;
    initial
        .report(report_id.clone(), "alice".into(), "bob".into())
        .await
        .unwrap();

    let reader = Arc::new(CountingReader {
        calls: AtomicUsize::new(0),
        failure: Some(Error::internal("rating unavailable")),
        first_call_barrier: None,
    });
    let (_ctx, replay) = service_with_reader(&pool, reader.clone()).await;
    replay
        .report(report_id.clone(), "alice".into(), "bob".into())
        .await
        .expect("an exact replay must not depend on rating availability");
    assert_eq!(reader.calls.load(Ordering::SeqCst), 0);

    let (id,): (String,) =
        sqlx::query_as("SELECT id::text FROM match.matches WHERE report_id = $1")
            .bind(&report_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(event_count(&pool, &id).await, 1);
    cleanup(&pool, &id).await;
}

#[tokio::test]
async fn duplicate_different_payload_is_conflict_without_rating_call() {
    let Some(pool) = test_pool().await else { return };
    let report_id = rid("payload-conflict");
    let (_ctx, initial) = wired(&pool).await;
    initial
        .report(report_id.clone(), "alice".into(), "bob".into())
        .await
        .unwrap();

    let reader = Arc::new(CountingReader {
        calls: AtomicUsize::new(0),
        failure: Some(Error::internal("must not be called")),
        first_call_barrier: None,
    });
    let (_ctx, replay) = service_with_reader(&pool, reader.clone()).await;
    let err = replay
        .report(report_id.clone(), "mallory".into(), "bob".into())
        .await
        .expect_err("one ReportId cannot name two different matches");
    assert_eq!(err.status, opsapi::Status::Conflict);
    assert_eq!(err.msg, Service::REPORT_ID_CONFLICT);
    assert_eq!(reader.calls.load(Ordering::SeqCst), 0);

    let (winner, loser, id): (String, String, String) = sqlx::query_as(
        "SELECT winner, loser, id::text FROM match.matches WHERE report_id = $1",
    )
    .bind(&report_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!((winner.as_str(), loser.as_str()), ("alice", "bob"));
    assert_eq!(event_count(&pool, &id).await, 1);
    cleanup(&pool, &id).await;
}

#[tokio::test]
async fn concurrent_payload_collision_accepts_exactly_one_payload() {
    let Some(pool) = test_pool().await else { return };
    let report_id = rid("concurrent-conflict");
    let reader = Arc::new(CountingReader {
        calls: AtomicUsize::new(0),
        failure: None,
        first_call_barrier: Some(Arc::new(tokio::sync::Barrier::new(2))),
    });
    let (_ctx, svc) = service_with_reader(&pool, reader).await;

    let left = tokio::spawn({
        let svc = svc.clone();
        let report_id = report_id.clone();
        async move { svc.report(report_id, "alice".into(), "bob".into()).await }
    });
    let right = tokio::spawn({
        let svc = svc.clone();
        let report_id = report_id.clone();
        async move { svc.report(report_id, "carol".into(), "dave".into()).await }
    });
    let results = [left.await.unwrap(), right.await.unwrap()];
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    let conflict = results
        .iter()
        .find_map(|result| result.as_ref().err())
        .expect("the losing payload must conflict");
    assert_eq!(conflict.status, opsapi::Status::Conflict);
    assert_eq!(conflict.msg, Service::REPORT_ID_CONFLICT);

    let (winner, loser, id): (String, String, String) = sqlx::query_as(
        "SELECT winner, loser, id::text FROM match.matches WHERE report_id = $1",
    )
    .bind(&report_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(
        (winner == "alice" && loser == "bob") || (winner == "carol" && loser == "dave")
    );
    assert_eq!(event_count(&pool, &id).await, 1);
    cleanup(&pool, &id).await;
}

/// An empty (or whitespace) ReportId is rejected with `Invalid` — the idempotency key
/// is REQUIRED; a missing key must fail loud, never silently degrade the dedup.
#[tokio::test]
async fn empty_report_id_is_invalid() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;

    for bad in ["", "   "] {
        let err = svc
            .report(bad.into(), "erin".into(), "frank".into())
            .await
            .expect_err("empty ReportId must be rejected");
        assert_eq!(err.status, opsapi::Status::Invalid, "got: {}", err.msg);
    }
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
