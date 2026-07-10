//! `config` — a central, DB-backed configuration store with live reload (port of
//! Go's `modules/config`). Namespaced `key=value` settings live in schema `config`;
//! any module reads them via the provided [`configapi::Config`] capability
//! (`get_string`/`get_bool`/`get_int`/`get` with a code-default fallback), and edits
//! made in `/admin` (or raw `psql`) propagate to every reader through the
//! `config.settings` write trigger. Secrets stay in env; only non-secret operational
//! knobs go here.
//!
//! ## Step 7 — one trigger, two planes, monotonic revision
//! Every INSERT/UPDATE/DELETE on `config.settings` fires ONE trigger that, in the
//! writing transaction and in this order: (a) bumps the monotonic `config.revision`
//! singleton, (b) `pg_notify`s the `config_changed` channel, (c) appends a durable
//! `config.changed` event via the plane-owned `asyncevents.append_event` (config DDL
//! NEVER touches the plane's tables — only that function). A raw `psql` write and a
//! service `set` therefore audit and invalidate identically — the trigger is the
//! single emission path, so there is no producer-side double-emit to dedup.
//!
//! Cache freshness rides the BROADCAST invalidation plane, not the durable event:
//! the local [`Service`] (and, in a split, the remote `configrpc::CachedConfig`)
//! registers an authoritative-refresh callback on the `config_changed` channel. The
//! callback re-reads the whole snapshot and swaps it atomically, applying only a
//! revision strictly newer than the one held — so a stale or duplicate NOTIFY is a
//! no-op. The durable `config.changed` event now exists purely for audit and any
//! future durable projection.
//!
//! An impl crate: NO other module imports it. Consumers depend on `configapi` (the
//! reader trait, resolved via `registry::key("config", "reader")`) and
//! `configevents` (the `config.changed` event) — never on this crate.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock, RwLock};

use lifecycle::{Caps, Context, Module};
use sqlx::PgPool;

/// Fallback DSN — same default as the shared pool. Test-only now that the module owns
/// no listener connection (the lazy-pool unit tests never issue a query).
#[cfg(test)]
const DEFAULT_DSN: &str =
    "postgres://gamebackend:gamebackend@localhost:5432/gamebackend?sslmode=disable";

/// The broadcast-invalidation channel the `config.settings` write trigger `pg_notify`s
/// and every config cache LISTENs on for its authoritative refresh.
const NOTIFY_CHANNEL: &str = "config_changed";

/// Creates this module's OWN schema and nothing else — full logical isolation (#10).
/// Idempotent (`IF NOT EXISTS` / `OR REPLACE`).
///
/// `config.revision` is a monotonic singleton (one row, `revision` bigint). The
/// `AFTER INSERT OR UPDATE OR DELETE` trigger is the single source of change
/// propagation: for ANY writer — this service's `set`, another service's, or a raw
/// `psql` write — it branches on `TG_OP` (a DELETE reads `OLD`, not `NEW`) and, in the
/// writing transaction, IN ORDER (a) locks + increments `config.revision`, (b)
/// `pg_notify`s `config_changed` with the operation/namespace/key/revision payload, and
/// (c) appends the durable `config.changed` event via the plane-owned
/// `asyncevents.append_event` — the ONLY `asyncevents` object config's DDL references
/// (it never touches the plane's tables). `RETURN NULL` is correct for an AFTER trigger.
/// `value` is `null` on a DELETE. `config.revision` seeds at 0; the first mutation
/// yields revision 1 (real revisions are ≥ 1, so a cache initialised to -1 always
/// applies its first load).
const SCHEMA_DDL: &str = r#"
CREATE SCHEMA IF NOT EXISTS config;
CREATE TABLE IF NOT EXISTS config.settings (
	namespace  text NOT NULL,
	key        text NOT NULL,
	value      text NOT NULL,
	updated_at timestamptz NOT NULL DEFAULT now(),
	PRIMARY KEY (namespace, key)
);
CREATE TABLE IF NOT EXISTS config.revision (
	singleton bool   PRIMARY KEY DEFAULT true CHECK (singleton),
	revision  bigint NOT NULL DEFAULT 0
);
INSERT INTO config.revision (singleton, revision) VALUES (true, 0)
	ON CONFLICT (singleton) DO NOTHING;
