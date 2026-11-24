use super::*;
use bus::{AnyTx, Delivery, TxHandler};
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

/// `validate_quantity` is THE quantity-policy authority: it accepts exactly
/// `1..=MAX_HOLDING_QTY` and rejects everything else. Pins each boundary so a
/// future edit to the one gate can't silently widen or narrow it. No DB.
#[test]
fn validate_quantity_boundaries() {
    assert!(validate_quantity(0).is_err(), "zero is a no-op grant — rejected");
    assert!(validate_quantity(-1).is_err(), "negative — rejected");
    assert_eq!(validate_quantity(1).unwrap(), 1, "the smallest legit grant");
    assert_eq!(
        validate_quantity(MAX_HOLDING_QTY).unwrap(),
        MAX_HOLDING_QTY,
        "the cap itself is inclusive"
    );
    assert!(validate_quantity(MAX_HOLDING_QTY + 1).is_err(), "one over the cap — rejected");
    assert!(
        validate_quantity(i64::MAX).is_err(),
        "the value that overflowed the old int4 column (22003) — rejected before the DB"
    );
}

// ---- Live Postgres integration ----------------------------------------

use testdb::test_pool;

/// Serializes the tests that plant or sweep `inventory.purged_players`: the whole-table `FOR UPDATE SKIP LOCKED` sweep skips rows a sibling's open sweep has locked.
static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn ensure_schema(pool: &PgPool) {
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
}

async fn unique_uuid(pool: &PgPool) -> String {
    let (id,): (String,) = sqlx::query_as("SELECT gen_random_uuid()::text")
        .fetch_one(pool)
        .await
        .unwrap();
    id
}

/// The ids of `player_id`'s characters in inventory's `character_owners` record, in id order.
async fn recorded_characters(pool: &PgPool, player_id: &str) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT character_id::text FROM inventory.character_owners WHERE player_id = $1::uuid ORDER BY character_id",
    )
    .bind(player_id)
    .fetch_all(pool)
    .await
    .unwrap()
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
    let _ = sqlx::query("DELETE FROM inventory.character_owners WHERE character_id = $1::uuid")
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

/// The character → player record: `record_character_owner` fills it, a redelivery is a
/// no-op, `wipe_character` removes it, and a record redelivered after the wipe is
/// skipped by the tombstone.
#[tokio::test]
async fn character_owner_record_then_wipe_on_conn() {
    let Some(pool) = test_pool().await else { return };
    ensure_schema(&pool).await;
    let cid = unique_uuid(&pool).await;
    let pid = unique_uuid(&pool).await;
    let inner = inner_with(pool.clone(), Arc::new(FakeConfig::new(STARTER_ITEM, STARTER_QTY)));

    let mut conn = pool.acquire().await.unwrap();
    inner.record_character_owner(&mut conn, &cid, &pid).await.unwrap();
    assert_eq!(recorded_characters(&pool, &pid).await, vec![cid.clone()]);

    // A redelivered character.created changes nothing.
    inner.record_character_owner(&mut conn, &cid, &pid).await.unwrap();
    assert_eq!(
        recorded_characters(&pool, &pid).await,
        vec![cid.clone()],
        "a redelivery must not add or change the record"
    );

    inner.wipe_character(&mut conn, &cid).await.unwrap();
    assert!(
        recorded_characters(&pool, &pid).await.is_empty(),
        "the wipe must remove the owner record"
    );

    inner.record_character_owner(&mut conn, &cid, &pid).await.unwrap();
    assert!(
        recorded_characters(&pool, &pid).await.is_empty(),
        "a record after the wipe must be skipped by the tombstone"
    );

    cleanup_owner(&pool, &cid).await;
}

/// `character.deleted` delivered before `character.created` (independent
/// subscriptions): the record never appears.
#[tokio::test]
async fn character_owner_wipe_before_record_records_nothing() {
    let Some(pool) = test_pool().await else { return };
    ensure_schema(&pool).await;
    let cid = unique_uuid(&pool).await;
    let pid = unique_uuid(&pool).await;
    let inner = inner_with(pool.clone(), Arc::new(FakeConfig::new(STARTER_ITEM, STARTER_QTY)));

    let mut conn = pool.acquire().await.unwrap();
    inner.wipe_character(&mut conn, &cid).await.unwrap();
    inner.record_character_owner(&mut conn, &cid, &pid).await.unwrap();
    assert!(recorded_characters(&pool, &pid).await.is_empty());

    cleanup_owner(&pool, &cid).await;
}

/// The wiring, through the real plane: `inventory.character-owner.v1` records the
/// owner from a durable `character.created`, and `character.deleted` removes it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn character_owner_follows_created_and_deleted_via_on_tx() {
    let Some(pool) = test_pool().await else { return };
    ensure_schema(&pool).await;

    let dsn = std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DSN.to_string());
    let mut plane = asyncevents::Plane::new(pool.clone(), dsn).unwrap();
    let ctx = Context::with_db_and_transport(pool.clone(), plane.transport());
    ctx.registry().provide::<dyn Ownership>(
        key("characters", "ownership"),
        Arc::new(FakeOwnership::Miss) as Arc<dyn Ownership>,
    );
    ctx.registry().provide::<dyn Config>(
        key("config", "reader"),
        Arc::new(FakeConfig::new(STARTER_ITEM, STARTER_QTY)) as Arc<dyn Config>,
    );
    let inv = Inventory::new();
    inv.register(&ctx).unwrap();
    inv.init(&ctx).unwrap();
    plane.start().await.unwrap();

    let cid = unique_uuid(&pool).await;
    let pid = unique_uuid(&pool).await;

    let mut tx = pool.begin().await.unwrap();
    let created = charactersevents::Created {
        character_id: cid.clone(),
        player_id: pid.clone(),
        name: "Eowyn".into(),
        class: "novice".into(),
    };
    ctx.bus()
        .emit_tx(AnyTx::new(&mut *tx), &charactersevents::CREATED, &created)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let mut recorded = false;
    for _ in 0..50 {
        if recorded_characters(&pool, &pid).await == vec![cid.clone()] {
            recorded = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(recorded, "character.created did not record the character's player");

    let mut tx = pool.begin().await.unwrap();
    let deleted = charactersevents::Deleted {
        character_id: cid.clone(),
        player_id: pid.clone(),
    };
    ctx.bus()
        .emit_tx(AnyTx::new(&mut *tx), &charactersevents::DELETED, &deleted)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let mut removed = false;
    for _ in 0..50 {
        if recorded_characters(&pool, &pid).await.is_empty() {
            removed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(removed, "character.deleted did not remove the owner record");

    plane.stop().await;
    cleanup_owner(&pool, &cid).await;
    let _ = asyncevents::testing::cleanup_events(&pool, "character_id", &cid).await;
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

/// `lock_key` is representation-invariant across the spellings Postgres's `::uuid`
/// treats as EQUAL (case, braces, hyphenation): the row SQL normalizes via `::uuid`,
/// so a grant and a wipe for the one character MUST derive the SAME advisory key or
/// they fail to serialize and commit an orphan holding alongside a tombstone. A
/// genuinely different uuid must still diverge. Fails on the pre-fix raw-byte hash.
#[test]
fn lock_key_is_representation_invariant() {
    let canon = "a2b7e8c1-0000-4000-8000-000000000001";
    assert_eq!(lock_key(canon), lock_key(&canon.to_uppercase()), "case must not change the key");
    assert_eq!(lock_key(canon), lock_key(&format!("{{{canon}}}")), "braces must not change the key");
    assert_eq!(lock_key(canon), lock_key(&canon.replace('-', "")), "hyphenation must not change the key");
    assert_ne!(
        lock_key(canon),
        lock_key("a2b7e8c1-0000-4000-8000-000000000002"),
        "a genuinely different uuid must still take a different key"
    );
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
/// inventory `init` resolves `dyn Config` via `require::<dyn Config>(key("config",
/// "reader"))`, and `grant_starter`/`starter_spec` read the starter item straight off
/// that injected reader — so a fresh grant always reflects the reader's current value.
///
/// SCOPE: this unit narrows to that inventory-LOCAL config resolution + read only,
/// against a fake `configapi::Config`. The end-to-end live-reload path — a raw SQL /
/// admin write to `config.settings` → Postgres trigger `pg_notify(config_changed)` →
/// the app-owned broadcast invalidation plane refreshing the reader → the NEXT grant
/// picking up the new value — is asserted CROSS-PROCESS in split-proof [C1]-[C4b]
/// (`tools/splitproof/src/main.rs`), where the real trigger and invalidation plane
/// exist. Driving the real `config` module + `invalidation` plane here would only
/// re-prove that flow with a module->module dev-dep, so we deliberately don't.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn grant_starter_reflects_current_config_reader_value() {
    let Some(pool) = test_pool().await else { return };
    ensure_schema(&pool).await;

    // inventory.init registers its durable on_tx subscriptions unconditionally, so the
    // Context needs a durable transport for the subs to record into. The plane is
    // never started — this unit exercises only the direct config-read path.
    let dsn = std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DSN.to_string());
    let events_plane = asyncevents::Plane::new(pool.clone(), dsn).unwrap();
    let ctx = Context::with_db_and_transport(pool.clone(), events_plane.transport());

    // A mutable fake reader provided under the SAME registry key the real `config`
    // module would use — inventory.init resolves it there, exactly as in the split.
    let cfg = Arc::new(FakeConfig::new(STARTER_ITEM, STARTER_QTY));
    ctx.registry()
        .provide::<dyn Ownership>(key("characters", "ownership"), Arc::new(FakeOwnership::Miss) as Arc<dyn Ownership>);
    ctx.registry()
        .provide::<dyn Config>(key("config", "reader"), cfg.clone() as Arc<dyn Config>);

    let inv = Inventory::new();
    inv.register(&ctx).unwrap();
    inv.init(&ctx).unwrap(); // resolves the config reader we just provided

    // With the reader at its default, a grant uses the compiled STARTER_ITEM.
    let cid_before = unique_uuid(&pool).await;
    let mut conn = pool.acquire().await.unwrap();
    inv.inner().grant_starter(&mut conn, &cid_before).await.unwrap();
    let before = inv.inner().store.list(&Owner::character(&cid_before)).await.unwrap();
    assert_eq!(before[0].item_id, STARTER_ITEM);

    // Change what the reader returns (the effect the invalidation refresh has in
    // production, minus the wire), then prove the NEXT grant reads the new value.
    *cfg.item.lock().unwrap() = "health_potion".to_string();

    let cid_after = unique_uuid(&pool).await;
    inv.inner().grant_starter(&mut conn, &cid_after).await.unwrap();
    let after = inv.inner().store.list(&Owner::character(&cid_after)).await.unwrap();
    assert_eq!(after[0].item_id, "health_potion");

    cleanup_owner(&pool, &cid_before).await;
    cleanup_owner(&pool, &cid_after).await;
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

/// (f2) Same serialization guarantee as (f), but the two deliveries spell the ONE
/// character id DIFFERENTLY — grant on the canonical uuid, wipe on its uppercased
/// form. Postgres's `::uuid` treats them as equal (the row SQL agrees), so the
/// advisory lock MUST too: the wipe has to block on the grant's xact-lock exactly as
/// in (f). Pre-fix, the raw-byte hash gave the two spellings DIFFERENT keys, the wipe
/// would NOT block, and both txs would commit an orphan holding beside a tombstone.
///
/// This is a CONSTRUCTED injection: Step 2 makes production emit canonical ids, so
/// this divergence does not occur on today's producer path. It proves the authority
/// holds for a future producer / a raw psql write that spells the id non-canonically.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_grant_and_wipe_serialize_across_uuid_spelling() {
    let Some(pool) = test_pool().await else { return };
    ensure_schema(&pool).await;
    let cid = unique_uuid(&pool).await;
    let cid_upper = cid.to_uppercase();
    // A random uuid could (prob ~5e-7) contain no a-f nibble, making to_uppercase a
    // no-op and the proof vacuous (cid_upper == cid passes even pre-fix). Fail LOUD.
    assert_ne!(cid_upper, cid, "test needs a uuid with a hex letter to exercise case-normalization");
    let inner = inner_with(pool.clone(), Arc::new(FakeConfig::new(STARTER_ITEM, STARTER_QTY)));

    // Tx 1: grant on the CANONICAL id, NOT committed — holds the per-character xact-lock.
    let mut tx1 = pool.begin().await.unwrap();
    inner.grant_starter(&mut tx1, &cid).await.unwrap();

    // Tx 2: wipe on a NON-canonical (uppercased) spelling of the SAME id — must block.
    let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let wipe = tokio::spawn({
        let pool = pool.clone();
        let inner = inner.clone();
        let cid_upper = cid_upper.clone();
        let done = done.clone();
        async move {
            let mut tx2 = pool.begin().await.unwrap();
            inner.wipe_character(&mut tx2, &cid_upper).await.unwrap();
            done.store(true, std::sync::atomic::Ordering::SeqCst);
            tx2.commit().await.unwrap();
        }
    });

    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        !done.load(std::sync::atomic::Ordering::SeqCst),
        "the differently-spelled wipe must still block on the per-character advisory lock"
    );

    tx1.commit().await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), wipe).await.unwrap().unwrap();
    assert!(done.load(std::sync::atomic::Ordering::SeqCst));

    let holdings = inner.store.list(&Owner::character(&cid)).await.unwrap();
    assert!(holdings.is_empty(), "the serialized wipe must have cleared the committed grant");
    assert!(tombstone_exists(&pool, &cid).await, "the wipe must have planted the tombstone");

    cleanup_owner(&pool, &cid).await;
}

