use super::*;

use std::time::Duration;

use sqlx::postgres::PgListener;
use sqlx::Row;

/// A cacheless service over a lazy pool (never connects until a query) — for
/// unit tests that only read the cache or validate ids before any DB work.
fn lazy_service() -> Arc<Service> {
    Arc::new(Service::new(PgPool::connect_lazy(DEFAULT_DSN).unwrap()))
}

/// Preloads one coherent read-cache revision for unit tests that never touch the DB.
fn preload(svc: &Service, entries: &[(&str, &str, &str)]) {
    let mut guard = svc.cache.write().unwrap();
    guard.revision = 7;
    for (ns, key, val) in entries {
        guard
            .map
            .insert((ns.to_string(), key.to_string()), val.to_string());
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

/// The revision gate: [`Service::apply`] installs a snapshot only when its revision
/// is strictly newer than the one held, so a stale/duplicate refresh is a no-op. No
/// DB — drives `apply` directly.
#[tokio::test]
async fn apply_installs_only_newer_revisions() {
    use configapi::Config as _;
    let svc = lazy_service();

    svc.apply(5, vec![setting("game", "name", "v5")]);
    assert_eq!(svc.get("game", "name").as_deref(), Some("v5"));

    // A STALE snapshot (older revision) is ignored, even though its contents differ.
    svc.apply(3, vec![setting("game", "name", "v3")]);
    assert_eq!(svc.get("game", "name").as_deref(), Some("v5"), "stale revision must not apply");

    // A DUPLICATE (same revision) is also a no-op.
    svc.apply(5, vec![setting("game", "name", "dup")]);
    assert_eq!(svc.get("game", "name").as_deref(), Some("v5"), "duplicate revision must not apply");

    // A newer revision applies (and a dropped key disappears — full-map swap).
    svc.apply(6, vec![setting("game", "region", "eu")]);
    assert_eq!(svc.get("game", "region").as_deref(), Some("eu"));
    assert_eq!(svc.get("game", "name"), None, "full-map swap drops removed keys");
}

fn setting(ns: &str, key: &str, value: &str) -> Setting {
    Setting {
        namespace: ns.to_string(),
        key: key.to_string(),
        value: value.to_string(),
    }
}

fn form_values(form: &adminapi::Form) -> adminapi::Params {
    form.fields
        .iter()
        .map(|field| (field.name.clone(), field.value.clone()))
        .chain(
            form.hidden
                .iter()
                .map(|field| (field.name.clone(), field.value.clone())),
        )
        .collect()
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
    assert_eq!(form.hidden.len(), 1);
    assert_eq!(form.hidden[0].name, "_expected_revision");
    assert_eq!(form.hidden[0].value, "7");
    assert!(form.submit.is_some());
}

// ---- Live Postgres integration (the local DB is the test DB) ----------

/// Migrates the asyncevents schema (the durable plane's event log) EXACTLY ONCE per
/// test binary — its `CREATE INDEX`/`CREATE OR REPLACE TRIGGER` deadlock under
/// parallel idempotent re-runs, so serialize them (mirrors the characters tests).
static ASYNCEVENTS_READY: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();

/// Config's optimistic token is the one global revision, so DB-writing tests must not
/// invalidate each other's freshly rendered forms inside this test binary.
static CONFIG_DB_TEST_GUARD: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

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

/// Opens the local Postgres and migrates BOTH the asyncevents plane (so the config
/// trigger's `asyncevents.append_event` call resolves on every `config.settings` write)
/// and the config schema; returns `None` (printing a skip line) when Postgres is
/// unreachable.
async fn test_pool() -> Option<PgPool> {
    let dsn = std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DSN.to_string());
    let pool = match tokio::time::timeout(Duration::from_secs(3), PgPool::connect(&dsn)).await {
        Ok(Ok(p)) => p,
        _ => {
            eprintln!("SKIP: postgres unreachable at {dsn} — config DB tests skipped");
            return None;
        }
    };
    ensure_asyncevents_schema(&pool).await;
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
    let _ = asyncevents::testing::cleanup_events(pool, "namespace", ns).await;
}

/// Counts durable `config.changed` log events for a namespace.
async fn changed_event_count(pool: &PgPool, ns: &str) -> i64 {
    asyncevents::testing::events_count(pool, "config.changed", "namespace", ns)
        .await
        .unwrap()
}

async fn stored_value(pool: &PgPool, ns: &str, key: &str) -> Option<String> {
    sqlx::query_scalar("SELECT value FROM config.settings WHERE namespace = $1 AND key = $2")
        .bind(ns)
        .bind(key)
        .fetch_optional(pool)
        .await
        .unwrap()
}

/// The DB round-trip: `set` persists, a fresh `load_snapshot` + `apply` makes the
/// value readable through the getters at the revision the write produced.
#[tokio::test]
async fn set_then_snapshot_reads_back() {
    use configapi::Config as _;
    let _guard = CONFIG_DB_TEST_GUARD.lock().await;
    let Some(pool) = test_pool().await else { return };
    let ns = unique_ns(&pool).await;
    let svc = Arc::new(Service::new(pool.clone()));

    svc.set(&ns, "limit", "42").await.unwrap();
    let (revision, settings) = svc.load_snapshot().await.unwrap();
    assert!(revision >= 1, "the first write must produce a revision >= 1");
    svc.apply(revision, settings);

    assert_eq!(svc.get(&ns, "limit").as_deref(), Some("42"));
    assert_eq!(svc.get_int(&ns, "limit", 0), 42);
    cleanup(&pool, &ns).await;
}

/// One mutation ⇒ exactly one durable `config.changed` event (no double emission),
/// across INSERT/UPDATE/DELETE. The per-namespace events carry the operation, the new
/// value (`null` on DELETE), and a STRICTLY increasing revision — read straight off the
/// log, so the assertions are race-free against concurrent tests bumping the singleton.
#[tokio::test]
async fn each_mutation_emits_one_event_with_operation_value_and_revision() {
    let _guard = CONFIG_DB_TEST_GUARD.lock().await;
    let Some(pool) = test_pool().await else { return };
    let ns = unique_ns(&pool).await;
    let svc = Arc::new(Service::new(pool.clone()));

    // The trigger runs in each write's autocommit tx, so the event is committed by the
    // time the write returns — no polling.
    svc.set(&ns, "k", "1").await.unwrap(); // insert
    svc.set(&ns, "k", "2").await.unwrap(); // update
    sqlx::query("DELETE FROM config.settings WHERE namespace = $1 AND key = 'k'")
        .bind(&ns)
        .execute(&pool)
        .await
        .unwrap(); // delete

    assert_eq!(
        changed_event_count(&pool, &ns).await,
        3,
        "exactly one event per mutation (no double emission)"
    );

    let rows = sqlx::query(
        "SELECT payload->>'operation' AS op, payload->>'value' AS val, \
                (payload->>'revision')::bigint AS rev \
         FROM asyncevents.events WHERE topic = 'config.changed' AND payload->>'namespace' = $1 \
         ORDER BY generation, producer_xid, tie_breaker",
    )
    .bind(&ns)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(rows.len(), 3);

    let ops: Vec<String> = rows.iter().map(|r| r.get::<String, _>("op")).collect();
    assert_eq!(ops, vec!["insert", "update", "delete"]);

    // DELETE carries `value: null` (a NULL jsonb field → SQL NULL out of `->>`).
    assert_eq!(rows[0].get::<Option<String>, _>("val").as_deref(), Some("1"));
    assert_eq!(rows[1].get::<Option<String>, _>("val").as_deref(), Some("2"));
    assert_eq!(rows[2].get::<Option<String>, _>("val"), None, "DELETE value must be null");

    let revs: Vec<i64> = rows.iter().map(|r| r.get::<i64, _>("rev")).collect();
    assert!(revs[0] < revs[1] && revs[1] < revs[2], "revision must strictly increment: {revs:?}");

    cleanup(&pool, &ns).await;
}

/// A config value larger than `pg_notify`'s 8000-byte payload cap must NOT abort the
/// writing transaction: the NOTIFY payload is value-less by design (the invalidation
/// callback re-reads the whole snapshot and never reads the NOTIFY payload itself), so
/// only the durable `config.changed` event — not the NOTIFY — carries `value`. Confirms
/// the revision still increments, the oversized value is stored and readable back
/// through the getters, and a normal (small) write still refreshes fine afterwards.
#[tokio::test]
async fn large_value_write_does_not_abort_on_notify_cap() {
    use configapi::Config as _;
    let _guard = CONFIG_DB_TEST_GUARD.lock().await;
    let Some(pool) = test_pool().await else { return };
    let ns = unique_ns(&pool).await;
    let svc = Arc::new(Service::new(pool.clone()));

    // Comfortably over pg_notify's 8000-byte payload cap; the OLD code (value inside the
    // NOTIFY payload) would RAISE and abort this write's transaction.
    let big_value = "x".repeat(10_000);

    svc.set(&ns, "big", &big_value)
        .await
        .expect("a write with a >8KB value must not abort");

    let (revision, settings) = svc.load_snapshot().await.unwrap();
    assert!(revision >= 1, "the write must produce a revision >= 1");
    svc.apply(revision, settings);
    assert_eq!(svc.get(&ns, "big").as_deref(), Some(big_value.as_str()));

    // The durable `config.changed` event still carries the FULL value — only the
    // (unconsumed) NOTIFY payload dropped it.
    let (val,): (Option<String>,) = sqlx::query_as(
        "SELECT payload->>'value' FROM asyncevents.events \
         WHERE topic = 'config.changed' AND payload->>'namespace' = $1 AND payload->>'key' = 'big' \
         ORDER BY generation, producer_xid, tie_breaker DESC LIMIT 1",
    )
    .bind(&ns)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(val.as_deref(), Some(big_value.as_str()), "durable event must keep the full value");

    // A normal small write on the same namespace still round-trips (revision/refresh not
    // wedged by the earlier large write).
    svc.set(&ns, "small", "ok").await.unwrap();
    let (revision2, settings2) = svc.load_snapshot().await.unwrap();
    assert!(revision2 > revision, "a later write must yield a strictly greater revision");
    svc.apply(revision2, settings2);
    assert_eq!(svc.get(&ns, "small").as_deref(), Some("ok"));

    cleanup(&pool, &ns).await;
}

/// `snapshot()` reports the revision and settings from ONE statement: the revision
/// tracks writes (a second write yields a strictly greater snapshot revision) and the
/// settings reflect the store.
#[tokio::test]
async fn snapshot_reports_revision_and_settings() {
    let _guard = CONFIG_DB_TEST_GUARD.lock().await;
    let Some(pool) = test_pool().await else { return };
    let ns = unique_ns(&pool).await;
    let svc = Arc::new(Service::new(pool.clone()));

    svc.set(&ns, "a", "1").await.unwrap();
    let snap1 = configapi::ConfigSnapshot::snapshot(&*svc).await.unwrap();
    let mine = |s: &configapi::Setting| s.namespace == ns;
    assert!(
        snap1.settings.iter().filter(|s| mine(s)).any(|s| s.key == "a" && s.value == "1"),
        "snapshot must include the just-written setting"
    );
    assert!(snap1.revision >= 1);

    svc.set(&ns, "b", "2").await.unwrap();
    let snap2 = configapi::ConfigSnapshot::snapshot(&*svc).await.unwrap();
    assert!(
        snap2.revision > snap1.revision,
        "a later write must yield a strictly greater snapshot revision ({} !> {})",
        snap2.revision,
        snap1.revision
    );

    cleanup(&pool, &ns).await;
}

/// A raw SQL write (the psql / another-service path — NOT `Service::set`) still fires
/// the trigger: it produces the durable event AND a `config_changed` NOTIFY, so a
/// config cache LISTENing on the invalidation channel refreshes regardless of who wrote.
#[tokio::test]
async fn psql_style_write_emits_event_and_notify() {
    let _guard = CONFIG_DB_TEST_GUARD.lock().await;
    let Some(pool) = test_pool().await else { return };
    let dsn = std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DSN.to_string());
    let ns = unique_ns(&pool).await;

    let mut listener = PgListener::connect(&dsn).await.unwrap();
    listener.listen(NOTIFY_CHANNEL).await.unwrap();

    // A plain SQL write, exactly what `psql` (or a peer service) issues.
    sqlx::query("INSERT INTO config.settings (namespace, key, value) VALUES ($1, 'raw', 'yes')")
        .bind(&ns)
        .execute(&pool)
        .await
        .unwrap();

    // The channel is process-global (concurrent tests write other namespaces), so
    // recv until we see OUR namespace's notification, or time out.
    let mut got = false;
    for _ in 0..50 {
        match tokio::time::timeout(Duration::from_millis(200), listener.recv()).await {
            Ok(Ok(notif)) => {
                if notif.payload().contains(&ns) {
                    assert!(
                        notif.payload().contains("\"operation\""),
                        "NOTIFY payload must carry the operation: {}",
                        notif.payload()
                    );
                    got = true;
                    break;
                }
            }
            _ => break, // recv error or timeout
        }
    }
    assert!(got, "a raw write did not deliver a config_changed NOTIFY for {ns}");
    assert_eq!(
        changed_event_count(&pool, &ns).await,
        1,
        "a raw write must append exactly one durable event"
    );

    cleanup(&pool, &ns).await;
}