CREATE OR REPLACE FUNCTION config.notify_changed() RETURNS trigger
	LANGUAGE plpgsql AS $$
DECLARE
	_ns      text;
	_key     text;
	_value   text;
	_op      text;
	_rev     bigint;
	_payload jsonb;
BEGIN
	IF TG_OP = 'DELETE' THEN
		_ns := OLD.namespace; _key := OLD.key; _value := NULL; _op := 'delete';
	ELSIF TG_OP = 'UPDATE' THEN
		_ns := NEW.namespace; _key := NEW.key; _value := NEW.value; _op := 'update';
	ELSE
		_ns := NEW.namespace; _key := NEW.key; _value := NEW.value; _op := 'insert';
	END IF;

	-- (a) lock + increment the monotonic revision (serialises concurrent writers).
	UPDATE config.revision SET revision = revision + 1 WHERE singleton
		RETURNING revision INTO _rev;

	_payload := jsonb_build_object(
		'namespace', _ns,
		'key',       _key,
		'value',     _value,   -- to_jsonb(NULL) => JSON null (a DELETE carries no value)
		'operation', _op,
		'revision',  _rev
	);

	-- (b) broadcast-invalidation NOTIFY (every config cache refreshes on this).
	PERFORM pg_notify('config_changed', _payload::text);

	-- (c) durable audit event via the plane-owned writer (the ONLY asyncevents object
	-- config touches — never the plane's tables directly).
	PERFORM asyncevents.append_event('config.changed', 1, _payload);

	RETURN NULL;
END;
$$;
CREATE OR REPLACE TRIGGER settings_notify
	AFTER INSERT OR UPDATE OR DELETE ON config.settings
	FOR EACH ROW EXECUTE FUNCTION config.notify_changed();"#;

/// Seeds `config.changed`'s row in `asyncevents.history_contracts` — the retention
/// GC's per-`(topic, version)` policy source — by calling the plane-owned
/// `asyncevents.ensure_history_contract` function (never the plane's tables directly).
/// `config.changed` is emitted by the SQL trigger calling the plane-owned
/// `asyncevents.append_event` directly, bypassing both the native writer and the
/// typed-subscription reconcile paths that seed the row for every OTHER topic, so
/// nothing else ever seeds this row; without it retention's conservative "no row =
/// never GC" rule would keep this topic forever.
///
/// A plane-function call, NOT raw table access: archcheck (CLAUDE.md constraint 1)
/// forbids a module from taking a non-dev dependency on `asyncevents` — it is app-owned
/// infrastructure injected at `Context` construction, never a module capability — and
/// the plane owns its tables, exposing only this narrow function surface (the same
/// pattern the write trigger uses for `asyncevents.append_event`). The function's own
/// `ON CONFLICT ... DO NOTHING` + drift check RAISEs on a stored row with a DIFFERENT
/// policy (a topic's history promise is immutable), surfacing here as a query error.
async fn seed_history_contract(pool: &PgPool) -> anyhow::Result<()> {
    let contract = configevents::CHANGED.contract();
    let (policy, days): (&str, i32) = match contract.history {
        bus::HistoryPolicy::MinRetention { days } => {
            ("min_retention", i32::try_from(days).unwrap_or(i32::MAX))
        }
        bus::HistoryPolicy::KeepForever => ("keep_forever", 7),
    };
    let version = i32::try_from(contract.version)?;

    sqlx::query("SELECT asyncevents.ensure_history_contract($1, $2, $3, $4)")
        .bind(contract.topic)
        .bind(version)
        .bind(policy)
        .bind(days)
        .execute(pool)
        .await?;
    Ok(())
}

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

/// The atomically-swapped cache contents: the settings map plus the monotonic
/// `config.revision` they were read at. Held under ONE `RwLock` so a reader always sees
/// a coherent (revision, map) pair and the refresh's revision gate is race-free.
struct CacheState {
    /// The revision of the loaded snapshot; `-1` before the first load (real revisions
    /// are ≥ 0, and a mutation makes them ≥ 1, so the first refresh always applies).
    revision: i64,
    map: HashMap<CacheKey, String>,
}

impl CacheState {
    fn empty() -> CacheState {
        CacheState {
            revision: -1,
            map: HashMap::new(),
        }
    }
}

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
    cache: RwLock<CacheState>,
}

