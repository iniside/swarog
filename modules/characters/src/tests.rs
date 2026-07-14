use super::*;
use opsapi::Status;
use std::time::Duration;

/// Fallback DSN for the lazy-pool unit tests (the live tests read `DATABASE_URL`).
const DEFAULT_DSN: &str =
    "postgres://gamebackend:gamebackend@localhost:5432/gamebackend?sslmode=disable";

/// A minimal `configapi::Config` returning a fixed `characters/max_per_player` and
/// default-passthrough for every other key — enough to drive `create`'s cap gate.
struct FakeConfig {
    cap: i64,
}
impl Config for FakeConfig {
    fn get_string(&self, _ns: &str, _key: &str, def: &str) -> String {
        def.into()
    }
    fn get_bool(&self, _ns: &str, _key: &str, def: bool) -> bool {
        def
    }
    fn get_int(&self, ns: &str, key: &str, def: i64) -> i64 {
        if ns == "characters" && key == "max_per_player" {
            self.cap
        } else {
            def
        }
    }
    fn get(&self, _ns: &str, _key: &str) -> Option<String> {
        None
    }
}

/// A service over a lazy pool + a transport-less bus — for the validation tests
/// that return BEFORE any DB work or emit. Config is set (cap large) so the struct is
/// complete; the validation tests reject before the cap gate is reached anyway.
fn lazy_service() -> Arc<Service> {
    let svc = Service {
        store: Store {
            pool: PgPool::connect_lazy(DEFAULT_DSN).unwrap(),
        },
        bus: Arc::new(Bus::new()),
        config: OnceLock::new(),
    };
    let _ = svc.config.set(Arc::new(FakeConfig { cap: LIST_HARD_LIMIT }));
    Arc::new(svc)
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
    wired_with_cap(pool, LIST_HARD_LIMIT).await
}

/// Like [`wired`] but provides a `FakeConfig` returning `cap` for
/// `characters/max_per_player` under `config.reader` BEFORE `init` — so `init`'s real
/// `require::<dyn Config>` resolves it and `create`'s cap gate reads exactly `cap`.
/// Exercises the true init wiring (no direct private-field poke).
async fn wired_with_cap(pool: &PgPool, cap: i64) -> (Context, Arc<Service>) {
    ensure_schema(pool).await;
    let transport = asyncevents::testing::transport(pool.clone());
    let ctx = Context::with_db_and_transport(pool.clone(), transport.handle());
    ctx.registry()
        .provide::<dyn Config>(key("config", "reader"), Arc::new(FakeConfig { cap }));

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

/// THE ATOMIC EMIT PROOF, FAILURE DIRECTION (the branch the success test can't
/// reach): the durable append inside `create`'s tx FAILS, so the whole tx must roll
/// back — no `characters.characters` row survives. Failure is injected at the
/// TRANSPORT seam via `asyncevents::testing::failing_transport()` (its `enqueue_tx`
/// returns `Err`), landing exactly where `create` calls `bus.emit_tx(AnyTx::new(&mut
/// *tx), …)` AFTER the domain INSERT — so the error early-returns and `tx` drops
/// without commit → implicit ROLLBACK. This pins the emit_tx atomicity contract from
/// the other side: the character is durable IFF its event is. It would go RED if the
/// INSERT were ever committed before (or independently of) the emit — e.g. moving
/// `tx.commit()` above the `emit_tx` call, or writing the row on a separate pool
/// connection outside the tx: then the row would persist despite the append failing
/// and `char_count_by_player` would be 1, not 0.
///
/// Each test builds its OWN ctx over its OWN transport handle (this one gets the
/// failing handle, the positive test gets the real appending one), so the injected
/// failure cannot leak across tests — no shared DB state is poisoned.
///
/// SCOPE: inventory gets NO analogous emitter-atomicity test — it never emits its own
/// events (it is a durable CONSUMER: `on_tx` grant_starter/wipe_character, and there
/// is no `api/inventory/events`), so emitter-side emit_tx rollback does not apply; its
/// atomicity is the different handler-write + checkpoint shape, out of scope here.
#[tokio::test]
async fn create_rolls_back_domain_row_when_durable_append_fails() {
    let Some(pool) = test_pool().await else { return };
    ensure_schema(&pool).await;

    // Same wiring as `wired_with_cap`, but the transport's append always errors.
    let ctx = Context::with_db_and_transport(pool.clone(), asyncevents::testing::failing_transport());
    ctx.registry()
        .provide::<dyn Config>(key("config", "reader"), Arc::new(FakeConfig { cap: LIST_HARD_LIMIT }));
    let chars = Characters::new();
    chars.register(&ctx).unwrap();
    chars.init(&ctx).unwrap();
    let svc = chars.svc();

    let pid = unique_player(&pool).await;

    let e = svc
        .create(Identity::player(&pid), "Somebody".into(), String::new())
        .await
        .expect_err("emit_tx append failed, so create must return Err");
    assert_eq!(e.status, Status::Internal, "a durable-append failure surfaces as Internal");

    assert_eq!(
        char_count_by_player(&pool, &pid).await,
        0,
        "the failed durable append must roll back the domain INSERT — no character row survives"
    );

    cleanup(&pool, &[&pid]).await;
}

/// THE CAP PROOF (sequential failing branch): with `characters/max_per_player` = 3,
/// a player's first 3 creates succeed and the 4th is rejected `Conflict` (409). A
/// DIFFERENT player is unaffected — the cap is PER-PLAYER. Pre-fix the 4th succeeded
/// (no cap existed at all).
#[tokio::test]
async fn create_enforces_per_player_cap() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired_with_cap(&pool, 3).await;
    let pid = unique_player(&pool).await;
    let other = unique_player(&pool).await;

    for i in 0..3 {
        svc.create(Identity::player(&pid), format!("Hero{i}"), String::new())
            .await
            .unwrap_or_else(|e| panic!("create {i} under the cap must succeed, got {e:?}"));
    }
    assert_eq!(char_count_by_player(&pool, &pid).await, 3);

    // The 4th crosses the cap → Conflict (409), NOT a 500 or a silent extra row.
    let e = svc
        .create(Identity::player(&pid), "Overflow".into(), String::new())
        .await
        .unwrap_err();
    assert_eq!(e.status, Status::Conflict, "creating past the cap must be a 409 Conflict");
    assert_eq!(
        char_count_by_player(&pool, &pid).await,
        3,
        "the rejected create must have rolled back — no row past the cap"
    );

    // The cap is per-player: a different player can still create.
    svc.create(Identity::player(&other), "Fresh".into(), String::new())
        .await
        .expect("a different player is unaffected by the first player's cap");
    assert_eq!(char_count_by_player(&pool, &other).await, 1);

    cleanup(&pool, &[&pid, &other]).await;
}