/// The trigger and the native Rust writer are ONE writer protocol: both stamp
/// `producer_xid = pg_current_xact_id()` of their own transaction, read the same
/// generation under the shared advisory lock, and order same-tx appends by
/// `tie_breaker`. Drives both in controlled transactions and asserts identical
/// position/locking semantics.
#[tokio::test]
async fn trigger_and_native_writer_share_position_semantics() {
    let _guard = CONFIG_DB_TEST_GUARD.lock().await;
    let Some(pool) = test_pool().await else { return };
    let ns = unique_ns(&pool).await;

    let generation: i64 =
        sqlx::query_scalar("SELECT generation FROM asyncevents.plane_meta WHERE singleton")
            .fetch_one(&pool)
            .await
            .unwrap();

    // Writer B — the config trigger: two settings in ONE transaction. Both events must
    // carry this tx's xid and increasing tie_breakers (append order).
    let mut tx_b = pool.begin().await.unwrap();
    sqlx::query("INSERT INTO config.settings (namespace, key, value) VALUES ($1, 'a', '1')")
        .bind(&ns)
        .execute(&mut *tx_b)
        .await
        .unwrap();
    sqlx::query("INSERT INTO config.settings (namespace, key, value) VALUES ($1, 'b', '2')")
        .bind(&ns)
        .execute(&mut *tx_b)
        .await
        .unwrap();
    let xid_b: String = sqlx::query_scalar("SELECT pg_current_xact_id()::text")
        .fetch_one(&mut *tx_b)
        .await
        .unwrap();
    tx_b.commit().await.unwrap();

    // Writer A — the native Rust writer (`store::append`) via config.changed's contract,
    // in its own transaction. Different xid, same generation and codec.
    let mut tx_a = pool.begin().await.unwrap();
    let payload = serde_json::to_vec(&configevents::Changed {
        namespace: ns.clone(),
        key: "native".into(),
        value: Some("3".into()),
        operation: "insert".into(),
        revision: 0,
    })
    .unwrap();
    asyncevents::store::append(&mut tx_a, configevents::CHANGED.contract(), &payload)
        .await
        .unwrap();
    let xid_a: String = sqlx::query_scalar("SELECT pg_current_xact_id()::text")
        .fetch_one(&mut *tx_a)
        .await
        .unwrap();
    tx_a.commit().await.unwrap();

    let rows = sqlx::query(
        "SELECT payload->>'key' AS key, producer_xid::text AS xid, generation, tie_breaker \
         FROM asyncevents.events WHERE topic = 'config.changed' AND payload->>'namespace' = $1 \
         ORDER BY generation, producer_xid, tie_breaker",
    )
    .bind(&ns)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(rows.len(), 3, "two trigger events + one native event");

    let by_key = |k: &str| rows.iter().find(|r| r.get::<String, _>("key") == k).unwrap();
    let (a, b, native) = (by_key("a"), by_key("b"), by_key("native"));

    // Every writer reads the SAME generation under the shared advisory lock.
    for r in [a, b, native] {
        assert_eq!(r.get::<i64, _>("generation"), generation, "generation mismatch across writers");
    }
    // xid = pg_current_xact_id() of the writing tx: the two trigger events share tx B's
    // xid; the native event carries tx A's xid; the two are distinct transactions.
    assert_eq!(a.get::<String, _>("xid"), xid_b);
    assert_eq!(b.get::<String, _>("xid"), xid_b);
    assert_eq!(native.get::<String, _>("xid"), xid_a);
    assert_ne!(xid_a, xid_b, "distinct transactions must have distinct xids");
    // tie_breaker orders same-tx appends: 'a' was inserted before 'b'.
    assert!(
        a.get::<i64, _>("tie_breaker") < b.get::<i64, _>("tie_breaker"),
        "tie_breaker must order same-tx appends"
    );

    cleanup(&pool, &ns).await;
}