/// (g) Posture B (HTTP IAP): `Holdings::grant` REJECTS an out-of-range qty as
/// Invalid (400) — never a DB error — and accepts the cap. Pins the client-facing
/// posture (contrast the durable-subscription degrade in posture A). i64::MAX is the
/// value that used to 500 on the int4 overflow; now it's a clean 400 before any DB
/// touch. The `MAX_HOLDING_QTY` accept path exercises the real store insert.
#[tokio::test]
async fn grant_http_rejects_out_of_range_qty() {
    let Some(pool) = test_pool().await else { return };
    ensure_schema(&pool).await;
    let pid = unique_uuid(&pool).await;
    // dev_grant defaults ON in inner_with, so grant reaches its input validation.
    let inner = inner_with(pool.clone(), Arc::new(FakeConfig::new(STARTER_ITEM, STARTER_QTY)));

    // i64::MAX — the overflow value — is rejected as Invalid, NOT an internal/DB error.
    let e = inner
        .grant(Identity::player(&pid), "coin".into(), i64::MAX)
        .await
        .unwrap_err();
    assert_eq!(e.status, Status::Invalid, "i64::MAX qty must be a 400, never a DB error");

    // One over the app cap — same client-facing rejection.
    let e = inner
        .grant(Identity::player(&pid), "coin".into(), MAX_HOLDING_QTY + 1)
        .await
        .unwrap_err();
    assert_eq!(e.status, Status::Invalid, "qty just over MAX_HOLDING_QTY must be a 400");

    // The cap itself is accepted and actually lands (within the DB CHECK's 2_000_000).
    let holdings = inner
        .grant(Identity::player(&pid), "coin".into(), MAX_HOLDING_QTY)
        .await
        .expect("qty == MAX_HOLDING_QTY must succeed");
    assert_eq!(holdings.len(), 1);
    assert_eq!(holdings[0].item_id, "coin");
    assert_eq!(holdings[0].quantity, MAX_HOLDING_QTY);

    cleanup_owner(&pool, &pid).await;
}

/// (h) THE POISON BRANCH, through the REAL delivery path: an out-of-range
/// `starter_qty` (2147483648 — 2^31, which overflowed the old int4 column to
/// SQLSTATE 22003 inside the delivery tx) must DEGRADE to `STARTER_QTY` and the
/// starter grant must LAND, so `inventory.character-created.v1` is NOT poison-paused.
/// Same end-to-end harness as `grant_on_created_via_on_tx`: real asyncevents plane,
/// real on_tx(CREATED), emit a durable Created, poll for the grant. On pre-fix code
/// the delivery raises 22003 and pauses the subscription — that's the branch pinned.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn starter_qty_out_of_range_degrades_to_default() {
    let Some(pool) = test_pool().await else { return };
    ensure_schema(&pool).await;

    let dsn = std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DSN.to_string());
    let mut plane = asyncevents::Plane::new(pool.clone(), dsn).unwrap();
    let ctx = Context::with_db_and_transport(pool.clone(), plane.transport());

    ctx.registry().provide::<dyn Ownership>(
        key("characters", "ownership"),
        Arc::new(FakeOwnership::Miss) as Arc<dyn Ownership>,
    );
    // 2^31 — a value the old int4 quantity column could not hold. It exceeds
    // MAX_HOLDING_QTY, so posture A degrades it to STARTER_QTY on the read path.
    ctx.registry().provide::<dyn Config>(
        key("config", "reader"),
        Arc::new(FakeConfig::new(STARTER_ITEM, 2_147_483_648)) as Arc<dyn Config>,
    );

    let inv = Inventory::new();
    inv.register(&ctx).unwrap();
    inv.init(&ctx).unwrap();
    plane.start().await.unwrap();

    let cid = unique_uuid(&pool).await;
    let pid = unique_uuid(&pool).await;

    let mut tx = pool.begin().await.unwrap();
    let created = charactersevents::Created {
        character_id: cid.clone(),
        player_id: pid,
        name: "Faramir".into(),
        class: "novice".into(),
    };
    ctx.bus()
        .emit_tx(AnyTx::new(&mut *tx), &charactersevents::CREATED, &created)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    // The grant landing (not pausing) IS the proof the subscription survived the
    // out-of-range config value — and it lands at the degraded default qty.
    let mut granted = false;
    for _ in 0..50 {
        let holdings = inv.inner().store.list(&Owner::character(&cid)).await.unwrap();
        if !holdings.is_empty() {
            assert_eq!(holdings[0].item_id, STARTER_ITEM);
            assert_eq!(
                holdings[0].quantity, STARTER_QTY,
                "out-of-range starter_qty must degrade to the default, not poison the delivery"
            );
            granted = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        granted,
        "starter grant did not land — an out-of-range starter_qty poison-paused the subscription"
    );

    plane.stop().await;
    cleanup_owner(&pool, &cid).await;
    let _ = asyncevents::testing::cleanup_events(&pool, "character_id", &cid).await;
}

/// (i) The DB CHECK is the DEEPER authority: a raw sqlx INSERT (exactly what a `psql`
/// edit does, bypassing every service-side `validate_quantity`) with a quantity above
/// the 2_000_000 CHECK ceiling is refused by Postgres with SQLSTATE 23514 (check
/// violation). Pins the DDL authority — the guard that covers writers the Rust policy
/// fn never sees.
#[tokio::test]
async fn raw_sql_insert_above_check_is_rejected() {
    let Some(pool) = test_pool().await else { return };
    ensure_schema(&pool).await;
    let oid = unique_uuid(&pool).await;

    let err = sqlx::query(
        "INSERT INTO inventory.holdings (owner_type, owner_id, item_id, quantity) \
         VALUES ('player', $1::uuid, 'coin', 2000001)",
    )
    .bind(&oid)
    .execute(&pool)
    .await
    .expect_err("a quantity above the DB CHECK ceiling must be rejected");

    let code = err
        .as_database_error()
        .and_then(|d| d.code())
        .map(|c| c.into_owned())
        .unwrap_or_default();
    assert_eq!(code, "23514", "must be a CHECK-constraint violation (23514), not a silent write");

    cleanup_owner(&pool, &oid).await;
}

