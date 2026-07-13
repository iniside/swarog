use super::*;
use bus::AnyTx;
use opsapi::Status;
use std::sync::Mutex;
use std::time::Duration;

const DEFAULT_DSN: &str =
    "postgres://gamebackend:gamebackend@localhost:5432/gamebackend?sslmode=disable";

// ---- Fakes -------------------------------------------------------------

/// A configurable `Ownership` double: returns an error, a miss, or a given owner.
enum FakeOwnership {
    Fail,
    Miss,
    Owner(String),
}
#[async_trait]
impl Ownership for FakeOwnership {
    async fn owner_of(&self, _character_id: String) -> Result<Option<String>, Error> {
        match self {
            FakeOwnership::Fail => Err(Error::unavailable("boom")),
            FakeOwnership::Miss => Ok(None),
            FakeOwnership::Owner(p) => Ok(Some(p.clone())),
        }
    }
}

/// A MUTABLE `Config` double: the two starter keys read from interior-mutable
/// cells so a test can change them and re-fire `on_config_changed`.
struct FakeConfig {
    item: Mutex<String>,
    qty: Mutex<i64>,
}
impl FakeConfig {
    fn new(item: &str, qty: i64) -> FakeConfig {
        FakeConfig { item: Mutex::new(item.into()), qty: Mutex::new(qty) }
    }
}
impl Config for FakeConfig {
    fn get_string(&self, ns: &str, key: &str, def: &str) -> String {
        if ns == "inventory" && key == "starter_item" {
            self.item.lock().unwrap().clone()
        } else {
            def.into()
        }
    }
    fn get_bool(&self, _ns: &str, _key: &str, def: bool) -> bool {
        def
    }
    fn get_int(&self, ns: &str, key: &str, def: i64) -> i64 {
        if ns == "inventory" && key == "starter_qty" {
            *self.qty.lock().unwrap()
        } else {
            def
        }
    }
    fn get(&self, _ns: &str, _key: &str) -> Option<String> {
        None
    }
}

/// Builds an `Inner` over a pool with a fake config injected (starter spec
/// resolvable). Ownership is left unset unless the caller sets it.
fn inner_with(pool: PgPool, cfg: Arc<dyn Config>) -> Arc<Inner> {
    let inner = Arc::new(Inner {
        store: Store { pool },
        dev_grant: true, // the fixture default; the gate tests below build their own
        ownership: OnceLock::new(),
        cfg: OnceLock::new(),
    });
    let _ = inner.cfg.set(cfg);
    inner
}

/// An `Inner` with the dev-grant gate forced on/off over a LAZY pool (mirrors
/// accounts' `tests/dev_auth_gate.rs::gated_service`). The gate rejects `grant`
/// BEFORE any DB access, so the reject-path tests need no live DB.
fn gated_inner(dev_grant: bool) -> Arc<Inner> {
    Arc::new(Inner {
        store: Store {
            pool: PgPool::connect_lazy(DEFAULT_DSN).unwrap(),
        },
        dev_grant,
        ownership: OnceLock::new(),
        cfg: OnceLock::new(),
    })
}

// ---- No-DB unit tests --------------------------------------------------

/// `starter_spec` reads straight off the injected config reader (no inventory-owned
/// second cache, Step 8):
/// changing the fake's cells is visible on the VERY NEXT call, with no refresh step.
#[tokio::test]
async fn starter_spec_reads_config_directly_every_call() {
    let pool = PgPool::connect_lazy(DEFAULT_DSN).unwrap(); // never queried
    let cfg = Arc::new(FakeConfig::new(STARTER_ITEM, STARTER_QTY));
    let inner = inner_with(pool, cfg.clone());

    assert_eq!(inner.starter_spec(), (STARTER_ITEM.to_string(), 1));

    *cfg.item.lock().unwrap() = "health_potion".into();
    *cfg.qty.lock().unwrap() = 5;
    assert_eq!(
        inner.starter_spec(),
        ("health_potion".to_string(), 5),
        "no inventory-owned second cache — the very next read sees the new config values"
    );
}