/// The admin `apply_edit` submit closure end-to-end: rendering yields a Form whose
/// submit inserts an add-new triple; a subsequent authoritative `refresh` (the
/// invalidation callback's body) lands the new key in the cache.
#[tokio::test]
async fn admin_apply_edit_fresh_revision_inserts_new_triple() {
    use configapi::Config as _;
    let _guard = CONFIG_DB_TEST_GUARD.lock().await;
    let Some(pool) = test_pool().await else { return };
    let ns = unique_ns(&pool).await;
    let svc = Arc::new(Service::new(pool.clone()));

    svc.refresh().await.unwrap();
    let form = admin_render(&svc, &adminapi::Params::new())
        .unwrap()
        .form
        .unwrap();
    let mut values = form_values(&form);
    let submit = form.submit.unwrap();
    values.insert("_new_namespace".into(), ns.clone());
    values.insert("_new_key".into(), "spawned".into());
    values.insert("_new_value".into(), "yes".into());
    submit(values).await.unwrap();

    // The refresh callback re-reads the snapshot and applies it.
    svc.refresh().await.unwrap();
    assert_eq!(svc.get_string(&ns, "spawned", "def"), "yes");

    cleanup(&pool, &ns).await;
}

/// A same-key write committed after GET makes the whole form stale. The admin's
/// attempted change to a second key must not partially land or append an event.
#[tokio::test]
async fn admin_apply_edit_same_key_stale_conflicts_without_partial_writes() {
    let _guard = CONFIG_DB_TEST_GUARD.lock().await;
    let Some(pool) = test_pool().await else { return };
    let ns = unique_ns(&pool).await;
    let svc = Arc::new(Service::new(pool.clone()));

    svc.set(&ns, "changed", "old").await.unwrap();
    svc.set(&ns, "untouched", "old").await.unwrap();
    svc.refresh().await.unwrap();
    let form = admin_render(&svc, &adminapi::Params::new())
        .unwrap()
        .form
        .unwrap();
    let mut values = form_values(&form);
    values.insert(format!("{ns}:changed"), "stale-admin".into());
    values.insert(format!("{ns}:untouched"), "must-not-land".into());

    svc.set(&ns, "changed", "concurrent").await.unwrap();
    let events_after_concurrent = changed_event_count(&pool, &ns).await;
    let err = apply_edit(&svc, values).await.unwrap_err();
    assert!(matches!(err, adminapi::SubmitError::Conflict));

    assert_eq!(stored_value(&pool, &ns, "changed").await.as_deref(), Some("concurrent"));
    assert_eq!(stored_value(&pool, &ns, "untouched").await.as_deref(), Some("old"));
    assert_eq!(
        changed_event_count(&pool, &ns).await,
        events_after_concurrent,
        "the stale submit must append no config.changed event"
    );

    cleanup(&pool, &ns).await;
}