/// (j) The accumulated-state ceiling on the HTTP path: a player whose stored holding
/// legally reached 2_000_000 (each grant <= MAX_HOLDING_QTY, summed via ON CONFLICT)
/// gets Conflict (409) on the next grant — a definitive answer about durable state,
/// NEVER an undifferentiated Internal/500. Seeds the maxed holding via direct SQL
/// (a legal state the CHECK permits), then grants qty 1 through the real service
/// path, exercising the `is_holdings_cap_violation` mapping on the 23514 branch.
#[tokio::test]
async fn grant_at_holding_cap_maps_check_violation_to_conflict() {
    let Some(pool) = test_pool().await else { return };
    ensure_schema(&pool).await;
    let pid = unique_uuid(&pool).await;
    let inner = inner_with(pool.clone(), Arc::new(FakeConfig::new(STARTER_ITEM, STARTER_QTY)));

    // Legal accumulated state: exactly at the DB CHECK ceiling.
    sqlx::query(
        "INSERT INTO inventory.holdings (owner_type, owner_id, item_id, quantity) \
         VALUES ('player', $1::uuid, 'coin', 2000000)",
    )
    .bind(&pid)
    .execute(&pool)
    .await
    .unwrap();

    // A per-grant-legal qty (1) trips the post-sum CHECK — must map to Conflict.
    let e = inner
        .grant(Identity::player(&pid), "coin".into(), 1)
        .await
        .unwrap_err();
    assert_eq!(
        e.status,
        Status::Conflict,
        "a legal grant hitting the accumulated-state ceiling must be 409, never internal/500"
    );

    // The stored state is untouched — the failed grant did not partially apply.
    let holdings = inner.store.list(&Owner::player(&pid)).await.unwrap();
    assert_eq!(holdings.len(), 1);
    assert_eq!(holdings[0].quantity, 2_000_000);

    cleanup_owner(&pool, &pid).await;
}

/// (k) list is capped by `HOLDINGS_HARD_LIMIT` — the safety belt, not the per-grant
/// quantity policy: an owner holding more DISTINCT items than the ceiling (seeded
/// directly via SQL, bypassing `grant`, to prove the belt fires regardless of how
/// the rows got there) still gets a bounded response instead of an unbounded
/// `fetch_all`. Each holding needs a distinct `item_id` (the holdings PK is
/// `(owner_type, owner_id, item_id)`, and `item_id` carries an in-module FK to
/// `inventory.items`), so distinct synthetic items are seeded first.
#[tokio::test]
async fn holdings_list_is_capped_at_hard_limit() {
    let Some(pool) = test_pool().await else { return };
    ensure_schema(&pool).await;
    let pid = unique_uuid(&pool).await;
    let inner = inner_with(pool.clone(), Arc::new(FakeConfig::new(STARTER_ITEM, STARTER_QTY)));

    let over = HOLDINGS_HARD_LIMIT + 5;

    // Distinct synthetic items so each holding row can carry a unique item_id.
    sqlx::query(
        "INSERT INTO inventory.items (id, name, kind) \
         SELECT 'cap_test_' || g, 'Cap Test ' || g, 'misc' FROM generate_series(1, $1) g \
         ON CONFLICT (id) DO NOTHING",
    )
    .bind(over)
    .execute(&pool)
    .await
    .unwrap();

    // One holding per synthetic item, all for the same owner.
    sqlx::query(
        "INSERT INTO inventory.holdings (owner_type, owner_id, item_id, quantity) \
         SELECT 'player', $1::uuid, 'cap_test_' || g, 1 FROM generate_series(1, $2) g",
    )
    .bind(&pid)
    .bind(over)
    .execute(&pool)
    .await
    .unwrap();

    let holdings = inner.store.list(&Owner::player(&pid)).await.unwrap();
    assert_eq!(holdings.len(), HOLDINGS_HARD_LIMIT as usize);

    cleanup_owner(&pool, &pid).await;
    let _ = sqlx::query("DELETE FROM inventory.items WHERE id LIKE 'cap_test_%'")
        .execute(&pool)
        .await;
}

// ============================================================================
// Account-deletion purge (Steps 5-7): the tombstone, the sweep (player-owned rows
// cleared, character-owned rows kept), and the `grant` write guard.
// ============================================================================

async fn purge_swept_at(pool: &PgPool, player_id: &str) -> Option<Option<String>> {
    sqlx::query_as::<_, (Option<String>,)>(
        "SELECT swept_at::text FROM inventory.purged_players WHERE player_id = $1::uuid",
    )
    .bind(player_id)
    .fetch_optional(pool)
    .await
    .unwrap()
    .map(|(s,)| s)
}

async fn stamp_purge_swept(pool: &PgPool, player_id: &str) {
    sqlx::query("UPDATE inventory.purged_players SET swept_at = now() WHERE player_id = $1::uuid")
        .bind(player_id)
        .execute(pool)
        .await
        .unwrap();
}

async fn cleanup_purge_row(pool: &PgPool, player_id: &str) {
    let _ = sqlx::query("DELETE FROM inventory.purged_players WHERE player_id = $1::uuid")
        .bind(player_id)
        .execute(pool)
        .await;
}

fn purge_payload(name: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({ "name": name })).unwrap()
}

async fn seed_holding(inner: &Inner, pool: &PgPool, owner: &Owner) {
    let mut tx = pool.begin().await.unwrap();
    inner.store.grant_exec(&mut tx, owner, "starter_sword", 1).await.unwrap();
    tx.commit().await.unwrap();
}

async fn holding_count(pool: &PgPool, player_id: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM inventory.holdings WHERE owner_type = 'player' AND owner_id = $1::uuid",
    )
    .bind(player_id)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn with_purge_cleanup<Fut>(
    pool: &PgPool,
    apps: Vec<String>,
    owners: Vec<String>,
    tombstones: Vec<String>,
    body: Fut,
)
where
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    let mut task = tokio::spawn(body);
    let outcome = tokio::time::timeout(Duration::from_secs(60), &mut task).await;
    if outcome.is_err() {
        task.abort();
        let _ = (&mut task).await;
    }
    if !matches!(outcome, Ok(Ok(()))) {
        for app in &apps {
            sqlx::query(
                "SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE application_name = $1",
            )
            .bind(app)
            .execute(pool)
            .await
            .unwrap();
        }
    }
    for id in &owners {
        cleanup_owner(pool, id).await;
    }
    for pid in &tombstones {
        cleanup_purge_row(pool, pid).await;
    }
    match outcome {
        Ok(Ok(())) => {}
        Ok(Err(e)) if e.is_panic() => std::panic::resume_unwind(e.into_panic()),
        Ok(Err(e)) => panic!("inventory purge test task ended without completing: {e}"),
        Err(_) => panic!("inventory purge test body did not finish within 60s"),
    }
}

fn fixed_player(n: u64) -> String {
    format!("00000000-0000-0000-0000-{n:012x}")
}

async fn clear_fixed_player(pool: &PgPool, player_id: &str) {
    cleanup_owner(pool, player_id).await;
    cleanup_purge_row(pool, player_id).await;
}

async fn assert_claimed_by_the_next_fire(pool: &PgPool, player_id: &str) {
    let below: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM inventory.purged_players \
          WHERE swept_at IS NULL AND player_id < $1::uuid",
    )
    .bind(player_id)
    .fetch_one(pool)
    .await
    .unwrap();
    assert!(
        below < projection::INVENTORY_PLAYERS_PER_FIRE,
        "precondition: {below} unswept tombstones sort below {player_id}, so the next fire \
         cannot claim it"
    );
}

async fn unstamped_tombstones(pool: &PgPool, ids: &[String]) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT player_id::text FROM inventory.purged_players \
          WHERE player_id = ANY($1::uuid[]) AND swept_at IS NULL ORDER BY player_id",
    )
    .bind(ids)
    .fetch_all(pool)
    .await
    .unwrap()
}

/// The tombstone insert is idempotent, and a redelivery after the sweep stamped
/// `swept_at` must NOT reset it.
#[tokio::test]
async fn plant_purged_player_is_idempotent_and_never_resets_swept_at() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    ensure_schema(&pool).await;
    let pid = unique_uuid(&pool).await;
    let inner = inner_with(pool.clone(), Arc::new(FakeConfig::new(STARTER_ITEM, STARTER_QTY)));

    let mut conn = pool.acquire().await.unwrap();
    inner.plant_purged_player(&mut conn, &pid, "Handle#0001").await.unwrap();
    inner.plant_purged_player(&mut conn, &pid, "Handle#0001").await.unwrap();

    let (n,): (i64,) =
        sqlx::query_as("SELECT count(*) FROM inventory.purged_players WHERE player_id = $1::uuid")
            .bind(&pid)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(n, 1, "a redelivered tombstone must not insert a second row");

    stamp_purge_swept(&pool, &pid).await;
    inner.plant_purged_player(&mut conn, &pid, "Handle#0001").await.unwrap();
    assert!(
        purge_swept_at(&pool, &pid).await.flatten().is_some(),
        "a redelivery after the sweep stamped swept_at must NOT reset it"
    );

    cleanup_purge_row(&pool, &pid).await;
}