/// dev grant OFF → `Holdings::grant` is withheld at the service level with NotFound
/// BEFORE any input handling or DB access (the pool is lazy and never queried) — the
/// single impl-side authority every exposure path traverses now that the op is
/// contributed unconditionally in both topologies.
#[tokio::test]
async fn dev_grant_off_withholds_grant_before_any_db_touch() {
    let inner = gated_inner(false);

    let e = inner
        .grant(Identity::player("p1"), "coin".into(), 1)
        .await
        .unwrap_err();
    assert_eq!(
        e.status,
        Status::NotFound,
        "grant must be withheld at the impl when INVENTORY_DEV_GRANT is off"
    );

    // The gate fires FIRST: even a request that would otherwise fail validation
    // (missing identity, non-positive qty) answers NotFound, proving no input
    // handling or store access precedes the guard.
    let e = inner.grant(Identity::none(), "coin".into(), 0).await.unwrap_err();
    assert_eq!(
        e.status,
        Status::NotFound,
        "the gate must reject before identity/qty validation"
    );
}

/// dev grant ON → the gate is open, so `grant` reaches its normal handling: a
/// non-positive qty surfaces as Invalid (validation), NOT NotFound (the gate).
/// Proves the guard only fires when the gate is off. DB-free — the qty check
/// precedes the item-existence lookup.
#[tokio::test]
async fn dev_grant_on_lets_grant_reach_validation() {
    let inner = gated_inner(true);

    let e = inner
        .grant(Identity::player("p1"), "coin".into(), 0)
        .await
        .unwrap_err();
    assert_eq!(
        e.status,
        Status::Invalid,
        "gate open: grant must reach validation (Invalid), not the gate (NotFound)"
    );
}

// ---- Live Postgres integration ----------------------------------------

async fn test_pool() -> Option<PgPool> {
    let dsn = std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DSN.to_string());
    let pool = match tokio::time::timeout(Duration::from_secs(3), PgPool::connect(&dsn)).await {
        Ok(Ok(p)) => p,
        _ => {
            eprintln!("SKIP: postgres unreachable at {dsn} — inventory DB tests skipped");
            return None;
        }
    };
    Some(pool)
}

/// Migrates asyncevents (durable plane's event log) + inventory schemas EXACTLY
/// ONCE per test binary — concurrent idempotent DDL across parallel tests can
/// deadlock on catalog locks, so serialize to a single run.
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
            let inv = Inventory::new();
            inv.register(&ctx).unwrap();
            inv.migrate(&ctx).await.unwrap();
        })
        .await;
}

async fn unique_uuid(pool: &PgPool) -> String {
    let (id,): (String,) = sqlx::query_as("SELECT gen_random_uuid()::text")
        .fetch_one(pool)
        .await
        .unwrap();
    id
}