/// The page represents the whole config snapshot, so even a write in an unrelated
/// namespace invalidates its global revision token conservatively.
#[tokio::test]
async fn admin_apply_edit_unrelated_revision_conflicts_without_target_event() {
    let _guard = CONFIG_DB_TEST_GUARD.lock().await;
    let Some(pool) = test_pool().await else { return };
    let ns = unique_ns(&pool).await;
    let other_ns = unique_ns(&pool).await;
    let svc = Arc::new(Service::new(pool.clone()));

    svc.set(&ns, "target", "old").await.unwrap();
    svc.refresh().await.unwrap();
    let form = admin_render(&svc, &adminapi::Params::new())
        .unwrap()
        .form
        .unwrap();
    let mut values = form_values(&form);
    values.insert(format!("{ns}:target"), "must-not-land".into());
    let target_events = changed_event_count(&pool, &ns).await;

    svc.set(&other_ns, "unrelated", "new").await.unwrap();
    let err = apply_edit(&svc, values).await.unwrap_err();
    assert!(matches!(err, adminapi::SubmitError::Conflict));
    assert_eq!(stored_value(&pool, &ns, "target").await.as_deref(), Some("old"));
    assert_eq!(
        changed_event_count(&pool, &ns).await,
        target_events,
        "an unrelated stale conflict must emit nothing for the target namespace"
    );

    cleanup(&pool, &ns).await;
    cleanup(&pool, &other_ns).await;
}

