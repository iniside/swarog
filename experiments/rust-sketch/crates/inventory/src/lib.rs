//! `inventory` — owns item holdings for any owner (a player, e.g. IAP, or a
//! character). It depends on accounts-supplied identity (the gateway injects it) and
//! on `characters` for ownership checks, and REACTS to character lifecycle events:
//! granting a starter item on creation and wiping holdings on deletion — integrity
//! WITHOUT a cross-module foreign key. `characters` has no idea inventory exists.
//! Port of Go's `modules/inventory`.
//!
//! The polymorphic [`Owner`] (player | character) lives entirely inside this module;
//! `owner_id` is referenced by id with NO cross-module FK (logical isolation #10).
//! The in-module FK from `holdings.item_id` to `items.id` DOES exist. The player
//! operations (`list_mine`/`list_character`/`grant`) are exposed as `opsapi`
//! Operations: the gateway fronts the HTTP routes, authenticates ONCE, and dispatches
//! to the service with the verified caller identity threaded in — never a
//! client-supplied one. An impl crate: no other module imports it.

use std::sync::{Arc, Mutex, OnceLock, RwLock};

use async_trait::async_trait;
use charactersapi::Ownership;
use configapi::Config;
use inventoryapi::{holdings_rpc, Holding, Holdings};
use lifecycle::{Caps, Context, Module};
use opsapi::{Error, Identity};
use registry::key;
use sqlx::{PgConnection, PgPool};

/// The per-key DEFAULT starter spec, used when `inventory/starter_item` /
/// `inventory/starter_qty` are absent from config. `config` is a mandatory
/// dependency (`requires`), so there is no "config isn't hosted" fallback case.
const STARTER_ITEM: &str = "starter_sword";
const STARTER_QTY: i64 = 1;

/// The admin surface ids (Section groups it in the sidebar; Label is the entry).
const ADMIN_ITEM_ID: &str = "inventory";
const ADMIN_SECTION: &str = "Game Content";
const ADMIN_LABEL: &str = "Inventory";
const ADMIN_OWNERS_LIMIT: i64 = 200;

/// Creates this module's OWN schema and nothing else — full logical isolation (#10).
/// Idempotent. Verbatim from Go's `schemaDDL`: `holdings.owner_id` is a plain `uuid`
/// ref to a player/character with NO cross-module FK; `holdings.item_id` DOES carry
/// the in-module FK to `items(id)`; `quantity` is CHECK'd non-negative.
const SCHEMA_DDL: &str = r#"
CREATE SCHEMA IF NOT EXISTS inventory;

CREATE TABLE IF NOT EXISTS inventory.items (
	id   text PRIMARY KEY,
	name text NOT NULL,
	kind text NOT NULL
);
INSERT INTO inventory.items (id, name, kind) VALUES
	('coin','Coin','currency'),
	('starter_sword','Starter Sword','weapon'),
	('health_potion','Health Potion','consumable')
ON CONFLICT (id) DO NOTHING;

CREATE TABLE IF NOT EXISTS inventory.holdings (
	owner_type text NOT NULL,                 -- 'player' | 'character'
	owner_id   uuid NOT NULL,                 -- ref player/character id, no cross-module FK
	item_id    text NOT NULL REFERENCES inventory.items(id),
	quantity   int  NOT NULL CHECK (quantity >= 0),
	PRIMARY KEY (owner_type, owner_id, item_id)
);
CREATE INDEX IF NOT EXISTS holdings_owner_idx ON inventory.holdings(owner_type, owner_id);"#;

/// Folds any lower-level error into an `Internal` operation error.
fn internal<E: std::fmt::Display>(e: E) -> Error {
    Error::internal(e.to_string())
}