/// THE PREVIOUSLY-WRONG SHAPE this sweep must never regress into: clearing a
/// player's holdings by player_id must NOT touch that same player's characters'
/// holdings — those arrive on the untouched `character.deleted`/wipe path, driven
/// separately by `characters`' own sweep. A sweep that (wrongly) tried to enumerate
/// and clear a purged player's characters here would delete the character-owned
/// row this test pins as a survivor.
#[tokio::test]
async fn sweep_clears_player_owned_holdings_but_keeps_character_owned_rows() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    ensure_schema(&pool).await;
    let pid = fixed_player(0x100);
    clear_fixed_player(&pool, &pid).await;
    let cid = unique_uuid(&pool).await; // a character owned by the same purged player
    let inner = inner_with(pool.clone(), Arc::new(FakeConfig::new(STARTER_ITEM, STARTER_QTY)));

    with_purge_cleanup(&pool, vec![], vec![pid.clone(), cid.clone()], vec![pid.clone()], {
        let pool = pool.clone();
        let inner = inner.clone();
        async move {
            seed_holding(&inner, &pool, &Owner::player(&pid)).await;
            seed_holding(&inner, &pool, &Owner::character(&cid)).await;

            let mut conn = pool.acquire().await.unwrap();
            inner.plant_purged_player(&mut conn, &pid, "Sweep#0001").await.unwrap();
            drop(conn);
            assert_claimed_by_the_next_fire(&pool, &pid).await;

            let mut tx = pool.begin().await.unwrap();
            inner.sweep_purged_players(&mut tx).await.unwrap();
            tx.commit().await.unwrap();

            let player_holdings = inner.store.list(&Owner::player(&pid)).await.unwrap();
            assert!(player_holdings.is_empty(), "the player-owned holding must be cleared");
            let char_holdings = inner.store.list(&Owner::character(&cid)).await.unwrap();
            assert_eq!(
                char_holdings.len(),
                1,
                "the character-owned holding must be KEPT — it is not this sweep's work"
            );
            assert!(purge_swept_at(&pool, &pid).await.flatten().is_some());
        }
    })
    .await;
}

/// A survivor (non-tombstoned player) is untouched by the sweep, and a foreign
/// `scheduler.fired` name sweeps nothing — proven with a positive control on the
/// same row.
#[tokio::test]
async fn sweep_handler_survivor_untouched_and_ignores_foreign_schedule_name() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    ensure_schema(&pool).await;
    let target = fixed_player(0x110);
    clear_fixed_player(&pool, &target).await;
    let survivor = unique_uuid(&pool).await;
    let inner = inner_with(pool.clone(), Arc::new(FakeConfig::new(STARTER_ITEM, STARTER_QTY)));

    let both = vec![target.clone(), survivor.clone()];
    with_purge_cleanup(&pool, vec![], both, vec![target.clone()], {
        let pool = pool.clone();
        let inner = inner.clone();
        async move {
            seed_holding(&inner, &pool, &Owner::player(&target)).await;
            seed_holding(&inner, &pool, &Owner::player(&survivor)).await;

            let mut conn = pool.acquire().await.unwrap();
            inner.plant_purged_player(&mut conn, &target, "Target#0001").await.unwrap();
            drop(conn);
            assert_claimed_by_the_next_fire(&pool, &target).await;

            let handler = projection::PurgeSweepHandler { inner: inner.clone() };

            let mut tx = pool.begin().await.unwrap();
            let delivery = Delivery { event_id: "inventory:test", tx: AnyTx::new(&mut *tx) };
            handler.call(delivery, purge_payload("some-other-job")).await.unwrap();
            tx.commit().await.unwrap();
            assert_eq!(
                inner.store.list(&Owner::player(&target)).await.unwrap().len(),
                1,
                "a foreign schedule name must not sweep"
            );
            assert_eq!(inner.store.list(&Owner::player(&survivor)).await.unwrap().len(), 1);

            let mut tx2 = pool.begin().await.unwrap();
            let delivery2 = Delivery { event_id: "inventory:test", tx: AnyTx::new(&mut *tx2) };
            handler
                .call(delivery2, purge_payload(projection::PURGE_SCHEDULE_NAME))
                .await
                .unwrap();
            tx2.commit().await.unwrap();
            assert!(inner.store.list(&Owner::player(&target)).await.unwrap().is_empty());
            assert_eq!(
                inner.store.list(&Owner::player(&survivor)).await.unwrap().len(),
                1,
                "the survivor must remain untouched by the real sweep too"
            );
        }
    })
    .await;
}

/// `grant` is blocked by the tombstone PREDICATE, not by "a tombstone exists
/// somewhere": a purged player's grant is Conflict with no holding row, while a
/// DIFFERENT, non-tombstoned player still succeeds.
#[tokio::test]
async fn grant_is_blocked_by_tombstone_predicate_not_presence() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    ensure_schema(&pool).await;
    let purged = unique_uuid(&pool).await;
    let other = unique_uuid(&pool).await;
    let inner = inner_with(pool.clone(), Arc::new(FakeConfig::new(STARTER_ITEM, STARTER_QTY)));

    let mut conn = pool.acquire().await.unwrap();
    inner.plant_purged_player(&mut conn, &purged, "Purged#0001").await.unwrap();
    drop(conn);

    let e = inner
        .grant(Identity::player(&purged), "starter_sword".into(), 1)
        .await
        .unwrap_err();
    assert_eq!(e.status, Status::Conflict);
    assert!(inner.store.list(&Owner::player(&purged)).await.unwrap().is_empty());

    inner
        .grant(Identity::player(&other), "starter_sword".into(), 1)
        .await
        .expect("a different, non-tombstoned player must still succeed");
    assert_eq!(inner.store.list(&Owner::player(&other)).await.unwrap().len(), 1);

    cleanup_owner(&pool, &purged).await;
    cleanup_owner(&pool, &other).await;
    cleanup_purge_row(&pool, &purged).await;
}



fn racer_pool(app: &str) -> PgPool {
    let options = testdb::dsn()
        .parse::<sqlx::postgres::PgConnectOptions>()
        .expect("DATABASE_URL parses")
        .application_name(app);
    sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(Duration::from_secs(60))
        .connect_lazy_with(options)
}

async fn racer_app(pool: &PgPool, what: &str) -> String {
    let app = format!("inventory-{what}-{}", unique_uuid(pool).await);
    // Postgres truncates application_name to 63 bytes, so a longer one never matches the
    // pg_stat_activity filter wait_advisory_parked runs.
    assert!(app.len() <= 63, "racer app name {app} is longer than Postgres keeps");
    app
}

async fn finish<T>(what: &str, handle: tokio::task::JoinHandle<T>) -> T {
    match tokio::time::timeout(Duration::from_secs(30), handle).await {
        Ok(Ok(value)) => value,
        Ok(Err(e)) => panic!("{what} task did not complete: {e}"),
        Err(_) => panic!("{what} did not finish within 30s"),
    }
}

