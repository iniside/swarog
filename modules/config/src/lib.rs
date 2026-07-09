//! `config` — a central, DB-backed configuration store with live reload (port of
//! Go's `modules/config`). Namespaced `key=value` settings live in schema `config`;
//! any module reads them via the provided [`configapi::Config`] capability
//! (`get_string`/`get_bool`/`get_int`/`get` with a code-default fallback), and edits
//! made in `/admin` (or raw `psql`) propagate to every reader through Postgres
//! LISTEN/NOTIFY → in-memory cache refresh → `config.changed`. Secrets stay in env;
//! only non-secret operational knobs go here.
//!
//! An impl crate: NO other module imports it. Consumers depend on `configapi` (the
//! reader trait, resolved via `registry::key("config", "reader")`) and
//! `configevents` (the `config.changed` event) — never on this crate.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::Duration;

use bus::{AnyTx, Bus};
use lifecycle::{Caps, Context, Module};
use sqlx::postgres::PgListener;
use sqlx::PgPool;
use tokio::sync::watch;
use tokio::task::JoinHandle;

/// Fallback DSN for the listener's dedicated connection — same default as the
/// shared pool. config can't store the DSN it needs to reach its own store, so this
/// bootstrap tier stays in env.
const DEFAULT_DSN: &str =
    "postgres://gamebackend:gamebackend@localhost:5432/gamebackend?sslmode=disable";

/// The LISTEN/NOTIFY channel the `config.settings` write trigger fires on.
const NOTIFY_CHANNEL: &str = "config_changed";

/// Creates this module's OWN schema and nothing else — full logical isolation (#10).
/// Idempotent (`IF NOT EXISTS` / `OR REPLACE`). Verbatim from Go's `schemaDDL`.
///
/// The `AFTER INSERT OR UPDATE` trigger is the single source of the `config_changed`
/// NOTIFY: ANY writer — this service's `set`, another service's, or a raw `psql`
/// UPDATE — fires it, so every LISTENing process reloads. The payload
/// `"namespace:key"` matches the listener's split-on-first-`:` because ids are
/// `^[a-z0-9_]+$`. DELETE is deliberately not triggered (the listener has no delete
/// path); `RETURN NULL` is correct for an AFTER trigger.
const SCHEMA_DDL: &str = r#"
CREATE SCHEMA IF NOT EXISTS config;
CREATE TABLE IF NOT EXISTS config.settings (
	namespace  text NOT NULL,
	key        text NOT NULL,
	value      text NOT NULL,
	updated_at timestamptz NOT NULL DEFAULT now(),
	PRIMARY KEY (namespace, key)
);
CREATE OR REPLACE FUNCTION config.notify_changed() RETURNS trigger
	LANGUAGE plpgsql AS $$
BEGIN
	PERFORM pg_notify('config_changed', NEW.namespace || ':' || NEW.key);
	RETURN NULL;
END;
$$;
CREATE OR REPLACE TRIGGER settings_notify
	AFTER INSERT OR UPDATE ON config.settings
	FOR EACH ROW EXECUTE FUNCTION config.notify_changed();"#;

/// One persisted config row (`updated_at` is intentionally not carried — the getters
/// and admin render only need the value).
#[derive(Clone, Debug, PartialEq, Eq)]
struct Setting {
    namespace: String,
    key: String,
    value: String,
}

/// The composite `(namespace, key)` map key backing the read cache.
type CacheKey = (String, String);