/// Env boolean mirroring Go's `envBool` (`"1"`/`"true"`/`"on"`, case-insensitive).
fn env_bool(key: &str, def: bool) -> bool {
    match std::env::var(key) {
        Ok(v) if !v.is_empty() => {
            v == "1" || v.eq_ignore_ascii_case("true") || v.eq_ignore_ascii_case("on")
        }
        _ => def,
    }
}

// ============================================================================
// Owner — the polymorphic owner. Referenced by id, no cross-module FK.
// ============================================================================

/// Who an inventory belongs to. `otype` is `"player"` or `"character"`; `id` is the
/// player/character uuid. The polymorphism lives entirely inside this module.
struct Owner {
    otype: String,
    id: String,
}

impl Owner {
    fn player(id: impl Into<String>) -> Owner {
        Owner { otype: "player".into(), id: id.into() }
    }
    fn character(id: impl Into<String>) -> Owner {
        Owner { otype: "character".into(), id: id.into() }
    }
    fn new(otype: &str, id: &str) -> Owner {
        Owner { otype: otype.into(), id: id.into() }
    }
}

// ============================================================================
// Store — the SQL layer. Grant/clear have a `&mut PgConnection` variant so the
// event-driven effect runs INSIDE the messaging inbox-dedup tx; reads use the pool.
// ============================================================================

struct Store {
    pool: PgPool,
}

/// One row of the admin owners list: an owner with its holding count + total qty.
struct OwnerStat {
    owner_type: String,
    owner_id: String,
    items: i64,
    qty: i64,
}

impl Store {
    /// Grants `qty` of `item_id` to `owner` on the given connection (a tx, so the
    /// grant + the inbox dedup row commit together). ON CONFLICT ADDS to the existing
    /// stack (`quantity = quantity + EXCLUDED.quantity`) — the exact Go math.
    async fn grant_exec(
        &self,
        conn: &mut PgConnection,
        owner: &Owner,
        item_id: &str,
        qty: i64,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO inventory.holdings (owner_type, owner_id, item_id, quantity) \
             VALUES ($1, $2::uuid, $3, $4) \
             ON CONFLICT (owner_type, owner_id, item_id) \
             DO UPDATE SET quantity = inventory.holdings.quantity + EXCLUDED.quantity",
        )
        .bind(&owner.otype)
        .bind(&owner.id)
        .bind(item_id)
        .bind(qty)
        .execute(&mut *conn)
        .await?;
        Ok(())
    }

    /// The pool-backed grant (the player IAP path): acquires a connection and runs
    /// `grant_exec` against it. Not the durable-event path — that hands its own tx.
    async fn grant_pool(&self, owner: &Owner, item_id: &str, qty: i64) -> Result<(), sqlx::Error> {
        let mut conn = self.pool.acquire().await?;
        self.grant_exec(&mut conn, owner, item_id, qty).await
    }

    async fn list(&self, owner: &Owner) -> Result<Vec<Holding>, sqlx::Error> {
        let rows: Vec<(String, String, String, String, i64)> = sqlx::query_as(
            "SELECT h.owner_type, h.owner_id::text, h.item_id, i.name, h.quantity::bigint \
               FROM inventory.holdings h \
               JOIN inventory.items i ON i.id = h.item_id \
              WHERE h.owner_type = $1 AND h.owner_id = $2::uuid \
              ORDER BY h.item_id",
        )
        .bind(&owner.otype)
        .bind(&owner.id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(owner_type, owner_id, item_id, item_name, quantity)| Holding {
                owner_type,
                owner_id,
                item_id,
                item_name,
                quantity,
            })
            .collect())
    }

    /// Removes every holding of an owner — the event-driven cleanup when a character
    /// (or later a player) is deleted. Runs on the sink's tx (`&mut PgConnection`).
    async fn clear_owner_exec(&self, conn: &mut PgConnection, owner: &Owner) -> Result<u64, sqlx::Error> {
        let res = sqlx::query("DELETE FROM inventory.holdings WHERE owner_type = $1 AND owner_id = $2::uuid")
            .bind(&owner.otype)
            .bind(&owner.id)
            .execute(&mut *conn)
            .await?;
        Ok(res.rows_affected())
    }

    async fn item_exists(&self, item_id: &str) -> Result<bool, sqlx::Error> {
        let (ok,): (bool,) = sqlx::query_as("SELECT EXISTS(SELECT 1 FROM inventory.items WHERE id = $1)")
            .bind(item_id)
            .fetch_one(&self.pool)
            .await?;
        Ok(ok)
    }

    async fn stats(&self) -> Result<(i64, i64), sqlx::Error> {
        let row: (i64, i64) = sqlx::query_as(
            "SELECT (SELECT count(*) FROM inventory.holdings), \
                    (SELECT count(*) FROM (SELECT DISTINCT owner_type, owner_id FROM inventory.holdings) t)",
        )
        .fetch_one(&self.pool)
        .await?;
        Ok(row)
    }

    async fn list_owners(&self, limit: i64) -> Result<Vec<OwnerStat>, sqlx::Error> {
        let rows: Vec<(String, String, i64, i64)> = sqlx::query_as(
            "SELECT owner_type, owner_id::text, count(*), coalesce(sum(quantity),0)::bigint \
               FROM inventory.holdings \
              GROUP BY owner_type, owner_id \
              ORDER BY owner_type, owner_id \
              LIMIT $1",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(owner_type, owner_id, items, qty)| OwnerStat { owner_type, owner_id, items, qty })
            .collect())
    }
}