/// Missing or malformed expected revisions never degrade into an unguarded write.
#[tokio::test]
async fn admin_apply_edit_requires_parseable_expected_revision() {
    let _guard = CONFIG_DB_TEST_GUARD.lock().await;
    let Some(pool) = test_pool().await else { return };
    let ns = unique_ns(&pool).await;
    let svc = Arc::new(Service::new(pool.clone()));
    svc.refresh().await.unwrap();

    let form = admin_render(&svc, &adminapi::Params::new())
        .unwrap()
        .form
        .unwrap();
    let base = form_values(&form);
    for malformed in [None, Some("not-a-revision")] {
        let mut values = base.clone();
        match malformed {
            None => {
                values.remove("_expected_revision");
            }
            Some(value) => {
                values.insert("_expected_revision".into(), value.into());
            }
        }
        values.insert("_new_namespace".into(), ns.clone());
        values.insert("_new_key".into(), "blocked".into());
        values.insert("_new_value".into(), "no".into());
        let err = apply_edit(&svc, values).await.unwrap_err();
        assert!(matches!(err, adminapi::SubmitError::Conflict));
    }

    assert_eq!(stored_value(&pool, &ns, "blocked").await, None);
    assert_eq!(changed_event_count(&pool, &ns).await, 0);
    cleanup(&pool, &ns).await;
}

