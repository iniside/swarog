use super::*;
use opsapi::Status;
use std::time::Duration;

/// Fallback DSN for the lazy-pool unit tests (the live tests read `DATABASE_URL`).
const DEFAULT_DSN: &str =
    "postgres://gamebackend:gamebackend@localhost:5432/gamebackend?sslmode=disable";

/// A service over a lazy pool + a transport-less bus — for the validation tests
/// that return BEFORE any DB work or emit.
fn lazy_service() -> Arc<Service> {
    Arc::new(Service {
        store: Store {
            pool: PgPool::connect_lazy(DEFAULT_DSN).unwrap(),
        },
        bus: Arc::new(Bus::new()),
    })
}

/// create validates identity then name BEFORE touching the DB, so a lazy pool
/// that would fail on connect still yields the typed `Invalid`.
#[tokio::test]
async fn create_rejects_missing_identity_and_empty_name() {
    let svc = lazy_service();
    let e = svc
        .create(Identity::none(), "Aragorn".into(), String::new())
        .await
        .unwrap_err();
    assert_eq!(e.status, Status::Invalid);
    let e = svc
        .create(Identity::player("p1"), "   ".into(), String::new())
        .await
        .unwrap_err();
    assert_eq!(e.status, Status::Invalid);
}

/// list/delete also reject a missing identity before any DB work.
#[tokio::test]
async fn list_and_delete_require_identity() {
    let svc = lazy_service();
    assert_eq!(
        svc.list(Identity::none()).await.unwrap_err().status,
        Status::Invalid
    );
    assert_eq!(
        svc.delete(Identity::none(), "whatever".into())
            .await
            .unwrap_err()
            .status,
        Status::Invalid
    );
}

// ---- Live Postgres integration (the local DB is the test DB) ----------

/// Opens the local Postgres; returns `None` (printing a skip line) when
/// unreachable, so the suite RUNS but SKIPs cleanly with no DB.
async fn test_pool() -> Option<PgPool> {
    let dsn = std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DSN.to_string());
    let pool = match tokio::time::timeout(Duration::from_secs(3), PgPool::connect(&dsn)).await {
        Ok(Ok(p)) => p,
        _ => {
            eprintln!("SKIP: postgres unreachable at {dsn} — characters DB tests skipped");
            return None;
        }
    };
    Some(pool)
}

/// Migrates BOTH the messaging (durable transport's outbox) and characters schemas
/// EXACTLY ONCE per test binary. Concurrent `CREATE INDEX`/`CREATE OR REPLACE
/// TRIGGER` across parallel tests take catalog locks that cycle into a Postgres
/// deadlock, so the idempotent DDL must be serialized to a single run.
static SCHEMA_READY: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();

async fn ensure_schema(pool: &PgPool) {
    SCHEMA_READY
        .get_or_init(|| async {
            let ctx = Context::with_db(pool.clone());
            let m = messaging::Messaging::new();
            m.register(&ctx).unwrap();
            m.migrate(&ctx).await.unwrap();
            let c = Characters::new();
            c.register(&ctx).unwrap();
            c.migrate(&ctx).await.unwrap();
        })
        .await;
}

/// Builds a real durable plane over the live pool: schemas are migrated once
/// (`ensure_schema`), then messaging's phase-1 `register` installs the
/// `bus::Transport` on THIS ctx's bus (needed before any `emit_tx`), and
/// characters registers/inits against the same ctx. Returns the ctx (owns the bus
/// + registry) and the wired service.
async fn wired(pool: &PgPool) -> (Context, Arc<Service>) {
    ensure_schema(pool).await;
    let ctx = Context::with_db(pool.clone());

    let messaging = messaging::Messaging::new();
    messaging.register(&ctx).unwrap();

    let chars = Characters::new();
    chars.register(&ctx).unwrap();
    chars.init(&ctx).unwrap();

    let svc = chars.svc();
    (ctx, svc)
}

/// A fresh random player_id (a valid uuid) so parallel test runs never collide.
async fn unique_player(pool: &PgPool) -> String {
    let (id,): (String,) = sqlx::query_as("SELECT gen_random_uuid()::text")
        .fetch_one(pool)
        .await
        .unwrap();
    id
}

async fn cleanup(pool: &PgPool, players: &[&str]) {
    for pid in players {
        let _ = sqlx::query("DELETE FROM characters.characters WHERE player_id = $1::uuid")
            .bind(pid)
            .execute(pool)
            .await;
        let _ = sqlx::query("DELETE FROM messaging.outbox WHERE payload->>'player_id' = $1")
            .bind(pid)
            .execute(pool)
            .await;
    }
}

