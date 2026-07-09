use super::*;

/// A cacheless service over a lazy pool (never connects until a query) — for
/// unit tests that only read the cache or validate ids before any DB work.
fn lazy_service() -> Arc<Service> {
    Arc::new(Service::new(PgPool::connect_lazy(DEFAULT_DSN).unwrap()))
}

fn preload(svc: &Service, entries: &[(&str, &str, &str)]) {
    let mut guard = svc.cache.write().unwrap();
    for (ns, key, val) in entries {
        guard.insert((ns.to_string(), key.to_string()), val.to_string());
    }
}

/// PULL demonstration: typed getters over a preloaded cache, each hitting and
/// falling back on a miss. No DB.
// `#[tokio::test]`: `PgPool::connect_lazy` needs a Tokio context (it spawns the
// pool's background worker), even though these tests never issue a query.
#[tokio::test]
async fn getters_hit_and_default() {
    use configapi::Config as _;
    let svc = lazy_service();
    preload(
        &svc,
        &[
            ("game", "name", "arena"),
            ("game", "max_players", "8"),
            ("game", "hardcore", "true"),
            ("game", "pvp", "ON"),
        ],
    );

    assert_eq!(svc.get_string("game", "name", "def"), "arena");
    assert_eq!(svc.get_string("game", "missing", "def"), "def");
    assert!(svc.get_bool("game", "hardcore", false));
    assert!(svc.get_bool("game", "pvp", false)); // case-insensitive "on"
    assert!(!svc.get_bool("game", "missing", false));
    assert!(svc.get_bool("game", "missing", true)); // default honoured
    assert_eq!(svc.get_int("game", "max_players", 1), 8);
    assert_eq!(svc.get_int("game", "missing", 3), 3);
    assert_eq!(svc.get_int("game", "name", 5), 5); // parse-fail -> default
    assert_eq!(svc.get("game", "name").as_deref(), Some("arena"));
    assert_eq!(svc.get("game", "missing"), None);
}

/// `set` validates ids BEFORE any DB work, so a bad id errors even over a lazy
/// pool that would fail on connect.
#[tokio::test]
async fn set_rejects_invalid_identifiers() {
    let svc = lazy_service();
    assert!(svc.set("bad ns", "k", "v").await.is_err());
    assert!(svc.set("UPPER", "k", "v").await.is_err());
    assert!(svc.set("ok", "Bad Key", "v").await.is_err());
    assert!(svc.set("", "k", "v").await.is_err());
}

#[test]
fn valid_ident_matches_go_regex() {
    assert!(valid_ident("game"));
    assert!(valid_ident("max_players2"));
    assert!(!valid_ident(""));
    assert!(!valid_ident("Bad"));
    assert!(!valid_ident("a:b"));
    assert!(!valid_ident("a b"));
}

/// `register` provides the reader capability under `"config.reader"`, downcasting
/// back to `Arc<dyn configapi::Config>`. No DB (lazy pool, no query).
#[tokio::test]
async fn register_provides_reader_capability() {
    let ctx = Context::with_db(PgPool::connect_lazy(DEFAULT_DSN).unwrap());
    let m = Config::new();
    m.register(&ctx).unwrap();
    let reader = ctx
        .registry()
        .try_require::<dyn configapi::Config>(&registry::key("config", "reader"));
    assert!(reader.is_some(), "config.reader capability not provided");
    // A miss degrades to the default through the trait object.
    assert_eq!(reader.unwrap().get_string("x", "y", "def"), "def");
}

/// The admin render builds KPIs (Settings + Namespaces counts), a table row per
/// setting, and one form field per setting plus the 3 add-new fields. Exercises
/// the render closure without a DB.
#[tokio::test]
async fn admin_render_builds_widgets_from_cache() {
    let svc = lazy_service();
    preload(
        &svc,
        &[
            ("game", "name", "arena"),
            ("game", "max_players", "8"),
            ("net", "region", "eu"),
        ],
    );
    let content = admin_render(&svc, &adminapi::Params::new()).unwrap();

    assert_eq!(content.kpis[0].label, "Settings");
    assert_eq!(content.kpis[0].value, "3");
    assert_eq!(content.kpis[1].label, "Namespaces");
    assert_eq!(content.kpis[1].value, "2"); // game, net

    let table = content.table.as_ref().unwrap();
    assert_eq!(table.columns, vec!["Namespace", "Key", "Value"]);
    assert_eq!(table.rows.len(), 3);
    assert!(table.rows[0][0].mono); // namespace cell is mono

    let form = content.form.as_ref().unwrap();
    assert_eq!(form.fields.len(), 3 + 3); // per-setting + add-new triple
    assert_eq!(form.fields[3].name, "_new_namespace");
    assert!(form.submit.is_some());
}

// ---- Live Postgres integration (the local DB is the test DB) ----------