/// THE FLOOR PROOF: `characters/max_per_player = 0` means "freeze creation", NOT the
/// clamped-to-1 surprise. With cap = 0 the gate `n >= cap` rejects even the FIRST
/// create (0 >= 0), so no character is ever admitted. Pins the clamp's lower bound at
/// 0 (fail-closed) and executes the n==0 branch.
#[tokio::test]
async fn create_with_zero_cap_rejects_first() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired_with_cap(&pool, 0).await;
    let pid = unique_player(&pool).await;

    let e = svc
        .create(Identity::player(&pid), "Nobody".into(), String::new())
        .await
        .unwrap_err();
    assert_eq!(e.status, Status::Conflict, "cap=0 must freeze creation — the first create is a 409");
    assert_eq!(
        char_count_by_player(&pool, &pid).await,
        0,
        "cap=0 must admit zero rows (no clamp-to-1 floor)"
    );

    cleanup(&pool, &[&pid]).await;
}

/// THE ADVISORY-LOCK PROOF (race failing branch): with cap = 1, two creates for the
/// SAME player race concurrently. The per-player transaction-scoped advisory lock in
/// `create` serializes their count-then-insert, so EXACTLY ONE succeeds and the other
/// is rejected `Conflict`, leaving exactly one row. WITHOUT the lock both would SELECT
/// `count` == 0 under READ COMMITTED (neither tx committed yet) and both insert — two
/// rows over the cap=1. Run on the multi-thread runtime so the two spawned tasks can
/// truly overlap on separate connections. The post-fix outcome (one ok, one conflict,
/// one row) is deterministic regardless of which task wins the lock.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_creates_respect_cap_under_advisory_lock() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired_with_cap(&pool, 1).await;
    let pid = unique_player(&pool).await;

    let a = tokio::spawn({
        let (svc, pid) = (svc.clone(), pid.clone());
        async move { svc.create(Identity::player(&pid), "Elrond".into(), String::new()).await }
    });
    let b = tokio::spawn({
        let (svc, pid) = (svc.clone(), pid.clone());
        async move { svc.create(Identity::player(&pid), "Arwen".into(), String::new()).await }
    });
    let ra = a.await.unwrap();
    let rb = b.await.unwrap();

    let oks = [&ra, &rb].iter().filter(|r| r.is_ok()).count();
    let conflicts = [&ra, &rb]
        .iter()
        .filter(|r| matches!(r, Err(e) if e.status == Status::Conflict))
        .count();
    assert_eq!(oks, 1, "exactly one concurrent create may pass the cap=1 gate");
    assert_eq!(
        conflicts, 1,
        "the other concurrent create must be rejected Conflict under the advisory lock"
    );
    assert_eq!(
        char_count_by_player(&pool, &pid).await,
        1,
        "exactly one row — the per-player advisory lock prevented a double-admit"
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

// ---- Admin extension points (Step 4) ---------------------------------------
// Pure-view tests over the admin builders — no DB (the malformed-owner path
// returns error-content BEFORE any store call).

/// A real `Character` fixture with only the fields the schema actually has (no
/// invented level/power).
fn admin_char(id: &str, name: &str, class: &str) -> charactersapi::Character {
    charactersapi::Character {
        id: id.into(),
        player_id: "00000000-0000-0000-0000-0000000000aa".into(),
        name: name.into(),
        class: class.into(),
        created_at: "Jan 01, 00:00".into(),
    }
}

const ADMIN_UUID: &str = "b3f1a2c4-1111-2222-3333-444455556666";

#[test]
fn admin_extension_entry_targets_players_row_menu() {
    // The ONE shared vec both the local Item and admin_data's ItemData carry.
    let ents = crate::admin::extension_entries();
    assert_eq!(ents.len(), 1);
    let e = &ents[0];
    assert_eq!(e.point, accountsapi::admin::PLAYERS_ROW_MENU.id);
    assert_eq!(e.label, "View Characters");
    assert_eq!(e.present, adminapi::Present::Navigate);
    assert_eq!(e.link, "characters?owner={id}");
}

#[test]
fn admin_player_scoped_render_has_header_and_cards() {
    let chars = [
        admin_char(ADMIN_UUID, "VoidR4nger", "Warlock"),
        admin_char("cccccccc-0000-0000-0000-000000000001", "Aria", "Ranger"),
    ];
    let content = crate::admin::build_player_scoped(ADMIN_UUID, &chars);

    let header = content.header.expect("scoped view carries a context header");
    // characters doesn't know account names → the short uuid form is the title.
    assert_eq!(header.title, "b3f1a2c4");
    assert_eq!(header.subtitle_mono, format!("player:{ADMIN_UUID}"));

    let grid = content.cards.expect("scoped view is a card grid");
    assert_eq!(grid.menu_point, charactersapi::admin::CHARACTERS_CARD_MENU.id);
    assert_eq!(grid.cards.len(), 2);

    let card = &grid.cards[0];
    assert_eq!(card.title, "VoidR4nger");
    assert_eq!(card.subtitle, "Warlock"); // REAL class only (no level column)
    assert_eq!(
        card.context.get("id").map(String::as_str),
        Some(format!("character:{ADMIN_UUID}").as_str())
    );
    // Native card menu: View (Modal) + inert Edit/Delete.
    assert_eq!(card.menu[0].label, "View");
    assert_eq!(card.menu[0].present, adminapi::Present::Modal);
    assert_eq!(card.menu[0].link.as_deref(), Some("characters?owner=character:{id}"));
    assert!(card.menu[1].disabled, "Edit inert");
    assert!(card.menu[2].disabled && card.menu[2].danger, "Delete inert + danger");
}

#[test]
fn admin_character_detail_sets_modal_point_and_context() {
    let c = admin_char(ADMIN_UUID, "VoidR4nger", "Warlock");
    let content = crate::admin::build_character_detail(&c);

    assert_eq!(content.modal_point, charactersapi::admin::CHARACTER_MODAL_ACTIONS.id);
    assert_eq!(
        content.context.get("id").map(String::as_str),
        Some(format!("character:{ADMIN_UUID}").as_str())
    );
    assert_eq!(content.header.expect("detail header").title, "VoidR4nger");
    // KPI stats from REAL fields only.
    assert!(content.kpis.iter().any(|k| k.label == "Class" && k.value == "Warlock"));
}

#[tokio::test]
async fn admin_malformed_owner_yields_error_content_not_err() {
    // Both a bad-uuid owner and a foreign owner shape must render error-content, never
    // Err (foreign-params tolerance) — resolved BEFORE any store I/O, so a lazy pool is
    // never dialled.
    let svc = lazy_service();
    for owner in ["player:not-a-uuid", "character:zzz", "something-else"] {
        let mut params = adminapi::Params::new();
        params.insert("owner".into(), owner.into());
        let content = crate::admin::admin_content(&svc.store, &params)
            .await
            .expect("never Err on a foreign/malformed owner");
        assert_eq!(content.kpis[0].label, "Error", "owner={owner}");
        assert!(content.cards.is_none());
    }
}