// ============================================================================
// Inner — the shared service state. Backs the `Holdings` capability (registry +
// generated edge face + gateway invokers), the two durable event effects
// (grant-starter/wipe), and the local admin render. One `Arc<Inner>` is handed to
// every path so they share the same store, ownership dep, and materialized starter.
// ============================================================================

struct Starter {
    item: String,
    qty: i64,
}

pub struct Inner {
    store: Store,
    /// The `characters` ownership capability backing `list_character`'s authz.
    /// Resolved in `init` (phase 2) — the service is Provided in `register` (phase 1)
    /// BEFORE `require` can run, exactly as Go sets `m.svc.characters` in Init.
    ownership: OnceLock<Arc<dyn Ownership>>,
    /// The mandatory `config` reader; resolved in `init` (a hard dependency).
    cfg: OnceLock<Arc<dyn Config>>,
    /// The MATERIALIZED starter spec: resolved once lazily (double-checked under the
    /// RwLock), rebuilt ONLY by `on_config_changed` on a `config.changed` event.
    starter: RwLock<Option<Starter>>,
}

impl Inner {
    fn ownership(&self) -> &Arc<dyn Ownership> {
        self.ownership
            .get()
            .expect("inventory.init must resolve characters ownership before use")
    }

    /// Resolves the starter spec into `Starter`. Caller holds the write lock. config
    /// is mandatory, so this always reads the two keys, falling back to the consts.
    fn load_starter_locked(&self) -> Starter {
        let cfg = self.cfg.get().expect("inventory.init must resolve config before use");
        Starter {
            item: cfg.get_string("inventory", "starter_item", STARTER_ITEM),
            qty: cfg.get_int("inventory", "starter_qty", STARTER_QTY),
        }
    }

    /// The materialized starter item + quantity, lazily loaded on first use under the
    /// double-checked lock (order-independent — no reliance on config's listener
    /// having started), then served from cache until `on_config_changed` rebuilds it.
    fn starter_spec(&self) -> (String, i64) {
        {
            let g = self.starter.read().unwrap();
            if let Some(s) = g.as_ref() {
                return (s.item.clone(), s.qty);
            }
        }
        let mut g = self.starter.write().unwrap();
        if g.is_none() {
            // double-check: another thread may have loaded it between the locks.
            *g = Some(self.load_starter_locked());
        }
        let s = g.as_ref().unwrap();
        (s.item.clone(), s.qty)
    }