async fn wait_advisory_parked<T>(
    pool: &PgPool,
    app: &str,
    key: i64,
    racer: &tokio::task::JoinHandle<T>,
) -> bool {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        let parked: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM pg_locks l JOIN pg_stat_activity a ON a.pid = l.pid \
              WHERE a.application_name = $1 AND NOT l.granted AND l.locktype = 'advisory' \
                AND l.objsubid = 1 AND ((l.classid::bigint << 32) | l.objid::bigint) = $2",
        )
        .bind(app)
        .bind(key)
        .fetch_one(pool)
        .await
        .unwrap();
        if parked > 0 {
            return true;
        }
        if racer.is_finished() {
            return false;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "racer {app} neither parked on the advisory lock {key} nor finished"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn wait_xact_parked<T>(
    pool: &PgPool,
    app: &str,
    xid: &str,
    racer: &tokio::task::JoinHandle<T>,
) -> bool {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        let parked: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM pg_locks l JOIN pg_stat_activity a ON a.pid = l.pid \
              WHERE a.application_name = $1 AND NOT l.granted AND l.locktype = 'transactionid' \
                AND l.transactionid::text = $2",
        )
        .bind(app)
        .bind(xid)
        .fetch_one(pool)
        .await
        .unwrap();
        if parked > 0 {
            return true;
        }
        if racer.is_finished() {
            return false;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "racer {app} neither parked on the transaction {xid} nor finished"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn gate_holding(gate: &mut sqlx::PgConnection, player_id: &str) -> String {
    sqlx::query(
        "INSERT INTO inventory.holdings (owner_type, owner_id, item_id, quantity) \
         VALUES ('player', $1::uuid, 'starter_sword', 1)",
    )
    .bind(player_id)
    .execute(&mut *gate)
    .await
    .unwrap();
    sqlx::query_scalar("SELECT (pg_current_xact_id()::text::bigint % 4294967296)::text")
        .fetch_one(&mut *gate)
        .await
        .unwrap()
}

async fn held_advisory_key(conn: &mut sqlx::PgConnection) -> i64 {
    let keys: Vec<i64> = sqlx::query_scalar(
        "SELECT (classid::bigint << 32) | objid::bigint FROM pg_locks \
          WHERE locktype = 'advisory' AND objsubid = 1 AND pid = pg_backend_pid()",
    )
    .fetch_all(&mut *conn)
    .await
    .unwrap();
    assert_eq!(keys.len(), 1, "exactly one advisory lock must be held, got {keys:?}");
    keys[0]
}

async fn taken_player_key(pool: &PgPool, inner: &Inner, spelling: &str) -> i64 {
    let mut tx = pool.begin().await.unwrap();
    inner.store.lock_player_tx(&mut tx, spelling).await.unwrap();
    let key = held_advisory_key(&mut tx).await;
    tx.rollback().await.unwrap();
    key
}

async fn race_grant(app: String, player_id: String) -> Result<usize, (Status, String)> {
    let racer = inner_with(racer_pool(&app), Arc::new(FakeConfig::new(STARTER_ITEM, STARTER_QTY)));
    racer
        .grant(Identity::player(&player_id), "starter_sword".into(), 1)
        .await
        .map(|holdings| holdings.len())
        .map_err(|e| (e.status, e.msg))
}

#[tokio::test]
async fn player_lock_key_is_one_per_uuid_across_spellings() {
    let Some(pool) = test_pool().await else { return };
    ensure_schema(&pool).await;
    let inner = inner_with(pool.clone(), Arc::new(FakeConfig::new(STARTER_ITEM, STARTER_QTY)));
    let canon = "a2b7e8c1-0000-4000-8000-0000000000f1";
    let hex = canon.replace('-', "");

    let key = taken_player_key(&pool, &inner, canon).await;
    assert_eq!(key, taken_player_key(&pool, &inner, &canon.to_uppercase()).await, "case must not change the key");
    assert_eq!(
        key,
        taken_player_key(&pool, &inner, &format!("{{{canon}}}")).await,
        "braces must not change the key"
    );
    assert_eq!(key, taken_player_key(&pool, &inner, &hex).await, "hyphens must not change the key");
    assert_ne!(
        key,
        taken_player_key(&pool, &inner, "a2b7e8c1-0000-4000-8000-0000000000f2").await,
        "a genuinely different uuid must take a different key"
    );
    let mut tx = pool.begin().await.unwrap();
    projection::lock_character(&mut tx, canon).await.unwrap();
    let character_key = held_advisory_key(&mut tx).await;
    tx.rollback().await.unwrap();
    assert_ne!(key, character_key, "the player lock must not be the character lock of the same uuid");
    taken_player_key(&pool, &inner, "not-a-uuid").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn sweep_waits_for_an_in_flight_grant() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    ensure_schema(&pool).await;
    let inner = inner_with(pool.clone(), Arc::new(FakeConfig::new(STARTER_ITEM, STARTER_QTY)));
    let target = fixed_player(0x200);
    clear_fixed_player(&pool, &target).await;
    let lock_key = taken_player_key(&pool, &inner, &target).await;
    let gate_app = racer_app(&pool, "race1-gate").await;
    let grant_app = racer_app(&pool, "race1-grant").await;
    let sweep_app = racer_app(&pool, "race1-sweep").await;

    let apps = vec![gate_app.clone(), grant_app.clone(), sweep_app.clone()];
    with_purge_cleanup(&pool, apps, vec![target.clone()], vec![target.clone()], {
        let pool = pool.clone();
        let inner = inner.clone();
        async move {
            let gate_pool = racer_pool(&gate_app);
            let mut gate = gate_pool.begin().await.unwrap();
            let gate_xid = gate_holding(&mut gate, &target).await;

            let grant = tokio::spawn(race_grant(grant_app.clone(), target.clone()));
            assert!(
                wait_xact_parked(&pool, &grant_app, &gate_xid, &grant).await,
                "the grant must be held at its INSERT, past its lock and its tombstone guard"
            );

            let mut conn = pool.acquire().await.unwrap();
            inner.plant_purged_player(&mut conn, &target, "Race#0001").await.unwrap();
            drop(conn);
            assert_claimed_by_the_next_fire(&pool, &target).await;

            let sweep = tokio::spawn({
                let handler = projection::PurgeSweepHandler { inner: inner.clone() };
                let racer = racer_pool(&sweep_app);
                async move {
                    let mut tx = racer.begin().await.unwrap();
                    let delivery = Delivery { event_id: "inventory:race1", tx: AnyTx::new(&mut *tx) };
                    handler
                        .call(delivery, purge_payload(projection::PURGE_SCHEDULE_NAME))
                        .await
                        .unwrap();
                    tx.commit().await.unwrap();
                }
            });
            let parked = wait_advisory_parked(&pool, &sweep_app, lock_key, &sweep).await;

            gate.rollback().await.unwrap();
            assert_eq!(
                finish("race-1 grant", grant).await,
                Ok(1),
                "the grant passed its guard before the tombstone"
            );
            finish("race-1 sweep", sweep).await;

            assert_eq!(holding_count(&pool, &target).await, 0, "the in-flight grant's holding survived the erasure");
            assert!(purge_swept_at(&pool, &target).await.flatten().is_some(), "the fire must stamp swept_at");
            assert!(parked, "the sweep must have parked on the player's advisory lock");
        }
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn grant_parks_behind_an_open_sweep_then_answers_player_deleted() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    ensure_schema(&pool).await;
    let inner = inner_with(pool.clone(), Arc::new(FakeConfig::new(STARTER_ITEM, STARTER_QTY)));
    let target = fixed_player(0x300);
    clear_fixed_player(&pool, &target).await;
    let lock_key = taken_player_key(&pool, &inner, &target).await;
    let sweep_app = racer_app(&pool, "race2-sweep").await;
    let grant_app = racer_app(&pool, "race2-grant").await;

    let apps = vec![sweep_app.clone(), grant_app.clone()];
    with_purge_cleanup(&pool, apps, vec![target.clone()], vec![target.clone()], {
        let pool = pool.clone();
        let inner = inner.clone();
        async move {
            let mut conn = pool.acquire().await.unwrap();
            inner.plant_purged_player(&mut conn, &target, "Race#0002").await.unwrap();
            drop(conn);
            assert_claimed_by_the_next_fire(&pool, &target).await;

            let sweep_pool = racer_pool(&sweep_app);
            let mut sweep_tx = sweep_pool.begin().await.unwrap();
            let handler = projection::PurgeSweepHandler { inner: inner.clone() };
            let delivery = Delivery { event_id: "inventory:race2", tx: AnyTx::new(&mut *sweep_tx) };
            handler
                .call(delivery, purge_payload(projection::PURGE_SCHEDULE_NAME))
                .await
                .unwrap();
            let stamped: bool = sqlx::query_scalar(
                "SELECT swept_at IS NOT NULL FROM inventory.purged_players \
                  WHERE player_id = $1::uuid",
            )
            .bind(&target)
            .fetch_one(&mut *sweep_tx)
            .await
            .unwrap();
            assert!(stamped, "the open sweep must have locked and swept this player");

            let grant = tokio::spawn(race_grant(grant_app.clone(), target.clone()));
            let parked = wait_advisory_parked(&pool, &grant_app, lock_key, &grant).await;
            assert!(parked, "the grant must park on the player's lock while the sweep is open");

            sweep_tx.commit().await.unwrap();
            assert_eq!(
                finish("race-2 grant", grant).await,
                Err((Status::Conflict, crate::service::PLAYER_DELETED.to_string())),
                "a grant to a purged player must answer PLAYER_DELETED"
            );
            assert_eq!(holding_count(&pool, &target).await, 0, "the refused grant must add no holding");
        }
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn grant_takes_the_player_lock_before_it_reads_the_tombstone() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    ensure_schema(&pool).await;
    let inner = inner_with(pool.clone(), Arc::new(FakeConfig::new(STARTER_ITEM, STARTER_QTY)));
    let target = fixed_player(0x500);
    clear_fixed_player(&pool, &target).await;
    let lock_key = taken_player_key(&pool, &inner, &target).await;
    let gate_app = racer_app(&pool, "guard-gate").await;
    let grant_app = racer_app(&pool, "guard-grant").await;

    let apps = vec![gate_app.clone(), grant_app.clone()];
    with_purge_cleanup(&pool, apps, vec![target.clone()], vec![target.clone()], {
        let pool = pool.clone();
        let inner = inner.clone();
        async move {
            assert_eq!(purge_swept_at(&pool, &target).await, None, "precondition: no tombstone yet");
            let gate_pool = racer_pool(&gate_app);
            let mut gate = gate_pool.begin().await.unwrap();
            inner.store.lock_player_tx(&mut gate, &target).await.unwrap();

            let grant = tokio::spawn(race_grant(grant_app.clone(), target.clone()));
            let parked = wait_advisory_parked(&pool, &grant_app, lock_key, &grant).await;
            assert!(parked, "the grant must park on the player's lock before its tombstone read");

            let mut conn = pool.acquire().await.unwrap();
            inner.plant_purged_player(&mut conn, &target, "Guard#0001").await.unwrap();
            drop(conn);
            gate.rollback().await.unwrap();

            assert_eq!(
                finish("guard-order grant", grant).await,
                Err((Status::Conflict, crate::service::PLAYER_DELETED.to_string())),
                "a grant to a purged player must answer PLAYER_DELETED"
            );
            assert_eq!(
                holding_count(&pool, &target).await,
                0,
                "a grant that read the tombstone before the lock would insert after the release"
            );
        }
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn one_sweep_fire_stamps_at_most_64_players() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    ensure_schema(&pool).await;
    let inner = inner_with(pool.clone(), Arc::new(FakeConfig::new(STARTER_ITEM, STARTER_QTY)));
    let ids: Vec<String> = (0x01..0x01 + 65).map(fixed_player).collect();
    for id in &ids {
        cleanup_purge_row(&pool, id).await;
    }
    let foreign_below: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM inventory.purged_players \
          WHERE swept_at IS NULL AND player_id <= $1::uuid",
    )
    .bind(ids.last().unwrap())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(foreign_below, 0, "precondition: a foreign unswept tombstone sorts among this test's ids");

    with_purge_cleanup(&pool, vec![], vec![], ids.clone(), {
        let (pool, inner, ids) = (pool.clone(), inner.clone(), ids.clone());
        async move {
            let mut conn = pool.acquire().await.unwrap();
            for (i, id) in ids.iter().enumerate() {
                inner
                    .plant_purged_player(&mut conn, id, &format!("Cap#{i:04}"))
                    .await
                    .unwrap();
            }
            drop(conn);

            let handler = projection::PurgeSweepHandler { inner: inner.clone() };
            let mut tx = pool.begin().await.unwrap();
            let delivery = Delivery { event_id: "inventory:cap", tx: AnyTx::new(&mut *tx) };
            handler
                .call(delivery, purge_payload(projection::PURGE_SCHEDULE_NAME))
                .await
                .unwrap();
            tx.commit().await.unwrap();
            assert_eq!(
                unstamped_tombstones(&pool, &ids).await,
                vec![ids[64].clone()],
                "one fire must stamp exactly the 64 lowest of 65 tombstones"
            );

            let mut tx2 = pool.begin().await.unwrap();
            let delivery2 = Delivery { event_id: "inventory:cap", tx: AnyTx::new(&mut *tx2) };
            handler
                .call(delivery2, purge_payload(projection::PURGE_SCHEDULE_NAME))
                .await
                .unwrap();
            tx2.commit().await.unwrap();
            assert!(
                unstamped_tombstones(&pool, &ids).await.is_empty(),
                "the next fire must resume and stamp the last one"
            );
        }
    })
    .await;
}

async fn holds_advisory(pool: &PgPool, app: &str, key: i64) -> bool {
    let held: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pg_locks l JOIN pg_stat_activity a ON a.pid = l.pid \
          WHERE a.application_name = $1 AND l.granted AND l.locktype = 'advisory' \
            AND l.objsubid = 1 AND ((l.classid::bigint << 32) | l.objid::bigint) = $2",
    )
    .bind(app)
    .bind(key)
    .fetch_one(pool)
    .await
    .unwrap();
    held > 0
}

fn probe_holds_advisory(app: &str, key: i64) -> bool {
    std::thread::spawn({
        let app = app.to_owned();
        move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(async move {
                    let probe = PgPool::connect(&testdb::dsn()).await.unwrap();
                    holds_advisory(&probe, &app, key).await
                })
        }
    })
    .join()
    .unwrap()
}

#[tokio::test]
async fn grant_releases_the_lock_before_it_answers_a_refusal() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    ensure_schema(&pool).await;
    let inner = inner_with(pool.clone(), Arc::new(FakeConfig::new(STARTER_ITEM, STARTER_QTY)));
    let target = fixed_player(0x400);
    clear_fixed_player(&pool, &target).await;
    let lock_key = taken_player_key(&pool, &inner, &target).await;
    let app = racer_app(&pool, "release").await;

    let mut conn = pool.acquire().await.unwrap();
    inner.plant_purged_player(&mut conn, &target, "Release#0001").await.unwrap();
    drop(conn);

    let racer_db = racer_pool(&app);
    let racer = inner_with(racer_db.clone(), Arc::new(FakeConfig::new(STARTER_ITEM, STARTER_QTY)));
    let mut control = racer_db.begin().await.unwrap();
    racer.store.lock_player_tx(&mut control, &target).await.unwrap();
    assert!(probe_holds_advisory(&app, lock_key), "the probe must see the racer's held player lock");
    control.rollback().await.unwrap();

    let granted = racer.grant(Identity::player(&target), "starter_sword".into(), 1).await;

    let held = probe_holds_advisory(&app, lock_key);
    drop(racer);
    cleanup_purge_row(&pool, &target).await;

    let Err(e) = granted else { panic!("a grant to a purged player must be refused") };
    assert_eq!((e.status, e.msg.as_str()), (Status::Conflict, crate::service::PLAYER_DELETED));
    assert!(!held, "a refused grant must not leave its transaction's lock held");
}

// ---- Operator grant request id retention -------------------------------

/// Wires the module the way `app::run` does (`register` then `init`) over a hand-driven
/// durable transport, and drains once before the test emits anything.
async fn wired_for_delivery(pool: &PgPool) -> (Context, asyncevents::testing::TestTransport) {
    ensure_schema(pool).await;
    let transport = asyncevents::testing::transport(pool.clone());
    let ctx = Context::with_db_and_transport(pool.clone(), transport.handle());
    ctx.registry().provide::<dyn Ownership>(
        key("characters", "ownership"),
        Arc::new(FakeOwnership::Miss) as Arc<dyn Ownership>,
    );
    ctx.registry().provide::<dyn Config>(
        key("config", "reader"),
        Arc::new(FakeConfig::new(STARTER_ITEM, STARTER_QTY)) as Arc<dyn Config>,
    );
    let inv = Inventory::new();
    inv.register(&ctx).unwrap();
    inv.init(&ctx).unwrap();
    transport.deliver_all().await.unwrap();
    (ctx, transport)
}

async fn emit_fired(ctx: &Context, pool: &PgPool, name: &str) {
    let mut tx = pool.begin().await.unwrap();
    let fired = schedulerevents::Fired { name: name.into() };
    ctx.bus()
        .emit_tx(AnyTx::new(&mut *tx), &schedulerevents::FIRED, &fired)
        .await
        .unwrap();
    tx.commit().await.unwrap();
}

/// A handler that returned `Err` leaves `consecutive_failures` above zero, and at 20 the
/// plane pauses the subscription; "the rows went" alone does not rule that out.
async fn assert_subscription_unpoisoned(pool: &PgPool, id: &str) {
    let (state, failures, last_error): (String, i32, Option<String>) = sqlx::query_as(
        "SELECT state, consecutive_failures, last_error FROM asyncevents.subscriptions \
          WHERE subscription_id = $1",
    )
    .bind(id)
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(state, "active", "{id} must still be active");
    assert_eq!(failures, 0, "{id} must not have failed; last_error = {last_error:?}");
}

/// Inserts one operator grant request id aged `age_days`, returning the id.
async fn seed_aged_grant(pool: &PgPool, age_days: i32) -> String {
    let id = format!("grants-prune-test-{}", unique_uuid(pool).await);
    sqlx::query(
        "INSERT INTO inventory.operator_grants (request_id, at) \
         VALUES ($1, now() - make_interval(days => $2))",
    )
    .bind(&id)
    .bind(age_days)
    .execute(pool)
    .await
    .unwrap();
    id
}

async fn grant_kept(pool: &PgPool, request_id: &str) -> bool {
    sqlx::query_scalar::<_, i32>("SELECT 1 FROM inventory.operator_grants WHERE request_id = $1")
        .bind(request_id)
        .fetch_optional(pool)
        .await
        .unwrap()
        .is_some()
}

async fn cleanup_grants(pool: &PgPool, ids: &[&str]) {
    for id in ids {
        let _ = sqlx::query("DELETE FROM inventory.operator_grants WHERE request_id = $1")
            .bind(id)
            .execute(pool)
            .await;
    }
}

/// The prune reached through a REAL delivery of `scheduler.fired`: the subscription is
/// registered on the raw topic and its handler must match the schedule name.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_scheduler_fire_prunes_only_grant_ids_past_retention() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    let (ctx, transport) = wired_for_delivery(&pool).await;
    let old = seed_aged_grant(&pool, OPERATOR_GRANT_RETENTION_DAYS + 5).await;
    let fresh = seed_aged_grant(&pool, 1).await;

    emit_fired(&ctx, &pool, GRANTS_PRUNE_SCHEDULE_NAME).await;
    transport.deliver_all().await.unwrap();

    assert!(!grant_kept(&pool, &old).await, "the request id past retention must go");
    assert!(grant_kept(&pool, &fresh).await, "the request id inside retention must stay");
    assert_subscription_unpoisoned(&pool, GRANTS_PRUNE_SUB.id).await;

    cleanup_grants(&pool, &[&old, &fresh]).await;
}

/// The subscription receives EVERY schedule's fire (a raw sink on the whole topic), so the
/// name guard is the only thing between another schedule's fire and this retention policy.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_foreign_schedule_name_prunes_no_grant_ids() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    let (ctx, transport) = wired_for_delivery(&pool).await;
    let old = seed_aged_grant(&pool, OPERATOR_GRANT_RETENTION_DAYS + 5).await;

    emit_fired(&ctx, &pool, schedulerevents::schedule_names::AUDIT_PRUNE).await;
    transport.deliver_all().await.unwrap();
    assert!(grant_kept(&pool, &old).await, "another schedule's fire must not prune grant ids");

    // The positive control on the SAME row: this module's own name does prune it.
    emit_fired(&ctx, &pool, GRANTS_PRUNE_SCHEDULE_NAME).await;
    transport.deliver_all().await.unwrap();
    assert!(!grant_kept(&pool, &old).await);
    assert_subscription_unpoisoned(&pool, GRANTS_PRUNE_SUB.id).await;

    cleanup_grants(&pool, &[&old]).await;
}

/// The watermarked LOOP, measured by a statement-level trigger counting every DELETE: no
/// single statement may exceed `GRANTS_PRUNE_BATCH`, and one fire keeps going until a short
/// batch. Everything runs in ONE transaction that is rolled back, so the table is untouched.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_fire_loops_batched_grant_deletes_and_never_exceeds_the_batch_per_statement() {
    let Some(pool) = test_pool().await else { return };
    let _serialized = DB_LOCK.lock().await;
    ensure_schema(&pool).await;
    let seeded = GRANTS_PRUNE_BATCH * 2 + 7;

    let mut tx = pool.begin().await.unwrap();
    sqlx::raw_sql(
        // `CREATE TRIGGER` takes SHARE ROW EXCLUSIVE on the table: bounded, so a concurrent
        // long transaction fails this test instead of hanging every other writer.
        "SET LOCAL lock_timeout = '5s'; SET LOCAL statement_timeout = '60s'; \
         CREATE TEMP TABLE grants_prune_probe (seq serial, n bigint) ON COMMIT DROP; \
         CREATE FUNCTION pg_temp.grants_prune_probe_log() RETURNS trigger LANGUAGE plpgsql AS $fn$ \
           BEGIN INSERT INTO grants_prune_probe (n) SELECT count(*) FROM removed; RETURN NULL; END $fn$; \
         CREATE TRIGGER inventory_grants_prune_probe AFTER DELETE ON inventory.operator_grants \
           REFERENCING OLD TABLE AS removed FOR EACH STATEMENT \
           EXECUTE FUNCTION pg_temp.grants_prune_probe_log();",
    )
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO inventory.operator_grants (request_id, at) \
         SELECT 'grants-prune-probe-' || n, now() - make_interval(days => 99) \
           FROM generate_series(1, $1) AS n",
    )
    .bind(seeded)
    .execute(&mut *tx)
    .await
    .unwrap();

    let payload = serde_json::to_vec(&schedulerevents::Fired {
        name: GRANTS_PRUNE_SCHEDULE_NAME.to_string(),
    })
    .unwrap();
    GrantsPruneHandler
        .call(
            Delivery {
                event_id: "grants-prune-probe-event",
                tx: AnyTx::new(&mut *tx),
            },
            payload,
        )
        .await
        .expect("the prune must answer Ok");

    let counts: Vec<i64> = sqlx::query_scalar("SELECT n FROM grants_prune_probe ORDER BY seq")
        .fetch_all(&mut *tx)
        .await
        .unwrap();
    assert!(
        counts.len() >= 3,
        "{seeded} stale rows at {GRANTS_PRUNE_BATCH} per statement must take at least 3 statements; got {counts:?}"
    );
    assert!(
        counts.iter().all(|n| *n <= GRANTS_PRUNE_BATCH),
        "no single statement may delete more than GRANTS_PRUNE_BATCH rows; got {counts:?}"
    );
    assert!(
        *counts.last().unwrap() < GRANTS_PRUNE_BATCH,
        "the loop must end on a SHORT batch; got {counts:?}"
    );
    assert!(
        counts.iter().sum::<i64>() >= seeded,
        "every stale row must be pruned in the one fire; got {counts:?} for {seeded} rows"
    );
    tx.rollback().await.unwrap();
}