impl Service {
    fn new(pool: PgPool) -> Service {
        Service {
            pool,
            cache: RwLock::new(CacheState::empty()),
        }
    }

    /// Validates the ids, then upserts the row in a single autocommit statement. The
    /// `config.settings` AFTER-write trigger runs on commit — bumping the revision,
    /// `pg_notify`ing `config_changed`, and appending the durable event. `set` does NOT
    /// touch the cache: the invalidation callback is the single refresh path, so a
    /// service write and an external `psql` edit are handled identically.
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

    /// Reads the whole store — the monotonic `config.revision` AND every setting — in
    /// ONE statement (a LEFT JOIN of the singleton revision row against settings), so
    /// the revision names exactly the setting set returned (never two READ COMMITTED
    /// reads that could straddle a concurrent write). With no settings the LEFT JOIN
    /// still yields one row carrying the revision and NULL setting columns.
    async fn load_snapshot(&self) -> anyhow::Result<(i64, Vec<Setting>)> {
        load_snapshot_from(&self.pool).await
    }

    /// Re-reads the authoritative store and applies it to the cache if newer — the
    /// invalidation callback's whole body. Building the map before taking the lock keeps
    /// the write critical section to the revision check + two moves.
    async fn refresh(&self) -> anyhow::Result<()> {
        let (revision, settings) = self.load_snapshot().await?;
        self.apply(revision, settings);
        Ok(())
    }

    /// Atomic revision-gated swap: replaces the cache only when `revision` is strictly
    /// newer than the one held, so a stale or duplicate NOTIFY (or a redundant boot
    /// refresh) is a no-op.
    fn apply(&self, revision: i64, settings: Vec<Setting>) {
        let mut map: HashMap<CacheKey, String> = HashMap::with_capacity(settings.len());
        for st in settings {
            map.insert((st.namespace, st.key), st.value);
        }
        let mut guard = self.cache.write().unwrap();
        if revision <= guard.revision {
            return;
        }
        guard.revision = revision;
        guard.map = map;
    }