    /// Rebuilds the materialized starter spec when a relevant config key changes. The
    /// ONLY spec-refresh path, so the `on(CHANGED)` subscription in `init` is
    /// load-bearing — without it a running inventory would never see an /admin edit.
    fn on_config_changed(&self, e: configevents::Changed) {
        if e.namespace != "inventory" || (e.key != "starter_item" && e.key != "starter_qty") {
            return;
        }
        let (item, qty) = {
            let mut g = self.starter.write().unwrap();
            let s = self.load_starter_locked();
            let out = (s.item.clone(), s.qty);
            *g = Some(s);
            out
        };
        tracing::info!(%item, qty, "inventory starter reloaded from config");
    }

    /// Grants a brand-new character its starter item. `conn` is the messaging
    /// transport's per-subscriber inbox-dedup tx (never the pool), so the grant
    /// commits atomically with the `(event_id,"inventory")` dedup row. The item +
    /// quantity come from the materialized (config-sourced, live-reloaded) spec.
    async fn grant_starter(&self, conn: &mut PgConnection, character_id: &str) -> Result<(), bus::Error> {
        let (item, qty) = self.starter_spec();
        self.store
            .grant_exec(conn, &Owner::character(character_id), &item, qty)
            .await
            .map_err(bus::Error::transport)
    }

    /// Removes a deleted character's holdings. Same handed-tx contract as
    /// `grant_starter` — atomic with the inbox dedup row.
    async fn wipe_character(&self, conn: &mut PgConnection, character_id: &str) -> Result<(), bus::Error> {
        self.store
            .clear_owner_exec(conn, &Owner::character(character_id))
            .await
            .map(|_| ())
            .map_err(bus::Error::transport)
    }
}

// Compile-time proof the shared state satisfies the generated player contract.
#[async_trait]
impl Holdings for Inner {
    /// The caller's own player-owned holdings (player_id from `identity`, NEVER an arg).
    async fn list_mine(&self, identity: Identity) -> Result<Vec<Holding>, Error> {
        let pid = identity
            .player_id()
            .ok_or_else(|| Error::invalid("missing player identity"))?;
        self.store.list(&Owner::player(pid)).await.map_err(internal)
    }

    /// A character's holdings, only if the caller owns it. The differentiated
    /// outcomes: an ownership-lookup transport failure → Unavailable (503), an unknown
    /// character → NotFound (404), a character owned by someone else → Forbidden (403).
    async fn list_character(&self, identity: Identity, character_id: String) -> Result<Vec<Holding>, Error> {
        let pid = identity
            .player_id()
            .ok_or_else(|| Error::invalid("missing player identity"))?
            .to_string();
        // characters may be hosted in a peer process; a transport failure is an
        // infrastructure problem, not a missing character.
        let owner = match self.ownership().owner_of(character_id.clone()).await {
            Ok(owner) => owner,
            Err(_) => return Err(Error::unavailable("characters service unavailable")),
        };
        let Some(owner_pid) = owner else {
            return Err(Error::not_found("not found"));
        };
        if owner_pid != pid {
            return Err(Error::forbidden("forbidden"));
        }
        self.store
            .list(&Owner::character(character_id))
            .await
            .map_err(internal)
    }

    /// Adds `qty` of `item_id` to the caller's own inventory (simulated IAP). A
    /// non-positive qty or an unknown item is Invalid (→ 400). Returns the updated
    /// holdings, matching the old handler's respond-with-list behaviour.
    async fn grant(&self, identity: Identity, item_id: String, qty: i64) -> Result<Vec<Holding>, Error> {
        let pid = identity
            .player_id()
            .ok_or_else(|| Error::invalid("missing player identity"))?
            .to_string();
        if qty <= 0 {
            return Err(Error::invalid("qty must be positive"));
        }
        if !self.store.item_exists(&item_id).await.map_err(internal)? {
            return Err(Error::invalid("unknown item"));
        }
        let owner = Owner::player(pid);
        self.store.grant_pool(&owner, &item_id, qty).await.map_err(internal)?;
        self.store.list(&owner).await.map_err(internal)
    }
}

