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

#[test]
fn character_text_caps_count_utf8_bytes_at_boundary_and_one_over() {
    let name_boundary = "é".repeat(MAX_NAME_BYTES / 2);
    let name_over = format!("{name_boundary}a");
    assert_eq!(name_boundary.len(), MAX_NAME_BYTES);
    assert_eq!(name_over.len(), MAX_NAME_BYTES + 1);
    assert!(name_within_cap(&name_boundary));
    assert!(!name_within_cap(&name_over));

    let class_boundary = "é".repeat(MAX_CLASS_BYTES / 2);
    let class_over = format!("{class_boundary}a");
    assert_eq!(class_boundary.len(), MAX_CLASS_BYTES);
    assert_eq!(class_over.len(), MAX_CLASS_BYTES + 1);
    assert!(class_within_cap(&class_boundary));
    assert!(!class_within_cap(&class_over));
}

/// Both caps reject before `pool.begin()`; the lazy service needs no reachable DB.
#[tokio::test]
async fn create_rejects_over_cap_name_and_class_before_db() {
    let svc = lazy_service();

    let over_name = format!("{}a", "é".repeat(MAX_NAME_BYTES / 2));
    let e = svc
        .create(Identity::player("p1"), over_name, String::new())
        .await
        .unwrap_err();
    assert_eq!(e.status, Status::Invalid);

    let over_class = format!("{}a", "é".repeat(MAX_CLASS_BYTES / 2));
    let e = svc
        .create(Identity::player("p1"), "Aragorn".into(), over_class)
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

/// Migrates BOTH the asyncevents (durable plane's event log) and characters schemas
/// EXACTLY ONCE per test binary. Concurrent `CREATE INDEX`/`CREATE OR REPLACE
/// TRIGGER` across parallel tests take catalog locks that cycle into a Postgres
/// deadlock, so the idempotent DDL must be serialized to a single run.
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
            let c = Characters::new();
            c.register(&ctx).unwrap();
            c.migrate(&ctx).await.unwrap();
        })
        .await;
}

/// Builds a real durable plane over the live pool: schemas are migrated once
/// (`ensure_schema`), then the asyncevents `bus::Transport` is injected at `Context`
/// construction (needed before any `emit_tx`), and characters registers/inits against
/// the same ctx. Returns the ctx (owns the bus + registry) and the wired service.
async fn wired(pool: &PgPool) -> (Context, Arc<Service>) {
    ensure_schema(pool).await;
    let transport = asyncevents::testing::transport(pool.clone());
    let ctx = Context::with_db_and_transport(pool.clone(), transport.handle());

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
        let _ = asyncevents::testing::cleanup_events(pool, "player_id", pid).await;
    }
}

async fn event_count(pool: &PgPool, topic: &str, character_id: &str) -> i64 {
    asyncevents::testing::events_count(pool, topic, "character_id", character_id)
        .await
        .unwrap()
}

async fn events_count_by_player(pool: &PgPool, topic: &str, player_id: &str) -> i64 {
    asyncevents::testing::events_count(pool, topic, "player_id", player_id)
        .await
        .unwrap()
}

async fn char_count(pool: &PgPool, id: &str) -> i64 {
    let (n,): (i64,) = sqlx::query_as("SELECT count(*) FROM characters.characters WHERE id = $1::uuid")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap();
    n
}

async fn char_count_by_player(pool: &PgPool, player_id: &str) -> i64 {
    let (n,): (i64,) =
        sqlx::query_as("SELECT count(*) FROM characters.characters WHERE player_id = $1::uuid")
            .bind(player_id)
            .fetch_one(pool)
            .await
            .unwrap();
    n
}