    /// Snapshots the cache as a slice sorted by `(namespace, key)` for a stable admin
    /// render.
    fn all(&self) -> Vec<Setting> {
        let mut out: Vec<Setting> = {
            let guard = self.cache.read().unwrap();
            guard
                .map
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

/// Reads `(revision, settings)` from ONE statement over `pool`. A free function so the
/// `ConfigSnapshot` RPC (which reads the STORE, not a `Service` cache) shares it.
async fn load_snapshot_from(pool: &PgPool) -> anyhow::Result<(i64, Vec<Setting>)> {
    // (revision, namespace?, key?, value?) — the setting columns are NULL on the single
    // no-settings LEFT JOIN row.
    type SnapshotRow = (i64, Option<String>, Option<String>, Option<String>);
    let rows: Vec<SnapshotRow> = sqlx::query_as(
        "SELECT r.revision, s.namespace, s.key, s.value \
         FROM config.revision r LEFT JOIN config.settings s ON true",
    )
    .fetch_all(pool)
    .await?;
    // `config.revision` is a seeded singleton, so there is always ≥ 1 row; the revision
    // is identical on every row.
    let revision = rows.first().map(|r| r.0).unwrap_or(0);
    let settings = rows
        .into_iter()
        .filter_map(|(_, ns, key, value)| match (ns, key, value) {
            (Some(namespace), Some(key), Some(value)) => Some(Setting {
                namespace,
                key,
                value,
            }),
            _ => None, // the no-settings LEFT JOIN row (NULL setting columns)
        })
        .collect();
    Ok((revision, settings))
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
            .map
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
    /// Reads the authoritative STORE (revision + every setting) in one statement, so a
    /// peer's `CachedConfig` gets a consistent snapshot with the revision that names it —
    /// never a `Service` cache read (which may be empty before the first refresh).
    async fn snapshot(&self) -> Result<configapi::Snapshot, opsapi::Error> {
        let (revision, settings) = self
            .load_snapshot()
            .await
            .map_err(|e| opsapi::Error::internal(e.to_string()))?;
        Ok(configapi::Snapshot {
            revision,
            settings: settings
                .into_iter()
                .map(|s| configapi::Setting {
                    namespace: s.namespace,
                    key: s.key,
                    value: s.value,
                })
                .collect(),
        })
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
/// invalidation callback, and the admin render).
pub struct Config {
    svc: OnceLock<Arc<Service>>,
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
        // No STOP: the cache is refreshed by the app-owned invalidation plane (which
        // owns the LISTEN connection and its shutdown), so config spawns no task of its
        // own. START does a single synchronous boot-load.
        Caps::REGISTER | Caps::MIGRATE | Caps::START
    }

    /// Phase 1, BEFORE any `init`: builds the service and offers it under the
    /// capability key `"config.reader"`, so a dependent's `require`/`try_require`
    /// resolves regardless of registration order. The cache is boot-loaded in `start`
    /// and kept fresh by the invalidation callback registered in `init`.
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

    /// Creates this module's own schema, THEN seeds `config.changed`'s row in
    /// `asyncevents.history_contracts` — the ONLY producer-side seed this topic gets:
    /// the write trigger emits via the plane-owned `asyncevents.append_event` SQL
    /// function directly (never the typed `enqueue_tx`/reconcile paths that seed the
    /// row for every OTHER topic), so without this call retention's conservative GC
    /// would never collect `config.changed` history. Runs after the plane's own
    /// migrate (structural in `app::run`, #8), so `asyncevents.history_contracts`
    /// already exists.
    async fn migrate(&self, ctx: &Context) -> anyhow::Result<()> {
        let pool = ctx
            .db()
            .ok_or_else(|| anyhow::anyhow!("config requires a DB pool"))?;
        sqlx::raw_sql(SCHEMA_DDL).execute(pool).await?;
        seed_history_contract(pool).await?;
        Ok(())
    }

    /// Only wires up — no DB I/O (#8). Contributes the admin editor page + the edge
    /// face, and registers the authoritative-refresh callback on the `config_changed`
    /// invalidation channel (REFRESH role only — the boot-fill stays in `start`).
    fn init(&self, ctx: &Context) -> anyhow::Result<()> {
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

        // Broadcast-invalidation REFRESH: every committed `config_changed` NOTIFY (from
        // the trigger — a service write OR a raw psql edit) re-runs this authoritative
        // refresh, which re-reads the snapshot and swaps the cache if the revision is
        // newer. Wiring-only (no I/O now): the closure first runs when the invalidation
        // plane starts, after module `start`. This is the ONLY refresh path once booted;
        // the `start` boot-load is the initial fill.
        let refresh = self.svc();
        ctx.invalidation().register(NOTIFY_CHANNEL, "config", move || {
            let refresh = refresh.clone();
            async move { refresh.refresh().await }
        });
        Ok(())
    }

    /// Boot-fills the cache once, synchronously, so a co-hosted reader sees populated
    /// config as soon as config has started (the invalidation plane's first refresh runs
    /// only after every module start). A load failure fails startup loudly — config's
    /// boot guarantee. Ongoing freshness is the invalidation callback's job.
    async fn start(&self, _ctx: &Context) -> anyhow::Result<()> {
        self.svc().refresh().await?;
        Ok(())
    }
}

// ============================================================================
// Tests. Unit tests need no DB; integration tests target the local Postgres (the
// test DB) and SKIP cleanly (early return + message) when it is unreachable. In-crate
// so they can drive the private `Service`/`load_snapshot`/`refresh` directly.
// ============================================================================
#[cfg(test)]
mod tests;