/// Two submits carrying the same fresh token serialize on the settings-table lock:
/// exactly one commits, the waiter observes the bumped revision and conflicts, and
/// both finish within a bounded interval (the lock order has no deadlock cycle).
#[tokio::test(flavor = "multi_thread")]
async fn concurrent_admin_submits_yield_one_success_and_one_conflict() {
    let _guard = CONFIG_DB_TEST_GUARD.lock().await;
    let Some(pool) = test_pool().await else { return };
    let ns = unique_ns(&pool).await;
    let svc = Arc::new(Service::new(pool.clone()));

    svc.set(&ns, "race", "old").await.unwrap();
    svc.refresh().await.unwrap();
    let form = admin_render(&svc, &adminapi::Params::new())
        .unwrap()
        .form
        .unwrap();
    let mut a_values = form_values(&form);
    let mut b_values = a_values.clone();
    a_values.insert(format!("{ns}:race"), "from_a".into());
    b_values.insert(format!("{ns}:race"), "from_b".into());
    let base_events = changed_event_count(&pool, &ns).await;

    let barrier = Arc::new(tokio::sync::Barrier::new(3));
    let a = {
        let svc = svc.clone();
        let barrier = barrier.clone();
        tokio::spawn(async move {
            barrier.wait().await;
            apply_edit(&svc, a_values).await
        })
    };
    let b = {
        let svc = svc.clone();
        let barrier = barrier.clone();
        tokio::spawn(async move {
            barrier.wait().await;
            apply_edit(&svc, b_values).await
        })
    };
    barrier.wait().await;
    let (a_result, b_result) = tokio::time::timeout(Duration::from_secs(10), async {
        (a.await.unwrap(), b.await.unwrap())
    })
    .await
    .expect("concurrent config submits must not deadlock");

    let mut successes = 0;
    let mut conflicts = 0;
    for result in [a_result, b_result] {
        match result {
            Ok(()) => successes += 1,
            Err(adminapi::SubmitError::Conflict) => conflicts += 1,
            Err(adminapi::SubmitError::Other(err)) => panic!("unexpected submit error: {err}"),
        }
    }
    assert_eq!((successes, conflicts), (1, 1));
    assert!(matches!(
        stored_value(&pool, &ns, "race").await.as_deref(),
        Some("from_a" | "from_b")
    ));
    assert_eq!(
        changed_event_count(&pool, &ns).await,
        base_events + 1,
        "only the winning submit may write and emit"
    );

    cleanup(&pool, &ns).await;
}

