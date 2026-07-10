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
        ownership: OnceLock::new(),
        cfg: OnceLock::new(),
    });
    let _ = inner.cfg.set(cfg);
    inner
}

// ---- No-DB unit tests --------------------------------------------------

/// `starter_spec` reads straight off the injected config reader (no cache, Step 8):
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
        "no local cache — the very next read sees the new config values"
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

    cleanup_owner(&pool, &cid).await;
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

/// (d) Step 8's replacement for the removed `config.changed` cache: `grant_starter`
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