// ---- Admin: the player's Inventory tab ----------------------------------

use controlplane::{ActionPoint, DataTable, EntityRef, FilterKind, PanelBody, PanelQuery, Point, Primitive, Provider};

fn admin_query(contribution: &str, pid: &str, params: &[(&str, &str)], search: Option<&str>, cursor: Option<&str>) -> PanelQuery {
    PanelQuery {
        module: "inventory".into(),
        contribution: contribution.into(),
        entity: Some(EntityRef { kind: "player".into(), id: pid.into() }),
        search: search.map(String::from),
        cursor: cursor.map(String::from),
        before: None,
        queue_state: None,
        range: None,
        params: params.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        refs: Vec::new(),
    }
}

fn admin_table(body: &PanelBody) -> &DataTable {
    match &body.primitive {
        Primitive::DataTable(t) => t,
        other => panic!("the Inventory tab is a DataTable, got {other:?}"),
    }
}

/// Each row as (item name, owner cell value), in drawn order.
fn admin_rows(t: &DataTable) -> Vec<(String, String)> {
    t.rows.iter().map(|r| (r.cells[0].value.clone(), r.cells[3].value.clone())).collect()
}

/// The manifest: the tab at 40 declaring its `owner`/`type` select keys, its count, Purges
/// pending at 63, the `holding` row kind, and its three implemented, prepared writes.
#[test]
fn admin_manifest_declares_the_tab_its_keys_and_its_writes() {
    let m = admin::manifest();
    assert!(m.writes);
    let tab = m.contributions.iter().find(|c| c.id == admin::INVENTORY).expect("the tab");
    assert_eq!(tab.order, 40);
    match &tab.point {
        Point::EntityTab { kind, tab, params, .. } => {
            assert_eq!((kind.as_str(), tab.as_str()), ("player", "inventory"));
            assert_eq!(params, &[admin::PARAM_OWNER, admin::PARAM_TYPE]);
        }
        other => panic!("the tab is an EntityTab, got {other:?}"),
    }
    let purges = m.contributions.iter().find(|c| c.id == admin::ATTENTION_PURGES).expect("purges");
    assert_eq!((purges.order, &purges.point), (63, &Point::DashboardAttention));
    assert!(m.contributions.iter().any(|c| c.id == admin::TAB_COUNT));
    assert_eq!(m.entity_kinds.iter().map(|k| k.kind.as_str()).collect::<Vec<_>>(), [admin::HOLDING_KIND]);
    assert_eq!(m.actions.len(), 3);
    assert!(m.actions.iter().all(|a| a.implemented && a.prepare), "{:?}", m.actions);
    let revoke = m.actions.iter().find(|a| a.point == ActionPoint::RowAction(admin::HOLDING_KIND.into())).expect("revoke");
    assert!(revoke.danger);
    let grant = m.actions.iter().find(|a| a.point == ActionPoint::RowAction("player".into())).expect("grant");
    assert_eq!(grant.order, 50);
}