// ============================================================================
// Admin — two views off the SAME item, switched by the ?owner= drill-down param.
// ============================================================================

/// Renders the owners list (no `?owner=`) or one owner's items (`?owner=<type>:<id>`).
async fn admin_content(store: &Store, params: &adminapi::Params) -> anyhow::Result<adminapi::Content> {
    let owner = adminapi::param(params, "owner");
    if owner.is_empty() {
        admin_owners_list(store).await
    } else {
        admin_owner_detail(store, owner).await
    }
}

/// The top-level view: KPIs plus one row per owner, the owner-id cell linking to that
/// owner's items page (`inventory?owner=<type>:<id>`).
async fn admin_owners_list(store: &Store) -> anyhow::Result<adminapi::Content> {
    let (holdings, owners) = store.stats().await?;
    let rows = store.list_owners(ADMIN_OWNERS_LIMIT).await?;

    let mut table = adminapi::Table {
        columns: vec!["OWNER".into(), "OWNER ID".into(), "ITEMS".into(), "TOTAL QTY".into()],
        rows: Vec::with_capacity(rows.len()),
    };
    for o in rows {
        table.rows.push(vec![
            adminapi::Cell {
                text: o.owner_type.clone(),
                badge: owner_badge(&o.owner_type).into(),
                ..Default::default()
            },
            adminapi::Cell {
                text: o.owner_id.clone(),
                mono: true,
                link: format!("{ADMIN_ITEM_ID}?owner={}:{}", o.owner_type, o.owner_id),
                ..Default::default()
            },
            adminapi::Cell::text(o.items.to_string()),
            adminapi::Cell::text(o.qty.to_string()),
        ]);
    }

    Ok(adminapi::Content {
        kpis: vec![
            adminapi::Kpi { label: "Holdings".into(), value: holdings.to_string(), sub: String::new() },
            adminapi::Kpi { label: "Owners".into(), value: owners.to_string(), sub: "players + characters".into() },
        ],
        table: Some(table),
        form: None,
    })
}

/// The drill-down view for one owner (`"<type>:<id>"`): its items. A malformed owner
/// param renders an error card (not a 500).
async fn admin_owner_detail(store: &Store, owner: &str) -> anyhow::Result<adminapi::Content> {
    let Some((otype, id)) = owner.split_once(':') else {
        return Ok(error_content("Invalid owner — expected player:<uuid> or character:<uuid>."));
    };
    if otype != "player" && otype != "character" {
        return Ok(error_content("Invalid owner — expected player:<uuid> or character:<uuid>."));
    }
    if !is_uuid(id) {
        return Ok(error_content("Invalid owner id — not a uuid."));
    }

    let holdings = store.list(&Owner::new(otype, id)).await?;
    let mut table = adminapi::Table {
        columns: vec!["ITEM".into(), "QTY".into()],
        rows: Vec::with_capacity(holdings.len()),
    };
    for h in &holdings {
        table.rows.push(vec![
            adminapi::Cell::text(&h.item_name),
            adminapi::Cell::text(h.quantity.to_string()),
        ]);
    }

    Ok(adminapi::Content {
        kpis: vec![
            adminapi::Kpi { label: "Owner".into(), value: otype.into(), sub: owner_badge_sub(otype).into() },
            adminapi::Kpi { label: "Owner ID".into(), value: id.into(), sub: String::new() },
            adminapi::Kpi { label: "Items".into(), value: holdings.len().to_string(), sub: String::new() },
        ],
        table: Some(table),
        form: None,
    })
}