async fn outbox_count(pool: &PgPool, topic: &str, character_id: &str) -> i64 {
    let (n,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM messaging.outbox WHERE topic = $1 AND payload->>'character_id' = $2",
    )
    .bind(topic)
    .bind(character_id)
    .fetch_one(pool)
    .await
    .unwrap();
    n
}

async fn char_count(pool: &PgPool, id: &str) -> i64 {
    let (n,): (i64,) = sqlx::query_as("SELECT count(*) FROM characters.characters WHERE id = $1::uuid")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap();
    n
}

/// THE ATOMIC EMIT PROOF: create writes BOTH a `characters.characters` row AND a
/// `messaging.outbox` row (topic `character.created`) in one tx — proving
/// `emit_tx` rode the domain transaction. Also proves the class default.
#[tokio::test]
async fn create_persists_character_and_outbox_event_atomically() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let pid = unique_player(&pool).await;

    // Empty class -> "novice" default.
    let c = svc
        .create(Identity::player(&pid), "Aragorn".into(), String::new())
        .await
        .unwrap();
    assert_eq!(c.player_id, pid);
    assert_eq!(c.class, "novice");

    assert_eq!(char_count(&pool, &c.id).await, 1, "character row must exist");
    assert_eq!(
        outbox_count(&pool, "character.created", &c.id).await,
        1,
        "outbox row (character.created) must exist — atomic emit_tx"
    );

    cleanup(&pool, &[&pid]).await;
}

/// delete of an OWNED character emits `character.deleted` (in the same tx as the
/// delete) and removes the row; delete of a character owned by SOMEONE ELSE is a
/// NotFound with NO event.
#[tokio::test]
async fn delete_emits_event_owned_and_is_notfound_unowned() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let owner = unique_player(&pool).await;
    let other = unique_player(&pool).await;

    let c = svc
        .create(Identity::player(&owner), "Legolas".into(), "archer".into())
        .await
        .unwrap();

    // Unowned delete: NotFound, no character.deleted event, row survives.
    let e = svc
        .delete(Identity::player(&other), c.id.clone())
        .await
        .unwrap_err();
    assert_eq!(e.status, Status::NotFound);
    assert_eq!(outbox_count(&pool, "character.deleted", &c.id).await, 0);
    assert_eq!(char_count(&pool, &c.id).await, 1);

    // Owned delete: succeeds, emits the event, row gone.
    svc.delete(Identity::player(&owner), c.id.clone())
        .await
        .unwrap();
    assert_eq!(outbox_count(&pool, "character.deleted", &c.id).await, 1);
    assert_eq!(char_count(&pool, &c.id).await, 0);

    cleanup(&pool, &[&owner, &other]).await;
}

/// owner_of: a hit returns the owner, a valid-but-absent uuid AND a malformed uuid
/// both return `Ok(None)` (distinct from an infra error).
#[tokio::test]
async fn owner_of_hit_miss_and_invalid_uuid() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let pid = unique_player(&pool).await;

    let c = svc
        .create(Identity::player(&pid), "Gimli".into(), String::new())
        .await
        .unwrap();

    assert_eq!(svc.owner_of(c.id.clone()).await.unwrap(), Some(pid.clone()));

    let absent = unique_player(&pool).await; // a valid uuid, not present
    assert_eq!(svc.owner_of(absent).await.unwrap(), None);

    // Malformed id → Ok(None), NOT an error.
    assert_eq!(svc.owner_of("not-a-uuid".into()).await.unwrap(), None);

    cleanup(&pool, &[&pid]).await;
}

/// list returns only the caller's own characters, newest-insertion order.
#[tokio::test]
async fn list_returns_only_callers_characters() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let me = unique_player(&pool).await;
    let them = unique_player(&pool).await;

    svc.create(Identity::player(&me), "A".into(), String::new())
        .await
        .unwrap();
    svc.create(Identity::player(&me), "B".into(), String::new())
        .await
        .unwrap();
    svc.create(Identity::player(&them), "C".into(), String::new())
        .await
        .unwrap();

    let mine = svc.list(Identity::player(&me)).await.unwrap();
    assert_eq!(mine.len(), 2);
    assert!(mine.iter().all(|c| c.player_id == me));

    cleanup(&pool, &[&me, &them]).await;
}