/// THE ATOMIC EMIT PROOF: create writes BOTH a `characters.characters` row AND an
/// `asyncevents.events` row (topic `character.created`) in one tx — proving
/// `emit_tx` rode the domain transaction. Also proves the class default.
#[tokio::test]
async fn create_persists_character_and_durable_event_atomically() {
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
        event_count(&pool, "character.created", &c.id).await,
        1,
        "log event (character.created) must exist — atomic emit_tx"
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
    assert_eq!(event_count(&pool, "character.deleted", &c.id).await, 0);
    assert_eq!(char_count(&pool, &c.id).await, 1);

    // Owned delete: succeeds, emits the event, row gone.
    svc.delete(Identity::player(&owner), c.id.clone())
        .await
        .unwrap();
    assert_eq!(event_count(&pool, "character.deleted", &c.id).await, 1);
    assert_eq!(char_count(&pool, &c.id).await, 0);

    cleanup(&pool, &[&owner, &other]).await;
}

/// A delete issued with NON-canonical spellings of BOTH the character id and the
/// identity's player_id (braces — `{uuid}`, which Postgres accepts and normalises via
/// `::uuid`) still deletes, but the emitted `character.deleted` must carry the
/// DB-canonical (lowercase, unbraced) values from `RETURNING id::text, player_id::text`
/// — NOT the client echo. Pre-fix the event carried the raw argument, diverging from
/// the canonical `character.created` and breaking inventory's lock_key + audit
/// consistency. Braces are a GUARANTEED non-canonical form (unlike `to_uppercase`,
/// which is a no-op on an all-numeric-hex uuid), so the divergence is deterministic.
#[tokio::test]
async fn delete_emits_canonical_ids_for_noncanonical_input() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let owner = unique_player(&pool).await;

    let c = svc
        .create(Identity::player(&owner), "Gimli".into(), "warrior".into())
        .await
        .unwrap();

    // Delete with the braced id AND a braced player_id in the Identity — Postgres
    // normalises both via `::uuid`, so the row still matches and the delete succeeds.
    let braced_id = format!("{{{}}}", c.id);
    let braced_player = format!("{{{}}}", owner);
    svc.delete(Identity::player(&braced_player), braced_id.clone())
        .await
        .unwrap();

    // The event carries the canonical (unbraced) character_id, NOT the braced echo.
    assert_eq!(
        event_count(&pool, "character.deleted", &c.id).await,
        1,
        "character.deleted must carry the canonical (RETURNING) character_id"
    );
    assert_eq!(
        event_count(&pool, "character.deleted", &braced_id).await,
        0,
        "character.deleted must NOT carry the raw braced character_id echo (fails pre-fix)"
    );

    // The event carries the canonical (unbraced) player_id too — the sibling branch.
    assert_eq!(
        events_count_by_player(&pool, "character.deleted", &c.player_id).await,
        1,
        "character.deleted must carry the canonical (RETURNING) player_id"
    );
    assert_eq!(
        events_count_by_player(&pool, "character.deleted", &braced_player).await,
        0,
        "character.deleted must NOT carry the raw braced player_id echo (fails pre-fix)"
    );

    assert_eq!(char_count(&pool, &c.id).await, 0, "row must be gone");

    cleanup(&pool, &[&owner]).await;
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

/// list is capped by `LIST_HARD_LIMIT` — the safety belt, not the per-player create
/// cap: a player with more rows than the ceiling (bulk-inserted directly, bypassing
/// `create`, to prove the belt fires regardless of how the rows got there) still
/// gets a bounded response instead of an unbounded `fetch_all`.
#[tokio::test]
async fn list_is_capped_at_hard_limit() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let pid = unique_player(&pool).await;

    let over = LIST_HARD_LIMIT + 5;
    sqlx::query(
        "INSERT INTO characters.characters (player_id, name, class) \
         SELECT $1::uuid, 'n' || g, 'novice' FROM generate_series(1, $2) g",
    )
    .bind(&pid)
    .bind(over)
    .execute(&pool)
    .await
    .unwrap();

    assert_eq!(char_count_by_player(&pool, &pid).await, over);

    let mine = svc.list(Identity::player(&pid)).await.unwrap();
    assert_eq!(mine.len(), LIST_HARD_LIMIT as usize);

    cleanup(&pool, &[&pid]).await;
}