/// A canonical 8-4-4-4-12 hex uuid check — guards the drill-down param before it
/// reaches the store's `$id::uuid` cast, so a malformed id renders an error card
/// instead of a Postgres cast error (a 500). Avoids a uuid dependency (Go's `isUUID`).
fn is_uuid(s: &str) -> bool {
    if s.len() != 36 {
        return false;
    }
    for (i, c) in s.chars().enumerate() {
        if i == 8 || i == 13 || i == 18 || i == 23 {
            if c != '-' {
                return false;
            }
        } else if !c.is_ascii_hexdigit() {
            return false;
        }
    }
    true
}

fn owner_badge(owner_type: &str) -> &'static str {
    if owner_type == "character" {
        "blue"
    } else {
        "grey"
    }
}

fn owner_badge_sub(owner_type: &str) -> &'static str {
    if owner_type == "character" {
        "character-scoped"
    } else {
        "player-scoped"
    }
}

/// Renders a single message as an error card (a lone KPI, so the page is a clean
/// card, never a 500).
fn error_content(msg: &str) -> adminapi::Content {
    adminapi::Content {
        kpis: vec![adminapi::Kpi { label: "Error".into(), value: msg.into(), sub: String::new() }],
        table: None,
        form: None,
    }
}

// ============================================================================
// Module — the lifecycle wiring.
// ============================================================================

/// The inventory module. Holds the constructed shared state and, in a split that
/// hosts it, the shared QUIC edge server onto which the generated RPC face is installed.
pub struct Inventory {
    inner: OnceLock<Arc<Inner>>,
    /// When set, the process-wide QUIC RPC server (built by `main`). `init` installs
    /// the `inventory.*` player-op handlers on it so a front gateway in a peer can
    /// route player-facing inventory operations here. `None` in the monolith.
    edge: Option<Arc<Mutex<edge::Server>>>,
}

impl Default for Inventory {
    fn default() -> Self {
        Inventory::new()
    }
}

impl Inventory {
    pub fn new() -> Inventory {
        Inventory { inner: OnceLock::new(), edge: None }
    }

    /// An inventory module that exposes its player capability over the shared edge
    /// server (a split process that hosts this module).
    pub fn with_edge(edge: Arc<Mutex<edge::Server>>) -> Inventory {
        Inventory { inner: OnceLock::new(), edge: Some(edge) }
    }

    fn inner(&self) -> Arc<Inner> {
        self.inner
            .get()
            .expect("inventory.register must run before init/start")
            .clone()
    }
}

#[async_trait]
impl Module for Inventory {
    fn name(&self) -> &str {
        "inventory"
    }

    fn requires(&self) -> Vec<String> {
        vec!["characters".into(), "config".into(), "messaging".into()]
    }

    fn caps(&self) -> Caps {
        Caps::REGISTER | Caps::MIGRATE
    }

    /// Phase 1, BEFORE any `init`: builds the store-backed shared state (from
    /// `ctx.db()`) and offers it under `inventory.holdings`, so a dependent's
    /// `require` resolves regardless of registration order. It touches only the pool;
    /// the ownership + config deps are injected later in `init` (phase 2), matching Go
    /// where `m.svc.characters`/`m.cfg` are set in Init.
    fn register(&self, ctx: &Context) -> anyhow::Result<()> {
        let pool = ctx
            .db()
            .ok_or_else(|| anyhow::anyhow!("inventory requires a DB pool"))?
            .clone();
        let inner = Arc::new(Inner {
            store: Store { pool },
            ownership: OnceLock::new(),
            cfg: OnceLock::new(),
            starter: RwLock::new(None),
        });
        self.inner
            .set(inner.clone())
            .map_err(|_| anyhow::anyhow!("inventory.register ran twice"))?;

        ctx.registry()
            .provide::<dyn Holdings>(key("inventory", "holdings"), inner);
        Ok(())
    }