/// Opens the local Postgres, migrates the config schema, and returns `None`
/// (printing a skip line) when it's unreachable.
async fn test_pool() -> Option<PgPool> {
    let dsn = std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DSN.to_string());
    let pool = match tokio::time::timeout(Duration::from_secs(3), PgPool::connect(&dsn)).await {
        Ok(Ok(p)) => p,
        _ => {
            eprintln!("SKIP: postgres unreachable at {dsn} — config DB tests skipped");
            return None;
        }
    };
    if let Err(err) = sqlx::raw_sql(SCHEMA_DDL).execute(&pool).await {
        eprintln!("SKIP: config migrate failed: {err}");
        return None;
    }
    Some(pool)
}

/// A fresh, unique namespace that is a VALID identifier (uuid hyphens stripped).
async fn unique_ns(pool: &PgPool) -> String {
    let (ns,): (String,) =
        sqlx::query_as("SELECT 'test_' || replace(gen_random_uuid()::text, '-', '')")
            .fetch_one(pool)
            .await
            .unwrap();
    ns
}

async fn cleanup(pool: &PgPool, ns: &str) {
    let _ = sqlx::query("DELETE FROM config.settings WHERE namespace = $1")
        .bind(ns)
        .execute(pool)
        .await;
    let _ = asyncevents::testing::cleanup_outbox(pool, "namespace", ns).await;
}

/// Serializes the tests that RUN a real config listener or ASSERT on the shared
/// `asyncevents.outbox`. The `config_changed` NOTIFY channel is process-global, so a
/// listener running in one test would pick up another test's `config.settings` write
/// and emit a `config.changed` outbox row for its namespace — contaminating an
/// outbox-count assertion. Holding this lock for the listener/outbox tests guarantees
/// no concurrent listener races their assertions (other DB tests run no listener and
/// never check the outbox, so they don't need it).
static DB_SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Migrates the asyncevents schema (the durable plane's outbox) EXACTLY ONCE per
/// test binary — its `CREATE INDEX`/`CREATE OR REPLACE TRIGGER` deadlock under
/// parallel idempotent re-runs, so serialize them (mirrors the characters tests).
static ASYNCEVENTS_READY: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();

async fn ensure_asyncevents_schema(pool: &PgPool) {
    ASYNCEVENTS_READY
        .get_or_init(|| async {
            let dsn = std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DSN.to_string());
            asyncevents::Plane::new(pool.clone(), dsn)
                .unwrap()
                .migrate()
                .await
                .unwrap();
        })
        .await;
}

/// Builds a config module wired against a LIVE durable plane: config registers + inits
/// on a ctx whose bus already carries the injected `bus::Transport` (the caller builds it
/// via `Context::with_db_and_transport`, needed before the listener's `emit_tx`). The
/// caller must also have migrated the asyncevents schema (`ensure_asyncevents_schema`).
fn build_module(ctx: &Context) -> Config {
    let m = Config::new();
    m.register(ctx).unwrap();
    m.init(ctx).unwrap();
    m
}

/// A ctx over the live pool with the asyncevents transport injected at construction —
/// the shape `app::run` builds, and what the durable-listener tests need before `start`.
fn wired_ctx(pool: &PgPool) -> Context {
    let transport = asyncevents::transport(pool.clone(), "test-origin");
    Context::with_db_and_transport(pool.clone(), transport)
}

/// Counts durable `config.changed` outbox rows for a namespace — the Step-5 durable
/// publish replaces the old sync-bus assertion.
async fn changed_outbox_count(pool: &PgPool, ns: &str) -> i64 {
    asyncevents::testing::outbox_count(pool, "config.changed", "namespace", ns)
        .await
        .unwrap()
}

/// The DB round-trip: `set` persists, a fresh `load_all` + `replace_cache` makes
/// the value readable through the getters.
#[tokio::test]
async fn set_then_load_reads_back() {
    use configapi::Config as _;
    let Some(pool) = test_pool().await else { return };
    let ns = unique_ns(&pool).await;
    let svc = Arc::new(Service::new(pool.clone()));

    svc.set(&ns, "limit", "42").await.unwrap();
    let all = svc.load_all().await.unwrap();
    svc.replace_cache(all);

    assert_eq!(svc.get(&ns, "limit").as_deref(), Some("42"));
    assert_eq!(svc.get_int(&ns, "limit", 0), 42);
    cleanup(&pool, &ns).await;
}