/// Atomicity of a MIXED admin submit: one call carrying a valid change to an existing
/// setting AND an invalid `_new_*` namespace. Phase-1 validation fails before any write,
/// so the store is untouched — the valid change does NOT sneak through and NO durable
/// `config.changed` event is appended.
#[tokio::test]
async fn admin_apply_edit_mixed_is_atomic() {
    let _guard = CONFIG_DB_TEST_GUARD.lock().await;
    let Some(pool) = test_pool().await else { return };
    let ns = unique_ns(&pool).await;
    let svc = Arc::new(Service::new(pool.clone()));

    // Seed one existing setting and load it into the cache (apply_edit diffs the cache).
    svc.set(&ns, "existing", "old").await.unwrap();
    svc.refresh().await.unwrap();
    let base_events = changed_event_count(&pool, &ns).await; // 1 — the seed insert.

    // One submit: a VALID change to `existing` + an INVALID new-row namespace (not
    // ^[a-z0-9_]+$). Phase-1 validation rejects the whole form before any write.
    let form = admin_render(&svc, &adminapi::Params::new())
        .unwrap()
        .form
        .unwrap();
    let mut values = form_values(&form);
    values.insert(format!("{ns}:existing"), "new".into());
    values.insert("_new_namespace".into(), "Bad NS".into());
    values.insert("_new_key".into(), "k".into());
    values.insert("_new_value".into(), "v".into());
    let err = apply_edit(&svc, values).await.unwrap_err();
    assert!(err.to_string().contains("invalid namespace"), "got: {err}");

    // Nothing committed: `existing` is unchanged and no new event was appended.
    let (_rev, settings) = svc.load_snapshot().await.unwrap();
    let existing = settings
        .iter()
        .find(|s| s.namespace == ns && s.key == "existing")
        .expect("existing setting present");
    assert_eq!(existing.value, "old", "the valid change must not have committed");
    assert_eq!(
        changed_event_count(&pool, &ns).await,
        base_events,
        "a rejected form must append no events"
    );

    cleanup(&pool, &ns).await;
}