    /// Creates this module's own schema. Idempotent.
    async fn migrate(&self, ctx: &Context) -> anyhow::Result<()> {
        let pool = ctx
            .db()
            .ok_or_else(|| anyhow::anyhow!("inventory requires a DB pool"))?;
        sqlx::raw_sql(SCHEMA_DDL).execute(pool).await?;
        Ok(())
    }

    /// Only wires up — no I/O (#8). EXACT order mirrors Go's `Init`:
    ///   1. resolve `characters` ownership → inject into the shared state,
    ///   2/3. the two DURABLE `on_tx` subscriptions (grant-starter/wipe, on the HANDED
    ///        conn so the effect is atomic with the inbox dedup row),
    ///   4. resolve `config` (HARD — fail-loud, this is why config is in `requires`),
    ///   5. the SYNC `on(config.changed)` — the only starter-spec refresh path,
    ///   6/7. contribute the player operations (grant dev-gated) + the local admin item,
    ///   and, if a shared edge server is held, the generated RPC face.
    fn init(&self, ctx: &Context) -> anyhow::Result<()> {
        let inner = self.inner();

        // 1. The ownership capability backs list_character's authz; the registry
        // resolves it to the real service (monolith) or the generated edge client
        // (split). Inject it into the state Provided in register.
        let ownership = ctx.registry().require::<dyn Ownership>(&key("characters", "ownership"));
        let _ = inner.ownership.set(ownership);

        // 2/3. React to character lifecycle — integrity without a cross-module FK.
        // DURABLE subscriptions on the messaging plane: the transport runs each effect
        // inside a per-(event_id,"inventory") inbox-dedup tx in BOTH topologies. The
        // effect runs on the HANDED conn so the grant/wipe commits atomically with the
        // dedup row.
        let granter = inner.clone();
        ctx.bus().on_tx(&charactersevents::CREATED, "inventory", move |conn, e: charactersevents::Created| {
            let granter = granter.clone();
            Box::pin(async move { granter.grant_starter(conn, &e.character_id).await })
        });
        let wiper = inner.clone();
        ctx.bus().on_tx(&charactersevents::DELETED, "inventory", move |conn, e: charactersevents::Deleted| {
            let wiper = wiper.clone();
            Box::pin(async move { wiper.wipe_character(conn, &e.character_id).await })
        });

        // 4. HARD dependency on config (declared in requires()): every binary that
        // hosts inventory also hosts config, so this fails loud at boot rather than
        // silently degrading to the starter consts.
        let cfg = ctx.registry().require::<dyn Config>(&key("config", "reader"));
        let _ = inner.cfg.set(cfg);

        // 5. The SYNC config.changed subscription — the ONLY path that rebuilds the
        // materialized starter spec, so editing inventory/starter_item in /admin flows
        // config.changed -> here -> the next grant uses the new item.
        let watcher = inner.clone();
        ctx.bus().on(&configevents::CHANGED, move |e: configevents::Changed| {
            watcher.on_config_changed(e);
        });

        // 6. Player operations: contribute each generated op (route + HTTP↔wire binding
        // + in-process invoker) so the gateway fronts GET /inventory/me + GET
        // /inventory/character/{id} (and, dev-gated, POST /inventory/me/grant),
        // authenticates once, and dispatches with the verified player_id in identity.
        let dev_grant = env_bool("INVENTORY_DEV_GRANT", true);
        if dev_grant {
            tracing::warn!(
                "INVENTORY_DEV_GRANT is ON — POST /inventory/me/grant (simulated IAP) is enabled; turn OFF in production"
            );
        }
        for op in holdings_rpc::operations(inner.clone()) {
            // grant is contributed only when dev-grant is set (mirrors the old
            // conditional route registration).
            if !dev_grant && op.operation.method == holdings_rpc::METHOD_GRANT {
                continue;
            }
            ctx.contribute(opsapi::SLOT, op.operation);
            ctx.contribute(opsapi::BINDING_SLOT, op.binding);
            ctx.contribute(opsapi::LOCAL_SLOT, op.local);
        }

        // 7. The local admin page (owners list + ?owner= drill-down). The RenderFn is
        // synchronous but the store reads are async; no admin PORTAL renders this in
        // M1, so the closure bridges via block_in_place (needs the multi-thread runtime
        // the app boots on).
        let render_inner = inner.clone();
        ctx.contribute(
            adminapi::SLOT,
            adminapi::Item::local(
                ADMIN_ITEM_ID,
                ADMIN_SECTION,
                ADMIN_LABEL,
                Arc::new(move |params: &adminapi::Params| {
                    let render_inner = render_inner.clone();
                    tokio::task::block_in_place(|| {
                        tokio::runtime::Handle::current().block_on(admin_content(&render_inner.store, params))
                    })
                }),
            ),
        );

        // Split topology: expose the inventory player capability over the shared QUIC
        // edge server. Pure wiring; main() starts the listener after all Inits.
        if let Some(edge) = &self.edge {
            let mut server = edge.lock().unwrap();
            holdings_rpc::register_server(&mut server, inner);
        }
        Ok(())
    }
}