/// Validates a namespace/key: non-empty and `[a-z0-9_]` only. Restricting ids this
/// way makes the `:` separator unambiguous everywhere it appears — the `pg_notify`
/// payload (`namespace:key`) and the admin form field name. (Go's `identRe`.)
fn valid_ident(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

// ============================================================================
// Service — the "config" capability: a read-mostly cache + a transactional Set.
// ============================================================================

/// The config service: a read-mostly in-memory cache of settings (kept fresh by the
/// listener) plus a validating [`Service::set`]. Readers get it as
/// `Arc<dyn configapi::Config>`; the concrete `set` is config-private (its own admin
/// uses it), NOT on the reader trait.
pub struct Service {
    pool: PgPool,
    cache: RwLock<HashMap<CacheKey, String>>,
}

impl Service {
    fn new(pool: PgPool) -> Service {
        Service {
            pool,
            cache: RwLock::new(HashMap::new()),
        }
    }

    /// Validates the ids, then upserts the row in a single autocommit statement. The
    /// `config.settings` AFTER-write trigger fires `pg_notify('config_changed',
    /// "ns:key")` on the same statement (delivered on commit), so no explicit NOTIFY
    /// is needed. `set` does NOT touch the cache: the listener is the single refresh
    /// path, so a local write and an external `psql` edit are handled identically.
    pub async fn set(&self, ns: &str, key: &str, value: &str) -> anyhow::Result<()> {
        if !valid_ident(ns) {
            anyhow::bail!("config: invalid namespace {ns:?} (must match ^[a-z0-9_]+$)");
        }
        if !valid_ident(key) {
            anyhow::bail!("config: invalid key {key:?} (must match ^[a-z0-9_]+$)");
        }
        sqlx::query(
            "INSERT INTO config.settings (namespace, key, value) VALUES ($1, $2, $3) \
             ON CONFLICT (namespace, key) DO UPDATE SET value = excluded.value, updated_at = now()",
        )
        .bind(ns)
        .bind(key)
        .bind(value)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Reads every setting — the full-reload source used on each listener (re)connect.
    /// Ordering is irrelevant: the cache is a map.
    async fn load_all(&self) -> anyhow::Result<Vec<Setting>> {
        let rows: Vec<(String, String, String)> =
            sqlx::query_as("SELECT namespace, key, value FROM config.settings")
                .fetch_all(&self.pool)
                .await?;
        Ok(rows
            .into_iter()
            .map(|(namespace, key, value)| Setting {
                namespace,
                key,
                value,
            })
            .collect())
    }

    /// Fetches a single setting's value. `Ok(None)` means the row is absent (a
    /// deleted key); a real DB error is returned as `Err`.
    async fn get_one(&self, ns: &str, key: &str) -> anyhow::Result<Option<String>> {
        let row: Option<(String,)> =
            sqlx::query_as("SELECT value FROM config.settings WHERE namespace = $1 AND key = $2")
                .bind(ns)
                .bind(key)
                .fetch_optional(&self.pool)
                .await?;
        Ok(row.map(|(v,)| v))
    }

    /// Swaps in a fresh cache from a full load (listener (re)connect) and returns the
    /// settings whose value changed vs the prior snapshot — a new key or a changed
    /// value counts as changed; removed keys are ignored (deletes out of scope). The
    /// diff is computed under the write lock while swapping, so it reflects exactly
    /// the snapshot installed. The listener replays these as `config.changed` after a
    /// RECONNECT so materialized push consumers heal.
    fn replace_cache(&self, settings: Vec<Setting>) -> Vec<Setting> {
        let mut new_map: HashMap<CacheKey, String> = HashMap::with_capacity(settings.len());
        for st in &settings {
            new_map.insert((st.namespace.clone(), st.key.clone()), st.value.clone());
        }
        let mut guard = self.cache.write().unwrap();
        let mut changed = Vec::new();
        for st in &settings {
            match guard.get(&(st.namespace.clone(), st.key.clone())) {
                Some(prev) if prev == &st.value => {}
                _ => changed.push(st.clone()),
            }
        }
        *guard = new_map;
        changed
    }

    /// Updates a single cached key (listener applying one notification).
    fn set_cache_one(&self, ns: &str, key: &str, value: &str) {
        self.cache
            .write()
            .unwrap()
            .insert((ns.to_string(), key.to_string()), value.to_string());
    }

    /// Snapshots the cache as a slice sorted by `(namespace, key)` for a stable admin
    /// render.
    fn all(&self) -> Vec<Setting> {
        let mut out: Vec<Setting> = {
            let guard = self.cache.read().unwrap();
            guard
                .iter()
                .map(|((ns, key), value)| Setting {
                    namespace: ns.clone(),
                    key: key.clone(),
                    value: value.clone(),
                })
                .collect()
        };
        out.sort_by(|a, b| a.namespace.cmp(&b.namespace).then(a.key.cmp(&b.key)));
        out
    }
}

/// The read-side capability config exposes. All getters degrade to `default` on a
/// cache miss. `set` is deliberately absent (config-private).
impl configapi::Config for Service {
    fn get_string(&self, ns: &str, key: &str, default: &str) -> String {
        self.get(ns, key).unwrap_or_else(|| default.to_string())
    }

    fn get_bool(&self, ns: &str, key: &str, default: bool) -> bool {
        match self.get(ns, key) {
            Some(v) => v == "1" || v.eq_ignore_ascii_case("true") || v.eq_ignore_ascii_case("on"),
            None => default,
        }
    }

    fn get_int(&self, ns: &str, key: &str, default: i64) -> i64 {
        match self.get(ns, key) {
            Some(v) => v.parse::<i64>().unwrap_or(default),
            None => default,
        }
    }

    fn get(&self, ns: &str, key: &str) -> Option<String> {
        self.cache
            .read()
            .unwrap()
            .get(&(ns.to_string(), key.to_string()))
            .cloned()
    }
}

/// The wire-only remoting capability (Step 5): a peer's `CachedConfig` calls this over
/// the internal edge to (re)build its cache. It reads the authoritative STORE (not the
/// in-memory cache) so a snapshot answered before the listener's first connect is
/// still correct.
#[async_trait::async_trait]
impl configapi::ConfigSnapshot for Service {
    async fn snapshot(&self) -> Result<Vec<configapi::Setting>, opsapi::Error> {
        let settings = self
            .load_all()
            .await
            .map_err(|e| opsapi::Error::internal(e.to_string()))?;
        Ok(settings
            .into_iter()
            .map(|s| configapi::Setting {
                namespace: s.namespace,
                key: s.key,
                value: s.value,
            })
            .collect())
    }
}

// ============================================================================
// Live reload — a dedicated PgListener, with boot-vs-reconnect replay.
// ============================================================================

/// Publishes one `config.changed` on the DURABLE plane (Step 5). The listener owns no
/// domain write, so it opens its OWN short transaction purely to carry the outbox
/// insert (`emit_tx` needs a tx), then commits. Mirrors Go, where ONLY the listener
/// emitted `config.changed` — the admin `set` path stays a plain upsert whose trigger
/// NOTIFY drives THIS emit, so a change is published exactly once regardless of who
/// wrote it (no producer-side double-emit to dedup).
async fn emit_changed(svc: &Service, bus: &Bus, ns: &str, key: &str, value: &str) -> anyhow::Result<()> {
    let mut tx = svc.pool.begin().await?;
    bus.emit_tx(
        AnyTx::new(&mut *tx), // erased after the deref: Transaction<'_> isn't 'static
        &configevents::CHANGED,
        &configevents::Changed {
            namespace: ns.to_string(),
            key: key.to_string(),
            value: value.to_string(),
        },
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Does the FULL cache reload every (re)connect performs and, on a RECONNECT (not
/// the initial boot load), emits `config.changed` for each key whose value differs
/// from the prior snapshot. PG does not queue NOTIFY for a dead session, so a
/// reconnect that only reloaded the pull cache would leave a materialized push
/// consumer (e.g. inventory's starter spec) stale for any key changed during the
/// disconnect — so we replay those changes as events. The boot load emits NOTHING
/// (that would spam one event per key at startup; pull consumers lazy-load anyway).
/// This is the single path the listener uses; the `reconnect` flag is the only
/// difference, so a test can drive healing by calling it with `reconnect = true`.
async fn reload_and_heal(svc: &Service, bus: &Bus, reconnect: bool) -> anyhow::Result<()> {
    let settings = svc.load_all().await?;
    let changed = svc.replace_cache(settings);
    if reconnect {
        for st in changed {
            // DURABLE replay so a materialized push consumer (e.g. inventory's starter
            // spec) heals across a listener reconnect even in a split topology.
            emit_changed(svc, bus, &st.namespace, &st.key, &st.value).await?;
        }
    }
    Ok(())
}

/// Keeps a dedicated `PgListener` connection LISTENing for `config_changed` and
/// refreshes the cache until `stop` flips. It never dies on a DB outage: each
/// (re)connect goes through [`listen_once`], which backs off on failure. `booted`
/// tracks whether the first successful reload (the boot load) has happened; every
/// subsequent reload is a reconnect and heals.
async fn listen(dsn: String, svc: Arc<Service>, bus: Arc<Bus>, mut stop: watch::Receiver<bool>) {
    let mut booted = false;
    while !*stop.borrow() {
        if listen_once(&dsn, &svc, &bus, booted, &mut stop).await {
            booted = true; // the first successful reload is the boot load; the rest reconnect
        }
    }
}

/// Owns exactly one dedicated connection for its lifetime: connects, LISTENs, does a
/// FULL cache reload (`reload_and_heal` with `booted` as the reconnect flag), then
/// blocks on notifications until an error or cancellation. Returns `true` iff it got
/// as far as installing a fresh cache (a successful reload), so the caller knows the
/// boot load is done and subsequent reloads should heal. The dedicated `PgListener`
/// connection is separate from `svc.pool` (which serves the reload/get_one reads),
/// matching Go's raw-pgx listener conn vs the shared `*sql.DB`.
async fn listen_once(
    dsn: &str,
    svc: &Arc<Service>,
    bus: &Arc<Bus>,
    booted: bool,
    stop: &mut watch::Receiver<bool>,
) -> bool {
    let mut listener = match PgListener::connect(dsn).await {
        Ok(l) => l,
        Err(err) => {
            tracing::error!(%err, "config listener connect failed");
            backoff(stop).await;
            return false;
        }
    };
    if let Err(err) = listener.listen(NOTIFY_CHANNEL).await {
        tracing::error!(%err, "config listener LISTEN failed");
        backoff(stop).await;
        return false;
    }
    if let Err(err) = reload_and_heal(svc, bus, booted).await {
        tracing::error!(%err, "config listener reload failed");
        backoff(stop).await;
        return false;
    }

    loop {
        tokio::select! {
            _ = stop.changed() => return true, // clean shutdown (reload already succeeded)
            res = listener.recv() => match res {
                Ok(notif) => {
                    let payload = notif.payload();
                    let Some((ns, key)) = payload.split_once(':') else {
                        tracing::warn!(%payload, "config listener ignoring malformed payload");
                        continue;
                    };
                    match svc.get_one(ns, key).await {
                        Ok(Some(v)) => {
                            svc.set_cache_one(ns, key, &v);
                            // DURABLE publish (Step 5): rides the outbox so a
                            // cross-process consumer (inventory in a split) sees it.
                            if let Err(err) = emit_changed(svc, bus, ns, key, &v).await {
                                tracing::error!(%ns, %key, %err, "config listener emit_tx failed");
                            }
                        }
                        Ok(None) => continue, // a delete (only upserts exist today) — nothing to cache
                        Err(err) => {
                            tracing::error!(%ns, %key, %err, "config listener getOne failed");
                            continue;
                        }
                    }
                }
                Err(err) => {
                    tracing::error!(%err, "config listener wait failed");
                    backoff(stop).await;
                    return true; // reconnect via the outer loop (conn dropped on return)
                }
            }
        }
    }
}

/// Waits ~1s, returning early if `stop` flips so shutdown stays prompt and a
/// reconnect storm never tight-spins.
async fn backoff(stop: &mut watch::Receiver<bool>) {
    tokio::select! {
        _ = stop.changed() => {}
        _ = tokio::time::sleep(Duration::from_secs(1)) => {}
    }
}

// ============================================================================
// Admin — the config editor page (contract only; no portal renders it in M1).
// ============================================================================

/// The config editor page: KPIs, a read-only table of the current settings, and an
/// editable [`adminapi::Form`] (one field per setting + an add-new triple). The
/// admin portal owns the POST route/auth/rendering; config only supplies the
/// declarative widgets and the `apply_edit` submit closure.
fn admin_render(svc: &Arc<Service>, _params: &adminapi::Params) -> anyhow::Result<adminapi::Content> {
    let rows = svc.all();

    let mut namespaces: HashSet<&str> = HashSet::new();
    let mut table = adminapi::Table {
        columns: vec!["Namespace".into(), "Key".into(), "Value".into()],
        rows: Vec::with_capacity(rows.len()),
    };
    let mut fields: Vec<adminapi::Field> = Vec::with_capacity(rows.len() + 3);
    for r in &rows {
        namespaces.insert(r.namespace.as_str());
        table.rows.push(vec![
            adminapi::Cell::mono(&r.namespace),
            adminapi::Cell::mono(&r.key),
            adminapi::Cell::text(&r.value),
        ]);
        fields.push(adminapi::Field {
            name: format!("{}:{}", r.namespace, r.key),
            label: format!("{} / {}", r.namespace, r.key),
            value: r.value.clone(),
        });
    }
    // Add-new triple: config owns the "" -> insert semantics; the adminapi::Form
    // contract stays a generic name/value list.
    fields.push(adminapi::Field {
        name: "_new_namespace".into(),
        label: "New namespace".into(),
        value: String::new(),
    });
    fields.push(adminapi::Field {
        name: "_new_key".into(),
        label: "New key".into(),
        value: String::new(),
    });
    fields.push(adminapi::Field {
        name: "_new_value".into(),
        label: "New value".into(),
        value: String::new(),
    });

    let submit_svc = svc.clone();
    let form = adminapi::Form {
        action: String::new(),
        fields,
        submit: Some(Arc::new(move |values: adminapi::Params| {
            let svc = submit_svc.clone();
            Box::pin(async move { apply_edit(&svc, values).await })
        })),
    };

    Ok(adminapi::Content {
        kpis: vec![
            adminapi::Kpi {
                label: "Settings".into(),
                value: rows.len().to_string(),
                sub: String::new(),
            },
            adminapi::Kpi {
                label: "Namespaces".into(),
                value: namespaces.len().to_string(),
                sub: String::new(),
            },
        ],
        table: Some(table),
        form: Some(form),
    })
}

/// The read-only settings content (KPIs + table, no editable form) — what the REMOTE
/// admin fan-out returns, since a remote form cannot marshal its `submit` closure.
fn admin_content_ro(svc: &Service) -> adminapi::Content {
    let rows = svc.all();
    let mut namespaces: HashSet<&str> = HashSet::new();
    let mut table = adminapi::Table {
        columns: vec!["Namespace".into(), "Key".into(), "Value".into()],
        rows: Vec::with_capacity(rows.len()),
    };
    for r in &rows {
        namespaces.insert(r.namespace.as_str());
        table.rows.push(vec![
            adminapi::Cell::mono(&r.namespace),
            adminapi::Cell::mono(&r.key),
            adminapi::Cell::text(&r.value),
        ]);
    }
    adminapi::Content {
        kpis: vec![
            adminapi::Kpi { label: "Settings".into(), value: rows.len().to_string(), sub: String::new() },
            adminapi::Kpi { label: "Namespaces".into(), value: namespaces.len().to_string(), sub: String::new() },
        ],
        table: Some(table),
        form: None,
    }
}

#[async_trait::async_trait]
impl adminapi::AdminData for Service {
    /// The admin fan-out (`admin.adminData` on the edge): the config page as
    /// `adminapi::ItemData`. Read-only over the wire (the editable form is LOCAL-only),
    /// same Section/Label the local `Item` carries.
    async fn admin_data(&self) -> Result<adminapi::ItemData, opsapi::Error> {
        Ok(adminapi::ItemData {
            id: "config".into(),
            section: "Platform".into(),
            label: "Game Config & Flags".into(),
            content: admin_content_ro(self),
        })
    }
}

/// Diffs the posted values against the current cache and `set`s ONLY the keys that
/// actually changed (each `set` is a NOTIFY + a `config.changed`; rewriting every row
/// would emit a storm of false "changed" events). It then inserts the add-new row if
/// its triple is fully filled. Returns the first error.
async fn apply_edit(svc: &Arc<Service>, values: adminapi::Params) -> anyhow::Result<()> {
    let mut first_err: Option<anyhow::Error> = None;

    for s in svc.all() {
        if let Some(v) = values.get(&format!("{}:{}", s.namespace, s.key)) {
            if *v != s.value {
                if let Err(e) = svc.set(&s.namespace, &s.key, v).await {
                    first_err.get_or_insert(e);
                }
            }
        }
    }

    let ns = adminapi::param(&values, "_new_namespace");
    let key = adminapi::param(&values, "_new_key");
    let val = adminapi::param(&values, "_new_value");
    if !ns.is_empty() && !key.is_empty() && !val.is_empty() {
        if let Err(e) = svc.set(ns, key, val).await {
            // set validates the ids
            first_err.get_or_insert(e);
        }
    }

    match first_err {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

// ============================================================================
// Module — the lifecycle wiring.
// ============================================================================

/// The config module. Holds the constructed service (shared between `register`, the
/// listener, and the admin render), the bus/dsn captured in `init`, and the
/// listener's cancel/join handles.
pub struct Config {
    svc: OnceLock<Arc<Service>>,
    bus: OnceLock<Arc<Bus>>,
    dsn: Mutex<Option<String>>,
    stop_tx: Mutex<Option<watch::Sender<bool>>>,
    tasks: Mutex<Vec<JoinHandle<()>>>,
}

impl Default for Config {
    fn default() -> Self {
        Config::new()
    }
}

impl Config {
    pub fn new() -> Config {
        Config {
            svc: OnceLock::new(),
            bus: OnceLock::new(),
            dsn: Mutex::new(None),
            stop_tx: Mutex::new(None),
            tasks: Mutex::new(Vec::new()),
        }
    }

    fn svc(&self) -> Arc<Service> {
        self.svc
            .get()
            .expect("config.register must run before init/start")
            .clone()
    }
}

#[async_trait::async_trait]
impl Module for Config {
    fn name(&self) -> &str {
        "config"
    }

    fn caps(&self) -> Caps {
        Caps::REGISTER | Caps::MIGRATE | Caps::START | Caps::STOP
    }

    /// Phase 1, BEFORE any `init`: builds the service and offers it under the
    /// capability key `"config.reader"`, so a dependent's `require`/`try_require`
    /// resolves regardless of registration order. The cache is filled by the
    /// listener's first connect (`start`), not here.
    fn register(&self, ctx: &Context) -> anyhow::Result<()> {
        let pool = ctx
            .db()
            .ok_or_else(|| anyhow::anyhow!("config requires a DB pool"))?
            .clone();
        let svc = Arc::new(Service::new(pool));
        self.svc
            .set(svc.clone())
            .map_err(|_| anyhow::anyhow!("config.register ran twice"))?;
        ctx.registry().provide::<dyn configapi::Config>(
            registry::key("config", "reader"),
            svc as Arc<dyn configapi::Config>,
        );
        Ok(())
    }

    /// Creates this module's own schema. Idempotent.
    async fn migrate(&self, ctx: &Context) -> anyhow::Result<()> {
        let pool = ctx
            .db()
            .ok_or_else(|| anyhow::anyhow!("config requires a DB pool"))?;
        sqlx::raw_sql(SCHEMA_DDL).execute(pool).await?;
        Ok(())
    }

    /// Only wires up — no DB I/O (#8). Captures the bus + listener DSN and contributes
    /// the admin editor page. The cache is filled by the listener's first connect.
    fn init(&self, ctx: &Context) -> anyhow::Result<()> {
        self.bus
            .set(ctx.bus().clone())
            .map_err(|_| anyhow::anyhow!("config.init ran twice"))?;
        *self.dsn.lock().unwrap() = Some(env_or("DATABASE_URL", DEFAULT_DSN));

        let svc = self.svc();
        ctx.contribute(
            adminapi::SLOT,
            adminapi::Item::local(
                "config",
                "Platform",
                "Game Config & Flags",
                Arc::new(move |params: &adminapi::Params| admin_render(&svc, params)),
            ),
        );

        // Edge exposure, contributed UNCONDITIONALLY — topology-blind (Step 3 seam):
        // config's wire-only `ConfigSnapshot` face rides `edge::EDGE_SLOT`; `app::run`
        // installs it iff this process serves an internal edge (config-svc does; the
        // monolith never applies it). Own glue (rule 5): `configrpc` register_server.
        let snap = self.svc();
        ctx.contribute(
            edge::EDGE_SLOT,
            edge::EdgeReg::new(move |server| {
                // The admin fan-out face (`admin.adminData`), via this module's OWN
                // glue crate's re-export (no foreign rpc import).
                configrpc::register_admin(server, snap.clone());
                configrpc::config_snapshot_rpc::register_server(server, snap);
            }),
        );
        Ok(())
    }

    /// Launches the LISTEN/NOTIFY loop on a FRESH `Background`-rooted task (via
    /// `tokio::spawn`, not tied to the `start` ctx), so a short start deadline can't
    /// kill the loop; `stop` cancels it. The initial full cache load happens inside
    /// the loop's first connect, so boot and reconnect share one cache-population
    /// path.
    async fn start(&self, _ctx: &Context) -> anyhow::Result<()> {
        let svc = self.svc();
        let bus = self
            .bus
            .get()
            .expect("config.init must run before start")
            .clone();
        let dsn = self
            .dsn
            .lock()
            .unwrap()
            .clone()
            .expect("config.init must run before start");

        let (stop_tx, stop_rx) = watch::channel(false);
        let task = tokio::spawn(listen(dsn, svc, bus, stop_rx));
        *self.stop_tx.lock().unwrap() = Some(stop_tx);
        self.tasks.lock().unwrap().push(task);
        Ok(())
    }

    /// Cancels the loop and awaits its exit. It does NOT close the listener conn — the
    /// loop owns and re-creates it across reconnects, so a permanent outage degrades
    /// to stale-cache + retry.
    async fn stop(&self, _ctx: &Context) -> anyhow::Result<()> {
        if let Some(tx) = self.stop_tx.lock().unwrap().take() {
            let _ = tx.send(true);
        }
        let tasks = std::mem::take(&mut *self.tasks.lock().unwrap());
        for t in tasks {
            let _ = t.await;
        }
        Ok(())
    }
}

fn env_or(key: &str, def: &str) -> String {
    match std::env::var(key) {
        Ok(v) if !v.is_empty() => v,
        _ => def.to_string(),
    }
}

// ============================================================================
// Tests. Unit tests need no DB; integration tests target the local Postgres (the
// test DB) and SKIP cleanly (early return + message) when it is unreachable. In-crate
// so they can drive the private `Service`/`reload_and_heal`/`listen` directly.
// ============================================================================
#[cfg(test)]
mod tests;