/// A successful batch of two changed settings in ONE submit lands both values and, since
/// the settings trigger is FOR EACH ROW, emits exactly two `config.changed` events —
/// committed atomically at the transaction boundary.
#[tokio::test]
async fn admin_apply_edit_batch_emits_one_event_per_change() {
    let _guard = CONFIG_DB_TEST_GUARD.lock().await;
    let Some(pool) = test_pool().await else { return };
    let ns = unique_ns(&pool).await;
    let svc = Arc::new(Service::new(pool.clone()));

    // Two existing settings loaded into the cache.
    svc.set(&ns, "a", "1").await.unwrap();
    svc.set(&ns, "b", "1").await.unwrap();
    svc.refresh().await.unwrap();
    let base_events = changed_event_count(&pool, &ns).await; // 2 — the seed inserts.

    // One submit changing BOTH values.
    let form = admin_render(&svc, &adminapi::Params::new())
        .unwrap()
        .form
        .unwrap();
    let mut values = form_values(&form);
    values.insert(format!("{ns}:a"), "2".into());
    values.insert(format!("{ns}:b"), "2".into());
    apply_edit(&svc, values).await.unwrap();

    // Both landed, and the batch emitted exactly two more events.
    let (_rev, settings) = svc.load_snapshot().await.unwrap();
    let val = |k: &str| {
        settings
            .iter()
            .find(|s| s.namespace == ns && s.key == k)
            .unwrap()
            .value
            .clone()
    };
    assert_eq!(val("a"), "2");
    assert_eq!(val("b"), "2");
    assert_eq!(
        changed_event_count(&pool, &ns).await - base_events,
        2,
        "a two-setting batch must emit exactly two config.changed events"
    );

    cleanup(&pool, &ns).await;
}

/// Step 8 carry-over: `Module::migrate` seeds `config.changed`'s row in
/// `asyncevents.history_contracts`. The write trigger emits via the plane-owned
/// `asyncevents.append_event` SQL function directly, bypassing both Rust seed paths
/// (native writer, typed-subscription reconcile) every OTHER topic gets seeded
/// through — without this, retention's conservative "no row = never GC" rule would
/// keep `config.changed` history forever. `test_pool` applies `SCHEMA_DDL` directly
/// (no module involved), so this drives the real `Module::migrate` to also exercise
/// `seed_history_contract`.
#[tokio::test]
async fn migrate_seeds_config_changed_history_contract() {
    let _guard = CONFIG_DB_TEST_GUARD.lock().await;
    let Some(pool) = test_pool().await else { return };
    let ctx = Context::with_db(pool.clone());
    let m = Config::new();
    m.register(&ctx).unwrap();
    m.migrate(&ctx).await.unwrap();

    let row: Option<(String, i32)> = sqlx::query_as(
        "SELECT policy, min_retention_days FROM asyncevents.history_contracts \
         WHERE topic = 'config.changed' AND contract_version = 1",
    )
    .fetch_optional(&pool)
    .await
    .unwrap();
    let (policy, days) =
        row.expect("config.changed history_contracts row must exist after Module::migrate");
    assert_eq!(policy, "min_retention");
    assert_eq!(days, 7, "must match configevents::CHANGED's declared HistoryPolicy::MinRetention{{days:7}}");

    // Idempotent: migrating again must not error (ON CONFLICT DO NOTHING + a matching
    // read-back never drifts against its own contract).
    m.migrate(&ctx).await.unwrap();
}
