use super::*;
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
        starter: RwLock::new(None),
    });
    let _ = inner.cfg.set(cfg);
    inner
}

// ---- No-DB unit tests --------------------------------------------------

/// The starter spec lazily loads from config, then rebuilds ONLY on a relevant
/// config.changed — an unrelated change is ignored.
#[tokio::test]
async fn starter_spec_reloads_on_config_change() {
    let pool = PgPool::connect_lazy(DEFAULT_DSN).unwrap(); // never queried
    let cfg = Arc::new(FakeConfig::new(STARTER_ITEM, STARTER_QTY));
    let inner = inner_with(pool, cfg.clone());

    // Lazy first load: the config values.
    assert_eq!(inner.starter_spec(), (STARTER_ITEM.to_string(), 1));

    // Change config, but fire an UNRELATED change → no reload.
    *cfg.item.lock().unwrap() = "health_potion".into();
    *cfg.qty.lock().unwrap() = 5;
    inner.on_config_changed(configevents::Changed {
        namespace: "game".into(),
        key: "name".into(),
        value: "arena".into(),
    });
    assert_eq!(inner.starter_spec(), (STARTER_ITEM.to_string(), 1), "unrelated change must not reload");

    // A relevant change reloads the materialized spec.
    inner.on_config_changed(configevents::Changed {
        namespace: "inventory".into(),
        key: "starter_item".into(),
        value: "health_potion".into(),
    });
    assert_eq!(inner.starter_spec(), ("health_potion".to_string(), 5));
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

/// Migrates asyncevents (durable plane's outbox) + inventory schemas EXACTLY
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
/// transport (live DB), register inventory's on_tx(CREATED), start the plane's relay,
/// emit a Created, and assert a starter holding materializes for that character.
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

    // Plane::start snapshots inventory's subscription into relay targets, then
    // launches the relay + LISTEN loop.
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
        .emit_tx(&mut tx, &charactersevents::CREATED, &created)
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

    // Cleanup: the holding + the outbox row for this character.
    cleanup_owner(&pool, &cid).await;
    let _ = sqlx::query("DELETE FROM asyncevents.outbox WHERE payload->>'character_id' = $1")
        .bind(&cid)
        .execute(&pool)
        .await;
}