/// The tab draws one Select per param its manifest declares, keyed by that param and in the
/// declared order: the Owner select offers the owner types, the Type select the given kinds.
#[test]
fn admin_inventory_tab_draws_a_select_per_manifest_param_in_order() {
    let m = admin::manifest();
    let tab = m.contributions.iter().find(|c| c.id == admin::INVENTORY).expect("the tab");
    let Point::EntityTab { params, .. } = &tab.point else { panic!("the tab is an EntityTab, got {:?}", tab.point) };
    let body = admin::inventory_body("p-1", &[], 0, None, vec!["consumable".into(), "weapon".into()]);
    let filters = &admin_table(&body).filters;
    assert_eq!(filters.iter().map(|f| f.key.as_str()).collect::<Vec<_>>(), *params);
    let options: Vec<&[String]> = filters
        .iter()
        .map(|f| match &f.kind {
            FilterKind::Select(options) => options.as_slice(),
            other => panic!("the {:?} filter is a Select, got {other:?}", f.key),
        })
        .collect();
    assert_eq!(options, [&["player", "character"][..], &["consumable", "weapon"][..]]);
}

/// The cursor is a holding's row id and parses back; a malformed one, or an unknown owner
/// select, is the caller's error.
#[test]
fn admin_cursor_and_owner_select_are_checked() {
    let key = HoldingKey { owner_type: "character".into(), owner_id: "c-1".into(), item_id: "starter_sword".into() };
    assert_eq!(admin::parse_cursor(&admin::holding_id(&key)).unwrap(), key);
    for bad in ["", "player", "player:x", "guild:x:coin", "player::coin", "player:x:"] {
        assert_eq!(admin::parse_cursor(bad).unwrap_err().status, Status::Invalid, "{bad:?}");
    }
    assert_eq!(admin::owner_filter(None).unwrap(), None, "no owner narrows nothing");
    assert_eq!(admin::owner_filter(Some("character")).unwrap(), Some("character"));
    assert_eq!(admin::owner_filter(Some("guild")).unwrap_err().status, Status::Invalid);
}