/// The REAL push path end-to-end: a running listener + `set` -> pg_notify ->
/// listener -> cache refresh AND a DURABLE `config.changed` (Step 5: an outbox row,
/// not a sync-bus event). Also proves the BOOT load is silent (a pre-seeded key
/// writes no outbox row) while a POST-boot set does. Observed by polling the cache
/// and the `asyncevents.outbox`.
#[tokio::test]
async fn live_reload_updates_cache_and_emits_changed() {
    use configapi::Config as _;
    let Some(pool) = test_pool().await else { return };
    ensure_asyncevents_schema(&pool).await;
    let _serial = DB_SERIAL.lock().await;
    let ns = unique_ns(&pool).await;

    // Pre-seed a key so the boot load has something to (silently) load.
    sqlx::query("INSERT INTO config.settings (namespace, key, value) VALUES ($1, 'sentinel', '1')")
        .bind(&ns)
        .execute(&pool)
        .await
        .unwrap();

    let ctx = wired_ctx(&pool); // durable transport injected at construction
    let m = build_module(&ctx);

    m.start(&ctx).await.unwrap();

    // Wait for the listener to connect + boot-load (cache reflects the sentinel).
    let svc = m.svc();
    wait_until(|| svc.get(&ns, "sentinel").as_deref() == Some("1")).await;

    // Boot load emitted NOTHING (reconnect=false): no config.changed outbox row.
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(
        changed_outbox_count(&pool, &ns).await,
        0,
        "boot load must write no config.changed outbox rows"
    );

    // A POST-boot set DOES emit durably: cache refresh + one outbox row.
    svc.set(&ns, "flag", "on").await.unwrap();

    wait_until_async(|| {
        let pool = pool.clone();
        let ns = ns.clone();
        async move { changed_outbox_count(&pool, &ns).await == 1 }
    })
    .await;
    assert_eq!(svc.get(&ns, "flag").as_deref(), Some("on"));
    assert!(svc.get_bool(&ns, "flag", false));

    // Clean shutdown, then remove the rows so reruns are stable.
    m.stop(&ctx).await.unwrap();
    cleanup(&pool, &ns).await;
}

/// The reconnect self-heal, deterministic (no listener timing): after a disconnect
/// PG does not replay missed NOTIFYs, so the reload DURABLY emits `config.changed`
/// for the gap-changed key. Drives the SAME `reload_and_heal` the listener uses. Also
/// asserts the boot reload (`reconnect=false`) writes no outbox row.
#[tokio::test]
async fn reconnect_reload_heals_gap_changes_boot_is_silent() {
    let Some(pool) = test_pool().await else { return };
    ensure_asyncevents_schema(&pool).await;
    let _serial = DB_SERIAL.lock().await;
    let ns = unique_ns(&pool).await;
    let ctx = wired_ctx(&pool); // durable transport injected at construction
    let m = build_module(&ctx);
    let svc = m.svc();
    let bus = ctx.bus().clone();

    // Seed a key, then boot-load (reconnect=false emits nothing).
    sqlx::query("INSERT INTO config.settings (namespace, key, value) VALUES ($1, 'level', '1')")
        .bind(&ns)
        .execute(&pool)
        .await
        .unwrap();
    reload_and_heal(&svc, &bus, false).await.unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(
        changed_outbox_count(&pool, &ns).await,
        0,
        "boot reload must write no config.changed outbox rows"
    );

    // A write missed during a disconnect (no NOTIFY to a dead session).
    sqlx::query("UPDATE config.settings SET value = '2' WHERE namespace = $1 AND key = 'level'")
        .bind(&ns)
        .execute(&pool)
        .await
        .unwrap();

    // The reconnect reload heals: it durably emits config.changed for the gap key.
    reload_and_heal(&svc, &bus, true).await.unwrap();
    assert_eq!(
        changed_outbox_count(&pool, &ns).await,
        1,
        "reconnect reload must write one config.changed outbox row for the gap key"
    );

    cleanup(&pool, &ns).await;
}

/// The admin `apply_edit` submit closure end-to-end: rendering yields a Form whose
/// submit inserts an add-new triple; with a running listener the new key
/// propagates back into the cache via the real push path.
#[tokio::test]
async fn admin_apply_edit_inserts_new_triple() {
    use configapi::Config as _;
    let Some(pool) = test_pool().await else { return };
    ensure_asyncevents_schema(&pool).await;
    let _serial = DB_SERIAL.lock().await;
    let ns = unique_ns(&pool).await;
    let ctx = wired_ctx(&pool); // durable transport injected at construction
    let m = build_module(&ctx);
    m.start(&ctx).await.unwrap();
    let svc = m.svc();

    // Render, then invoke the Form's submit with an add-new triple.
    let content = admin_render(&svc, &adminapi::Params::new()).unwrap();
    let submit = content.form.unwrap().submit.unwrap();
    let mut values = adminapi::Params::new();
    values.insert("_new_namespace".into(), ns.clone());
    values.insert("_new_key".into(), "spawned".into());
    values.insert("_new_value".into(), "yes".into());
    submit(values).await.unwrap();

    // The listener's push path lands the new key in the cache.
    wait_until(|| svc.get(&ns, "spawned").as_deref() == Some("yes")).await;
    assert_eq!(svc.get_string(&ns, "spawned", "def"), "yes");

    m.stop(&ctx).await.unwrap();
    cleanup(&pool, &ns).await;
}

/// Polls `cond` up to 3s at 20ms intervals; panics on timeout.
async fn wait_until(mut cond: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    loop {
        if cond() {
            return;
        }
        if std::time::Instant::now() > deadline {
            panic!("condition not met within deadline");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Async variant of [`wait_until`]: polls an async predicate (e.g. a DB count) up to
/// 3s at 20ms intervals; panics on timeout.
async fn wait_until_async<Fut>(mut cond: impl FnMut() -> Fut)
where
    Fut: std::future::Future<Output = bool>,
{
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    loop {
        if cond().await {
            return;
        }
        if std::time::Instant::now() > deadline {
            panic!("async condition not met within deadline");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