// ============================================================================
// Tests. The authz-mapping test uses a FAKE Ownership (no characters module); the
// starter-reload test uses a mutable FAKE Config (no DB). The DB tests target the
// local Postgres (the test DB) and SKIP cleanly when it is unreachable. In-crate so
// they can drive the private `Inner`/`Store` directly.
// ============================================================================
#[cfg(test)]
mod tests {
    use super::*;
    use opsapi::Status;
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

    /// Migrates messaging (durable transport's outbox) + inventory schemas EXACTLY
    /// ONCE per test binary — concurrent idempotent DDL across parallel tests can
    /// deadlock on catalog locks, so serialize to a single run.
    static SCHEMA_READY: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();

    async fn ensure_schema(pool: &PgPool) {
        SCHEMA_READY
            .get_or_init(|| async {
                let ctx = Context::with_db(pool.clone());
                let m = messaging::Messaging::new();
                m.register(&ctx).unwrap();
                m.migrate(&ctx).await.unwrap();
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

    /// (c) The on_tx grant-on-Created path IN-PROCESS: install a real messaging
    /// transport (live DB), register inventory's on_tx(CREATED), start the relay, emit
    /// a Created, and assert a starter holding materializes for that character.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn grant_on_created_via_on_tx() {
        let Some(pool) = test_pool().await else { return };
        ensure_schema(&pool).await;

        let ctx = Context::with_db(pool.clone());

        // messaging.register installs the durable transport BEFORE inventory.init's on_tx.
        let messaging = messaging::Messaging::new();
        messaging.register(&ctx).unwrap();

        // Provide the ownership + config deps inventory.init requires (fakes — no
        // characters/config module needed to exercise the event path).
        ctx.registry()
            .provide::<dyn Ownership>(key("characters", "ownership"), Arc::new(FakeOwnership::Miss) as Arc<dyn Ownership>);
        ctx.registry()
            .provide::<dyn Config>(key("config", "reader"), Arc::new(FakeConfig::new(STARTER_ITEM, STARTER_QTY)) as Arc<dyn Config>);

        let inv = Inventory::new();
        inv.register(&ctx).unwrap();
        inv.init(&ctx).unwrap(); // registers on_tx(CREATED/DELETED) -> subscribe_tx

        // messaging.init snapshots inventory's subscription into relay targets, then
        // start launches the relay + LISTEN loop.
        messaging.init(&ctx).unwrap();
        messaging.start(&ctx).await.unwrap();

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

        messaging.stop(&ctx).await.unwrap();

        // Cleanup: the holding + the outbox row for this character.
        cleanup_owner(&pool, &cid).await;
        let _ = sqlx::query("DELETE FROM messaging.outbox WHERE payload->>'character_id' = $1")
            .bind(&cid)
            .execute(&pool)
            .await;
    }
}