async fn cleanup_owner(pool: &PgPool, owner_id: &str) {
    let _ = sqlx::query("DELETE FROM inventory.holdings WHERE owner_id = $1::uuid")
        .bind(owner_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM inventory.wiped_characters WHERE character_id = $1::uuid")
        .bind(owner_id)
        .execute(pool)
        .await;
}

async fn tombstone_exists(pool: &PgPool, character_id: &str) -> bool {
    sqlx::query_scalar::<_, i32>("SELECT 1 FROM inventory.wiped_characters WHERE character_id = $1::uuid")
        .bind(character_id)
        .fetch_optional(pool)
        .await
        .unwrap()
        .is_some()
}

/// (a) grant_starter on a handed conn creates the holding; wipe_character clears it.
#[tokio::test]
async fn grant_starter_then_wipe_on_conn() {
    let Some(pool) = test_pool().await else { return };
    ensure_schema(&pool).await;
    let cid = unique_uuid(&pool).await;
    let inner = inner_with(pool.clone(), Arc::new(FakeConfig::new(STARTER_ITEM, STARTER_QTY)));

    let mut conn = pool.acquire().await.unwrap();
    inner.grant_starter(&mut conn, &cid).await.unwrap();

    let holdings = inner.store.list(&Owner::character(&cid)).await.unwrap();
    assert_eq!(holdings.len(), 1, "starter holding must exist");
    assert_eq!(holdings[0].item_id, "starter_sword");
    assert_eq!(holdings[0].quantity, 1);

    inner.wipe_character(&mut conn, &cid).await.unwrap();
    let holdings = inner.store.list(&Owner::character(&cid)).await.unwrap();
    assert!(holdings.is_empty(), "wipe must clear all holdings");
    assert!(tombstone_exists(&pool, &cid).await, "wipe must plant a tombstone");

    // A grant REDELIVERED (or reordered) after the wipe is skipped: the tombstone
    // is permanent truth — no holdings resurrect for a dead character.
    inner.grant_starter(&mut conn, &cid).await.unwrap();
    let holdings = inner.store.list(&Owner::character(&cid)).await.unwrap();
    assert!(holdings.is_empty(), "grant after wipe must be skipped by the tombstone");

    cleanup_owner(&pool, &cid).await;
}

/// (a2) Config-validation guard: an UNKNOWN configured starter item degrades to the
/// compiled default (`STARTER_ITEM`) instead of failing the delivery — a config typo
/// must never poison the `inventory.character-created.v1` subscription.
#[tokio::test]
async fn grant_starter_falls_back_to_default_item_on_unknown_config_item() {
    let Some(pool) = test_pool().await else { return };
    ensure_schema(&pool).await;
    let cid = unique_uuid(&pool).await;
    let inner = inner_with(pool.clone(), Arc::new(FakeConfig::new("no_such_item", 1)));

    let mut conn = pool.acquire().await.unwrap();
    inner.grant_starter(&mut conn, &cid).await.unwrap();

    let holdings = inner.store.list(&Owner::character(&cid)).await.unwrap();
    assert_eq!(holdings.len(), 1, "grant must succeed via the fallback item");
    assert_eq!(holdings[0].item_id, STARTER_ITEM, "unknown configured item degrades to the default");
    assert_eq!(holdings[0].quantity, 1);

    cleanup_owner(&pool, &cid).await;
}

/// (a3) A NEGATIVE configured starter qty (would trip the holdings CHECK — a
/// poison) degrades to `STARTER_QTY`.
#[tokio::test]
async fn grant_starter_falls_back_to_default_qty_on_negative_config_qty() {
    let Some(pool) = test_pool().await else { return };
    ensure_schema(&pool).await;
    let cid = unique_uuid(&pool).await;
    let inner = inner_with(pool.clone(), Arc::new(FakeConfig::new(STARTER_ITEM, -5)));

    let mut conn = pool.acquire().await.unwrap();
    inner.grant_starter(&mut conn, &cid).await.unwrap();

    let holdings = inner.store.list(&Owner::character(&cid)).await.unwrap();
    assert_eq!(holdings.len(), 1, "grant must succeed via the fallback qty");
    assert_eq!(holdings[0].item_id, STARTER_ITEM);
    assert_eq!(holdings[0].quantity, STARTER_QTY, "negative configured qty degrades to the default");

    cleanup_owner(&pool, &cid).await;
}

/// (a4) A ZERO configured starter qty (a silent no-op grant) degrades to
/// `STARTER_QTY` the same way.
#[tokio::test]
async fn grant_starter_falls_back_to_default_qty_on_zero_config_qty() {
    let Some(pool) = test_pool().await else { return };
    ensure_schema(&pool).await;
    let cid = unique_uuid(&pool).await;
    let inner = inner_with(pool.clone(), Arc::new(FakeConfig::new(STARTER_ITEM, 0)));

    let mut conn = pool.acquire().await.unwrap();
    inner.grant_starter(&mut conn, &cid).await.unwrap();

    let holdings = inner.store.list(&Owner::character(&cid)).await.unwrap();
    assert_eq!(holdings.len(), 1, "grant must succeed via the fallback qty");
    assert_eq!(holdings[0].quantity, STARTER_QTY, "zero configured qty degrades to the default");

    cleanup_owner(&pool, &cid).await;
}

/// `lock_key` is stable per character id (two concurrent deliveries derive the SAME
/// advisory key and contend) and its namespaced seed diverges from scheduler's
/// plain FNV-1a of the same input string — the two modules can never contend on
/// each other's locks for equal strings.
#[test]
fn lock_key_is_stable_and_namespaced() {
    let id = "a2b7e8c1-0000-4000-8000-000000000001";
    assert_eq!(lock_key(id), lock_key(id));
    assert_ne!(lock_key("a"), lock_key("b"));
    // Plain (un-namespaced) FNV-1a of the same input — scheduler's discipline.
    let plain = {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for b in id.bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        h as i64
    };
    assert_ne!(lock_key(id), plain, "inventory keys must not collide with scheduler's for equal strings");
}

/// (b) list_character authz mapping with a FAKE Ownership: err→503, None→404,
/// mismatch→403, match→lists.
#[tokio::test]
async fn list_character_authz_mapping() {
    let Some(pool) = test_pool().await else { return };
    ensure_schema(&pool).await;
    let pid = unique_uuid(&pool).await;
    let cid = unique_uuid(&pool).await;

    let cfg: Arc<dyn Config> = Arc::new(FakeConfig::new(STARTER_ITEM, STARTER_QTY));

    // err → Unavailable (503)
    let inner = inner_with(pool.clone(), cfg.clone());
    let _ = inner.ownership.set(Arc::new(FakeOwnership::Fail));
    let e = inner.list_character(Identity::player(&pid), cid.clone()).await.unwrap_err();
    assert_eq!(e.status, Status::Unavailable);

    // None → NotFound (404)
    let inner = inner_with(pool.clone(), cfg.clone());
    let _ = inner.ownership.set(Arc::new(FakeOwnership::Miss));
    let e = inner.list_character(Identity::player(&pid), cid.clone()).await.unwrap_err();
    assert_eq!(e.status, Status::NotFound);

    // mismatch → Forbidden (403)
    let other = unique_uuid(&pool).await;
    let inner = inner_with(pool.clone(), cfg.clone());
    let _ = inner.ownership.set(Arc::new(FakeOwnership::Owner(other)));
    let e = inner.list_character(Identity::player(&pid), cid.clone()).await.unwrap_err();
    assert_eq!(e.status, Status::Forbidden);

    // match → lists that character's holdings
    let inner = inner_with(pool.clone(), cfg.clone());
    let _ = inner.ownership.set(Arc::new(FakeOwnership::Owner(pid.clone())));
    // Seed a holding for the character so the list is non-empty.
    let mut conn = pool.acquire().await.unwrap();
    inner
        .store
        .grant_exec(&mut conn, &Owner::character(&cid), "starter_sword", 3)
        .await
        .unwrap();
    let holdings = inner.list_character(Identity::player(&pid), cid.clone()).await.unwrap();
    assert_eq!(holdings.len(), 1);
    assert_eq!(holdings[0].owner_id, cid);
    assert_eq!(holdings[0].quantity, 3);

    // missing identity → Invalid
    let e = inner.list_character(Identity::none(), cid.clone()).await.unwrap_err();
    assert_eq!(e.status, Status::Invalid);

    cleanup_owner(&pool, &cid).await;
}

/// (c) The on_tx grant-on-Created path IN-PROCESS: inject a real asyncevents
/// transport (live DB), register inventory's on_tx(CREATED), start the plane's pull
/// workers, emit a Created, and assert a starter holding materializes for that
/// character.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn grant_on_created_via_on_tx() {
    let Some(pool) = test_pool().await else { return };
    ensure_schema(&pool).await;

    // The plane owns the transport; injecting it at Context construction means
    // inventory.init's on_tx records into THIS plane's subscription table.
    let dsn = std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DSN.to_string());
    let mut plane = asyncevents::Plane::new(pool.clone(), dsn).unwrap();
    let ctx = Context::with_db_and_transport(pool.clone(), plane.transport());

    // Provide the ownership + config deps inventory.init requires (fakes — no
    // characters/config module needed to exercise the event path).
    ctx.registry()
        .provide::<dyn Ownership>(key("characters", "ownership"), Arc::new(FakeOwnership::Miss) as Arc<dyn Ownership>);
    ctx.registry()
        .provide::<dyn Config>(key("config", "reader"), Arc::new(FakeConfig::new(STARTER_ITEM, STARTER_QTY)) as Arc<dyn Config>);

    let inv = Inventory::new();
    inv.register(&ctx).unwrap();
    inv.init(&ctx).unwrap(); // registers on_tx(CREATED/DELETED) -> subscribe_tx

    // Plane::start reconciles inventory's subscriptions into the shared catalog,
    // then launches the pull workers + NOTIFY wake-up.
    plane.start().await.unwrap();

    let cid = unique_uuid(&pool).await;
    let pid = unique_uuid(&pool).await;

    // Emit a durable Created (atomic with its own tx, like characters.create).
    let mut tx = pool.begin().await.unwrap();
    let created = charactersevents::Created {
        character_id: cid.clone(),
        player_id: pid,
        name: "Boromir".into(),
        class: "novice".into(),
    };
    ctx.bus()
        .emit_tx(AnyTx::new(&mut *tx), &charactersevents::CREATED, &created)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    // Poll for the async cross-boundary delivery to land the starter holding.
    let mut granted = false;
    for _ in 0..50 {
        let holdings = inv.inner().store.list(&Owner::character(&cid)).await.unwrap();
        if !holdings.is_empty() {
            assert_eq!(holdings[0].item_id, "starter_sword");
            assert_eq!(holdings[0].quantity, 1);
            granted = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(granted, "starter item not granted via on_tx within timeout");

    plane.stop().await;

    // Cleanup: the holding + the log events for this character.
    cleanup_owner(&pool, &cid).await;
    let _ = asyncevents::testing::cleanup_events(&pool, "character_id", &cid).await;
}

/// (d) Step 8's replacement for the removed inventory-owned `config.changed` cache:
/// `grant_starter`
/// reads the config reader DIRECTLY, so freshness rides the app-owned broadcast
/// invalidation plane. Wires up the REAL `config` module + a REAL
/// `invalidation::InvalidationPlane` (dev-dependency, mirrors `modules/match`'s use of
/// `rating`), does a raw SQL write to `config.settings` — exactly what the admin form /
/// `psql` do — polls the reader for the trigger's `pg_notify` to land, then proves the
/// NEXT grant (not the one already in flight) uses the new item.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn grant_starter_reflects_config_after_invalidation_refresh() {
    let Some(pool) = test_pool().await else { return };
    ensure_schema(&pool).await;

    let dsn = std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DSN.to_string());

    // inventory.init registers its durable on_tx subscriptions unconditionally, so the
    // Context needs a durable transport too (that events plane is never started —
    // this test exercises only the invalidation path; the subs just need somewhere
    // to record).
    let events_plane = asyncevents::Plane::new(pool.clone(), dsn.clone()).unwrap();
    let mut inv_plane = invalidation::InvalidationPlane::new(dsn);
    let ctx = Context::with_db_and_transport(pool.clone(), events_plane.transport())
        .with_invalidation(inv_plane.handle());

    // config's own schema (asyncevents is already migrated by ensure_schema, and
    // config's trigger only ever calls the plane-owned `append_event` function).
    let cfg_module = config::Config::new();
    Module::register(&cfg_module, &ctx).unwrap();
    Module::migrate(&cfg_module, &ctx).await.unwrap();

    // The fixed (namespace, key) `grant_starter` reads is process-wide, not
    // per-test-unique (it names a real code path, not a fixture id) — start from a
    // known-clean row so a previous run/split-proof pass cannot leak into this one.
    sqlx::query("DELETE FROM config.settings WHERE namespace = 'inventory' AND key = 'starter_item'")
        .execute(&pool)
        .await
        .unwrap();

    Module::init(&cfg_module, &ctx).unwrap(); // registers the config_changed callback
    Module::start(&cfg_module, &ctx).await.unwrap(); // boot-fill

    ctx.registry()
        .provide::<dyn Ownership>(key("characters", "ownership"), Arc::new(FakeOwnership::Miss) as Arc<dyn Ownership>);

    let inv = Inventory::new();
    inv.register(&ctx).unwrap();
    inv.init(&ctx).unwrap(); // resolves the config reader config.register just provided

    // Starts the callback's first (synchronous) refresh, then the NOTIFY listener.
    inv_plane.start().await.unwrap();

    // Before any config row exists, a grant uses the code default.
    let cid_before = unique_uuid(&pool).await;
    let mut conn = pool.acquire().await.unwrap();
    inv.inner().grant_starter(&mut conn, &cid_before).await.unwrap();
    let before = inv.inner().store.list(&Owner::character(&cid_before)).await.unwrap();
    assert_eq!(before[0].item_id, STARTER_ITEM);

    // The raw SQL write — same trigger path as the admin form / a `psql` edit — bumps
    // the revision, `pg_notify`s `config_changed`, and appends the durable audit event.
    sqlx::query(
        "INSERT INTO config.settings (namespace, key, value) VALUES ('inventory', 'starter_item', 'health_potion') \
         ON CONFLICT (namespace, key) DO UPDATE SET value = excluded.value",
    )
    .execute(&pool)
    .await
    .unwrap();

    // Poll the READER (not a grant) for the invalidation plane's NOTIFY-driven refresh
    // to land — the revision application this test's name promises.
    let cfg_reader = ctx.registry().require::<dyn Config>(&key("config", "reader"));
    let mut synced = false;
    for _ in 0..50 {
        if cfg_reader.get_string("inventory", "starter_item", "") == "health_potion" {
            synced = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(synced, "invalidation refresh did not propagate the new starter_item within timeout");

    // The NEXT grant (a fresh character, never touched before) uses the new item.
    let cid_after = unique_uuid(&pool).await;
    inv.inner().grant_starter(&mut conn, &cid_after).await.unwrap();
    let after = inv.inner().store.list(&Owner::character(&cid_after)).await.unwrap();
    assert_eq!(after[0].item_id, "health_potion");

    inv_plane.stop().await;

    // Cleanup: both holdings, the config row (leave it clean for the next run), and the
    // durable config.changed audit event this write appended.
    cleanup_owner(&pool, &cid_before).await;
    cleanup_owner(&pool, &cid_after).await;
    sqlx::query("DELETE FROM config.settings WHERE namespace = 'inventory' AND key = 'starter_item'")
        .execute(&pool)
        .await
        .unwrap();
    let _ = asyncevents::testing::cleanup_events(&pool, "namespace", "inventory").await;
}

/// (e) THE REORDER CASE, through the real plane: `character.created` and
/// `character.deleted` ride independent subscriptions with no cross-subscription
/// ordering, so deliver Deleted BEFORE Created for the same character id. The wipe
/// must plant a tombstone and the late grant must skip — NO holdings row may exist
/// afterwards. Because the plane offers no "created was consumed" signal for a
/// skipped grant, a SENTINEL character's Created is emitted AFTER the reordered one:
/// per-subscription XID ordering means the sentinel's grant landing proves the
/// reordered Created was already processed (and its checkpoint committed — a paused
/// subscription would never reach the sentinel).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deleted_before_created_tombstones_and_skips_late_grant() {
    let Some(pool) = test_pool().await else { return };
    ensure_schema(&pool).await;

    let dsn = std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DSN.to_string());
    let mut plane = asyncevents::Plane::new(pool.clone(), dsn).unwrap();
    let ctx = Context::with_db_and_transport(pool.clone(), plane.transport());
    ctx.registry()
        .provide::<dyn Ownership>(key("characters", "ownership"), Arc::new(FakeOwnership::Miss) as Arc<dyn Ownership>);
    ctx.registry()
        .provide::<dyn Config>(key("config", "reader"), Arc::new(FakeConfig::new(STARTER_ITEM, STARTER_QTY)) as Arc<dyn Config>);

    let inv = Inventory::new();
    inv.register(&ctx).unwrap();
    inv.init(&ctx).unwrap();
    plane.start().await.unwrap();

    let cid = unique_uuid(&pool).await;
    let sentinel = unique_uuid(&pool).await;
    let pid = unique_uuid(&pool).await;

    // 1. Deleted FIRST (the reorder): the wipe handler must plant the tombstone.
    let mut tx = pool.begin().await.unwrap();
    ctx.bus()
        .emit_tx(
            AnyTx::new(&mut *tx),
            &charactersevents::DELETED,
            &charactersevents::Deleted { character_id: cid.clone(), player_id: pid.clone() },
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let mut tombstoned = false;
    for _ in 0..50 {
        if tombstone_exists(&pool, &cid).await {
            tombstoned = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(tombstoned, "wipe delivery did not plant a tombstone within timeout");

    // 2. The LATE Created for the dead character, then the sentinel's Created.
    for character_id in [&cid, &sentinel] {
        let mut tx = pool.begin().await.unwrap();
        ctx.bus()
            .emit_tx(
                AnyTx::new(&mut *tx),
                &charactersevents::CREATED,
                &charactersevents::Created {
                    character_id: character_id.to_string(),
                    player_id: pid.clone(),
                    name: "Banquo".into(),
                    class: "novice".into(),
                },
            )
            .await
            .unwrap();
        tx.commit().await.unwrap();
    }

    // 3. The sentinel's grant landing proves the reordered Created was processed.
    let mut sentinel_granted = false;
    for _ in 0..50 {
        if !inv.inner().store.list(&Owner::character(&sentinel)).await.unwrap().is_empty() {
            sentinel_granted = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(sentinel_granted, "sentinel starter grant did not land within timeout");

    // 4. The dead character got NOTHING; the tombstone stands.
    let holdings = inv.inner().store.list(&Owner::character(&cid)).await.unwrap();
    assert!(holdings.is_empty(), "late grant must be skipped — no holdings for a wiped character");
    assert!(tombstone_exists(&pool, &cid).await, "tombstone must survive the skipped grant");

    plane.stop().await;

    cleanup_owner(&pool, &cid).await;
    cleanup_owner(&pool, &sentinel).await;
    let _ = asyncevents::testing::cleanup_events(&pool, "character_id", &cid).await;
    let _ = asyncevents::testing::cleanup_events(&pool, "character_id", &sentinel).await;
}

/// (f) The advisory xact-lock is actually exercised (mirrors the scheduler lock
/// tests' shape): two parallel txs on separate connections — one holds the GRANT
/// path pre-commit (the xact-lock is held until commit), the other runs the WIPE
/// path and must BLOCK on `pg_advisory_xact_lock` until the first commits. Without
/// the lock, under READ COMMITTED both would proceed and commit an orphaned holding
/// alongside a tombstone.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_grant_and_wipe_serialize_on_advisory_lock() {
    let Some(pool) = test_pool().await else { return };
    ensure_schema(&pool).await;
    let cid = unique_uuid(&pool).await;
    let inner = inner_with(pool.clone(), Arc::new(FakeConfig::new(STARTER_ITEM, STARTER_QTY)));

    // Tx 1: grant, NOT committed — holds the per-character xact-lock.
    let mut tx1 = pool.begin().await.unwrap();
    inner.grant_starter(&mut tx1, &cid).await.unwrap();

    // Tx 2 (separate connection, parallel task): the wipe must block on the lock.
    let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let wipe = tokio::spawn({
        let pool = pool.clone();
        let inner = inner.clone();
        let cid = cid.clone();
        let done = done.clone();
        async move {
            let mut tx2 = pool.begin().await.unwrap();
            inner.wipe_character(&mut tx2, &cid).await.unwrap();
            done.store(true, std::sync::atomic::Ordering::SeqCst);
            tx2.commit().await.unwrap();
        }
    });

    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        !done.load(std::sync::atomic::Ordering::SeqCst),
        "wipe must block on the per-character advisory lock while the grant tx is open"
    );

    // Commit releases the xact-lock; the wipe proceeds and wins (it runs second).
    tx1.commit().await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), wipe).await.unwrap().unwrap();
    assert!(done.load(std::sync::atomic::Ordering::SeqCst));

    let holdings = inner.store.list(&Owner::character(&cid)).await.unwrap();
    assert!(holdings.is_empty(), "the serialized wipe must have cleared the committed grant");
    assert!(tombstone_exists(&pool, &cid).await, "the wipe must have planted the tombstone");

    cleanup_owner(&pool, &cid).await;
}