/// The Inventory tab lists the player's own holdings and its characters' (not another
/// player's character's), labels a character owner, and the Owner and Type selects and the
/// search each narrow the rows AND the exact count; the tab count stays unfiltered.
#[tokio::test]
async fn admin_inventory_tab_lists_and_filters_narrow_rows_and_count() {
    let Some(pool) = test_pool().await else { return };
    ensure_schema(&pool).await;
    let (pid, other) = (unique_uuid(&pool).await, unique_uuid(&pool).await);
    let (mine, theirs) = (unique_uuid(&pool).await, unique_uuid(&pool).await);
    let inner = inner_with(pool.clone(), Arc::new(FakeConfig::new(STARTER_ITEM, STARTER_QTY)));
    let mut conn = pool.acquire().await.unwrap();
    inner.record_character_owner(&mut conn, &mine, &pid).await.unwrap();
    inner.record_character_owner(&mut conn, &theirs, &other).await.unwrap();
    let grants = [
        (Owner::player(&pid), "coin", 5),
        (Owner::player(&pid), "health_potion", 2),
        (Owner::character(&mine), "starter_sword", 1),
        (Owner::character(&mine), "health_potion", 3),
        (Owner::character(&theirs), "coin", 7),
        (Owner::player(&other), "coin", 9),
    ];
    for (owner, item, qty) in &grants {
        inner.store.grant_exec(&mut conn, owner, item, *qty).await.unwrap();
    }
    let admin = admin::InventoryAdmin::new(inner.clone());
    let tab = |params: &'static [(&'static str, &'static str)], search: Option<&'static str>| {
        let q = admin_query(admin::INVENTORY, &pid, params, search, None);
        let admin = &admin;
        async move { admin.panel(q).await }
    };

    let all = tab(&[], None).await.unwrap();
    let t = admin_table(&all);
    // Primary-key order: 'character' sorts before 'player'.
    let want = vec![
        ("Health Potion".to_string(), mine.clone()),
        ("Starter Sword".to_string(), mine.clone()),
        ("Coin".to_string(), "player".to_string()),
        ("Health Potion".to_string(), "player".to_string()),
    ];
    assert_eq!(admin_rows(t), want);
    assert_eq!(t.page.total, Some(4));
    assert_eq!(all.meta.as_deref(), Some(format!("4 items · owner player:{pid}").as_str()));
    let label = t.rows[0].cells[3].label.as_ref().expect("a character owner is labelled");
    assert_eq!((label.kind.as_str(), label.id.as_str()), ("character", mine.as_str()));
    assert!(t.rows[2].cells[3].label.is_none());
    let row = t.rows[0].entity.as_ref().expect("a holding row");
    assert_eq!((row.kind.as_str(), row.id.clone()), ("holding", format!("character:{mine}:health_potion")));

    type Case = (&'static [(&'static str, &'static str)], Option<&'static str>, Vec<&'static str>);
    let narrowed: [Case; 6] = [
        (&[("owner", "character")], None, vec!["Health Potion", "Starter Sword"]),
        (&[("owner", "player")], None, vec!["Coin", "Health Potion"]),
        (&[("type", "consumable")], None, vec!["Health Potion", "Health Potion"]),
        (&[("owner", "player"), ("type", "consumable")], None, vec!["Health Potion"]),
        (&[], Some("SWORD"), vec!["Starter Sword"]),
        (&[], Some("%"), vec![]),
    ];
    for (params, search, names) in narrowed {
        let body = tab(params, search).await.unwrap();
        let t = admin_table(&body);
        let got: Vec<String> = t.rows.iter().map(|r| r.cells[0].value.clone()).collect();
        assert_eq!(got, names, "{params:?} {search:?}");
        assert_eq!(t.page.total, Some(names.len() as u64), "the count narrows too: {params:?} {search:?}");
        assert!(t.page.next_cursor.is_none());
    }

    let count = admin.panel(admin_query(admin::TAB_COUNT, &pid, &[("owner", "player")], None, None)).await.unwrap();
    match count.primitive {
        Primitive::Cell(c) => assert_eq!(c.value, "4", "the tab count is every holding"),
        other => panic!("the tab count is a Cell, got {other:?}"),
    }
    for params in [&[("owner", "guild")][..], &[("type", "cosmetic")][..]] {
        assert_eq!(tab(params, None).await.unwrap_err().status, Status::Invalid, "{params:?}");
    }
    let bad = admin_query(admin::INVENTORY, &pid, &[], None, Some("player:not-a-uuid:coin"));
    assert_eq!(admin.panel(bad).await.unwrap_err().status, Status::Invalid);

    for id in [&pid, &other, &mine, &theirs] {
        cleanup_owner(&pool, id).await;
    }
}

/// The keyset reader pages in primary-key order after the cursor and answers the exact
/// count on every page, a page past the end included.
#[tokio::test]
async fn admin_holdings_page_keysets_and_counts_every_page() {
    let Some(pool) = test_pool().await else { return };
    ensure_schema(&pool).await;
    let (pid, cid) = (unique_uuid(&pool).await, unique_uuid(&pool).await);
    let inner = inner_with(pool.clone(), Arc::new(FakeConfig::new(STARTER_ITEM, STARTER_QTY)));
    let mut conn = pool.acquire().await.unwrap();
    inner.record_character_owner(&mut conn, &cid, &pid).await.unwrap();
    for (owner, item) in [
        (Owner::player(&pid), "coin"),
        (Owner::player(&pid), "starter_sword"),
        (Owner::character(&cid), "coin"),
    ] {
        inner.store.grant_exec(&mut conn, &owner, item, 1).await.unwrap();
    }
    let all = HoldingsFilter { owner_type: None, kind: None, needle: None };
    let ids = |p: &HoldingsPage| p.rows.iter().map(|h| admin::holding_id(&h.key)).collect::<Vec<_>>();

    let first = inner.store.player_holdings_page(&pid, &all, None, 2).await.unwrap();
    assert_eq!(ids(&first), [format!("character:{cid}:coin"), format!("player:{pid}:coin")]);
    assert_eq!(first.total, 3);
    let after = first.rows.last().map(|h| h.key.clone());
    let second = inner.store.player_holdings_page(&pid, &all, after.as_ref(), 2).await.unwrap();
    assert_eq!(ids(&second), [format!("player:{pid}:starter_sword")]);
    assert_eq!(second.total, 3);
    let after = second.rows.last().map(|h| h.key.clone());
    let past = inner.store.player_holdings_page(&pid, &all, after.as_ref(), 2).await.unwrap();
    assert!(past.rows.is_empty());
    assert_eq!(past.total, 3, "a page past the end still answers the count");

    for id in [&pid, &cid] {
        cleanup_owner(&pool, id).await;
    }
}

/// The search matches the item id OR the name, and `_` is a literal: `r_s` reaches Starter
/// Sword only through its id, `r S` only through its name, and `_` keeps every
/// underscored id and drops Coin (an unescaped `_` would match every row).
#[tokio::test]
async fn admin_inventory_search_matches_id_or_name_and_underscore_is_literal() {
    let Some(pool) = test_pool().await else { return };
    ensure_schema(&pool).await;
    let (pid, cid) = (unique_uuid(&pool).await, unique_uuid(&pool).await);
    let inner = inner_with(pool.clone(), Arc::new(FakeConfig::new(STARTER_ITEM, STARTER_QTY)));
    let mut conn = pool.acquire().await.unwrap();
    inner.record_character_owner(&mut conn, &cid, &pid).await.unwrap();
    for (owner, item) in [
        (Owner::player(&pid), "coin"),
        (Owner::player(&pid), "health_potion"),
        (Owner::character(&cid), "health_potion"),
        (Owner::character(&cid), "starter_sword"),
    ] {
        inner.store.grant_exec(&mut conn, &owner, item, 1).await.unwrap();
    }
    let admin = admin::InventoryAdmin::new(inner.clone());
    let (character, player) = (cid.clone(), "player".to_string());
    let cases = [
        ("r_s", vec![("Starter Sword", &character)]),
        ("r S", vec![("Starter Sword", &character)]),
        ("h P", vec![("Health Potion", &character), ("Health Potion", &player)]),
        ("_", vec![("Health Potion", &character), ("Starter Sword", &character), ("Health Potion", &player)]),
    ];
    for (search, want) in cases {
        let body = admin.panel(admin_query(admin::INVENTORY, &pid, &[], Some(search), None)).await.unwrap();
        let t = admin_table(&body);
        let want: Vec<(String, String)> = want.iter().map(|(n, o)| (n.to_string(), o.to_string())).collect();
        assert_eq!(admin_rows(t), want, "{search:?}");
        assert_eq!(t.page.total, Some(want.len() as u64), "{search:?}");
        assert!(t.page.next_cursor.is_none(), "{search:?}");
    }

    for id in [&pid, &cid] {
        cleanup_owner(&pool, id).await;
    }
}

/// Past `ADMIN_PAGE_SIZE` the tab draws a full page with the exact total and the last drawn
/// row's id as the cursor; fed back, the cursor draws exactly the rest and no cursor.
#[tokio::test]
async fn admin_inventory_tab_pages_past_the_page_size_with_the_cursor_fed_back() {
    let Some(pool) = test_pool().await else { return };
    ensure_schema(&pool).await;
    let pid = unique_uuid(&pool).await;
    let mut chars = Vec::new();
    for _ in 0..8 {
        chars.push(unique_uuid(&pool).await);
    }
    chars.sort();
    let inner = inner_with(pool.clone(), Arc::new(FakeConfig::new(STARTER_ITEM, STARTER_QTY)));
    let mut conn = pool.acquire().await.unwrap();
    let items = ["coin", "health_potion", "starter_sword"];
    let mut want = Vec::new();
    for cid in &chars {
        inner.record_character_owner(&mut conn, cid, &pid).await.unwrap();
        for item in items {
            inner.store.grant_exec(&mut conn, &Owner::character(cid), item, 1).await.unwrap();
            want.push(format!("character:{cid}:{item}"));
        }
    }
    for item in items {
        inner.store.grant_exec(&mut conn, &Owner::player(&pid), item, 1).await.unwrap();
        want.push(format!("player:{pid}:{item}"));
    }
    assert_eq!(want.len(), 27);
    let admin = admin::InventoryAdmin::new(inner.clone());
    let ids = |t: &DataTable| {
        t.rows.iter().map(|r| r.entity.as_ref().expect("a holding row").id.clone()).collect::<Vec<_>>()
    };

    let first = admin.panel(admin_query(admin::INVENTORY, &pid, &[], None, None)).await.unwrap();
    let t = admin_table(&first);
    let page1 = ids(t);
    assert_eq!(page1.len(), crate::store::ADMIN_PAGE_SIZE);
    assert_eq!(t.page.total, Some(27));
    let cursor = t.page.next_cursor.clone().expect("a full page has a cursor");
    assert_eq!(cursor, page1[crate::store::ADMIN_PAGE_SIZE - 1], "the cursor is the last drawn row");

    let second = admin.panel(admin_query(admin::INVENTORY, &pid, &[], None, Some(&cursor))).await.unwrap();
    let t = admin_table(&second);
    let page2 = ids(t);
    assert_eq!(page2.len(), 2);
    assert_eq!(t.page.total, Some(27));
    assert!(t.page.next_cursor.is_none(), "the last page has no cursor");
    assert_eq!([page1, page2].concat(), want, "the pages concatenate to every row once, in order");

    for id in chars.iter().chain([&pid]) {
        cleanup_owner(&pool, id).await;
    }
}

/// The keyset compares the whole row key: after the first character's last holding the next
/// page starts at the second character's first one (a per-column predicate would skip its
/// `coin`, which sorts below the cursor's item), then the player's.
#[tokio::test]
async fn admin_holdings_page_keyset_crosses_characters_then_the_player() {
    let Some(pool) = test_pool().await else { return };
    ensure_schema(&pool).await;
    let pid = unique_uuid(&pool).await;
    let mut chars = [unique_uuid(&pool).await, unique_uuid(&pool).await];
    chars.sort();
    let [a, b] = chars;
    let inner = inner_with(pool.clone(), Arc::new(FakeConfig::new(STARTER_ITEM, STARTER_QTY)));
    let mut conn = pool.acquire().await.unwrap();
    for cid in [&a, &b] {
        inner.record_character_owner(&mut conn, cid, &pid).await.unwrap();
        for item in ["coin", "health_potion"] {
            inner.store.grant_exec(&mut conn, &Owner::character(cid), item, 1).await.unwrap();
        }
    }
    inner.store.grant_exec(&mut conn, &Owner::player(&pid), "coin", 1).await.unwrap();
    let all = HoldingsFilter { owner_type: None, kind: None, needle: None };
    let ids = |p: &HoldingsPage| p.rows.iter().map(|h| admin::holding_id(&h.key)).collect::<Vec<_>>();

    let mut after: Option<HoldingKey> = None;
    let want = [
        vec![format!("character:{a}:coin"), format!("character:{a}:health_potion")],
        vec![format!("character:{b}:coin"), format!("character:{b}:health_potion")],
        vec![format!("player:{pid}:coin")],
    ];
    for (n, page_want) in want.iter().enumerate() {
        let page = inner.store.player_holdings_page(&pid, &all, after.as_ref(), 2).await.unwrap();
        assert_eq!(&ids(&page), page_want, "page {}", n + 1);
        assert_eq!(page.total, 5, "page {}", n + 1);
        after = page.rows.last().map(|h| h.key.clone());
    }

    for id in [&pid, &a, &b] {
        cleanup_owner(&pool, id).await;
    }
}

#[path = "admin_tests.rs"]
mod admin_tests;
