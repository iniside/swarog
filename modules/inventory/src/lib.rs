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

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use charactersapi::Ownership;
use configapi::Config;
use inventoryapi::{holdings_rpc, Holding, Holdings};
use lifecycle::{Context, Module};
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
CREATE INDEX IF NOT EXISTS holdings_owner_idx ON inventory.holdings(owner_type, owner_id);

-- Tombstones for wiped characters: character.created and character.deleted ride
-- INDEPENDENT durable subscriptions, and the plane's ordering contract is
-- per-subscription only (asyncevents README: "ordering is per-subscription in
-- XID-allocation order") -- so a wipe can be delivered BEFORE the grant. The wipe
-- handler plants a tombstone in its delivery tx; the grant handler skips tombstoned
-- ids. Sound because character ids are UUIDs and never recur.
CREATE TABLE IF NOT EXISTS inventory.wiped_characters (
	character_id uuid PRIMARY KEY,
	wiped_at     timestamptz NOT NULL DEFAULT now()
);"#;

/// Folds any lower-level error into an `Internal` operation error.
fn internal<E: std::fmt::Display>(e: E) -> Error {
    Error::internal(e.to_string())
}

/// Derives a stable 64-bit advisory-lock key for a character id via FNV-1a (the
/// same hash discipline as `modules/scheduler`'s `lock_key`), reinterpreted as
/// `i64` (pg advisory keys use the full signed bigint range). The seed is
/// NAMESPACED: the hash consumes the `"inventory.character/"` prefix before the
/// id, so inventory's keys cannot collide with scheduler's plain-name keys (or
/// any future module that namespaces differently). Two ids CAN still hash to the
/// same key — they then merely serialize their deliveries, never break anything.
fn lock_key(character_id: &str) -> i64 {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut h = OFFSET_BASIS;
    for b in b"inventory.character/".iter().chain(character_id.as_bytes()) {
        h ^= *b as u64;
        h = h.wrapping_mul(PRIME);
    }
    h as i64
}

/// Takes the per-character transaction-scoped advisory lock INSIDE the handed
/// delivery tx (released at commit/rollback). Both durable handlers take it FIRST,
/// so two concurrent deliveries for the same character serialize: without it, under
/// READ COMMITTED a concurrent grant could SELECT tombstone-absent while the wipe's
/// tombstone insert is still uncommitted, and both would commit — an orphaned
/// holding coexisting with a tombstone.
async fn lock_character(conn: &mut PgConnection, character_id: &str) -> Result<(), sqlx::Error> {
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(lock_key(character_id))
        .execute(conn)
        .await?;
    Ok(())
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
// event-driven effect runs INSIDE the messaging delivery tx; reads use the pool.
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
    /// grant + the checkpoint commit together in the delivery tx). ON CONFLICT ADDS to the existing
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

    /// `item_exists` on a HANDED connection — the durable-delivery variant (same
    /// shape as `grant_exec`): `grant_starter` validates the configured starter
    /// item on the SAME delivery tx it inserts on, so the check and the insert
    /// are one atomic unit (a pool-backed check would be a different connection —
    /// TOCTOU against the tx's snapshot).
    async fn item_exists_exec(&self, conn: &mut PgConnection, item_id: &str) -> Result<bool, sqlx::Error> {
        let row: Option<i32> = sqlx::query_scalar("SELECT 1 FROM inventory.items WHERE id = $1")
            .bind(item_id)
            .fetch_optional(&mut *conn)
            .await?;
        Ok(row.is_some())
    }

    /// The pool-backed item check (the player IAP path): acquires a connection and
    /// runs `item_exists_exec` against it. Not the durable-event path.
    async fn item_exists(&self, item_id: &str) -> Result<bool, sqlx::Error> {
        let mut conn = self.pool.acquire().await?;
        self.item_exists_exec(&mut conn, item_id).await
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

pub struct Inner {
    store: Store,
    /// The `characters` ownership capability backing `list_character`'s authz.
    /// Resolved in `init` (phase 2) — the service is Provided in `register` (phase 1)
    /// BEFORE `require` can run, exactly as Go sets `m.svc.characters` in Init.
    ownership: OnceLock<Arc<dyn Ownership>>,
    /// The mandatory `config` reader; resolved in `init` (a hard dependency). Read
    /// directly on every grant — no local cache: since Step 7 this reader is a
    /// replica-local `CachedConfig`/`Service` kept fresh by the app-owned broadcast
    /// invalidation plane, so a second cache here would only add staleness risk.
    cfg: OnceLock<Arc<dyn Config>>,
}

impl Inner {
    fn ownership(&self) -> &Arc<dyn Ownership> {
        self.ownership
            .get()
            .expect("inventory.init must resolve characters ownership before use")
    }

    /// Reads the starter item + quantity straight off the injected `config` reader.
    /// No local cache (Step 8): the reader is a replica-local cache already kept
    /// fresh by the app-owned broadcast invalidation plane, so a second cache here
    /// would only add a staleness window without buying anything.
    fn starter_spec(&self) -> (String, i64) {
        let cfg = self.cfg.get().expect("inventory.init must resolve config before use");
        (
            cfg.get_string("inventory", "starter_item", STARTER_ITEM),
            cfg.get_int("inventory", "starter_qty", STARTER_QTY),
        )
    }

    /// Grants a brand-new character its starter item. `conn` is the plane's handed
    /// delivery tx (never the pool), so the grant commits atomically with the
    /// subscription checkpoint. The item + quantity come from a fresh read of the
    /// injected config reader.
    ///
    /// Ordering guard: `character.created` and `character.deleted` ride
    /// INDEPENDENT subscriptions — the plane's contract is "ordering is
    /// per-subscription in XID-allocation order" (asyncevents README), so the wipe
    /// for this character may already have been delivered. After serializing on
    /// the per-character advisory xact-lock, a tombstone in
    /// `inventory.wiped_characters` means the character is gone: skip the grant
    /// and return Ok — the checkpoint still commits (exactly-once preserved).
    /// UUIDs never recur, so the tombstone is permanent truth.
    ///
    /// Config validation guard: the config-read starter spec is VALIDATED here, on
    /// the read path, and a bad value degrades to the compiled defaults with a warn
    /// — never a delivery failure. A config typo is a property of the config, not
    /// of the event, so failing the delivery would poison
    /// `inventory.character-created.v1` for every subsequent character;
    /// poison-pause stays reserved for genuinely undeliverable events. The item
    /// check runs via `item_exists_exec` on the SAME handed delivery tx as the
    /// insert (check + insert one atomic unit). Validating on the read also covers
    /// values written straight via psql, which bypass any service-side check.
    async fn grant_starter(&self, conn: &mut PgConnection, character_id: &str) -> Result<(), bus::Error> {
        lock_character(conn, character_id).await.map_err(bus::Error::transport)?;
        let tombstoned: Option<i32> =
            sqlx::query_scalar("SELECT 1 FROM inventory.wiped_characters WHERE character_id = $1::uuid")
                .bind(character_id)
                .fetch_optional(&mut *conn)
                .await
                .map_err(bus::Error::transport)?;
        if tombstoned.is_some() {
            tracing::info!(
                character_id,
                "skipping starter grant — character already wiped (deleted delivered before created)"
            );
            return Ok(());
        }
        let (mut item, mut qty) = self.starter_spec();
        if qty <= 0 {
            // Negative would trip the holdings CHECK (quantity >= 0) — a poison;
            // zero would be a silent no-op grant. Both degrade to the default.
            tracing::warn!(
                qty,
                default = STARTER_QTY,
                "inventory: configured starter_qty invalid — using default"
            );
            qty = STARTER_QTY;
        }
        if !self
            .store
            .item_exists_exec(&mut *conn, &item)
            .await
            .map_err(bus::Error::transport)?
        {
            // An unknown item would trip the in-module FK on insert — a poison.
            // The compiled default `starter_sword` is seeded by this module's OWN
            // idempotent migrate DDL in its own schema, so the fallback row is
            // guaranteed present — the FK cannot fire on the default.
            tracing::warn!(
                %item,
                default = STARTER_ITEM,
                "inventory: configured starter_item unknown — using default"
            );
            item = STARTER_ITEM.to_string();
        }
        self.store
            .grant_exec(conn, &Owner::character(character_id), &item, qty)
            .await
            .map_err(bus::Error::transport)
    }

    /// Removes a deleted character's holdings. Same handed-tx contract as
    /// `grant_starter` — atomic with the subscription checkpoint. Takes the same
    /// per-character advisory xact-lock first, then plants the permanent tombstone
    /// (idempotent — redelivery hits ON CONFLICT DO NOTHING) BEFORE the delete, in
    /// the SAME delivery tx, so a grant delivered after this commit (or blocked on
    /// the lock until it) always sees the tombstone.
    async fn wipe_character(&self, conn: &mut PgConnection, character_id: &str) -> Result<(), bus::Error> {
        lock_character(conn, character_id).await.map_err(bus::Error::transport)?;
        sqlx::query(
            "INSERT INTO inventory.wiped_characters (character_id) VALUES ($1::uuid) ON CONFLICT DO NOTHING",
        )
        .bind(character_id)
        .execute(&mut *conn)
        .await
        .map_err(bus::Error::transport)?;
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

#[async_trait]
impl adminapi::AdminData for Inner {
    /// The admin fan-out (`admin.adminData` on the edge): this module's page as
    /// `adminapi::ItemData`. The REMOTE view is the owners LIST (no `?owner=`
    /// drill-down — that interactive view is LOCAL-only, driven by the portal's query
    /// params which do not ride this wire call).
    async fn admin_data(&self) -> Result<adminapi::ItemData, Error> {
        let content = admin_content(&self.store, &adminapi::Params::new())
            .await
            .map_err(internal)?;
        Ok(adminapi::ItemData {
            id: ADMIN_ITEM_ID.into(),
            section: ADMIN_SECTION.into(),
            label: ADMIN_LABEL.into(),
            content,
        })
    }
}

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

/// The inventory module. Holds the constructed shared state. Edge exposure is
/// topology-blind: `init` contributes the generated RPC face to `edge::EDGE_SLOT`
/// unconditionally, and `app::run` installs it iff this process serves an internal
/// QUIC edge — the module never knows.
pub struct Inventory {
    inner: OnceLock<Arc<Inner>>,
}

impl Default for Inventory {
    fn default() -> Self {
        Inventory::new()
    }
}

impl Inventory {
    pub fn new() -> Inventory {
        Inventory { inner: OnceLock::new() }
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
        vec!["characters".into(), "config".into()]
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

    /// Only wires up — no I/O (#8). EXACT order mirrors Go's `Init`, minus the config
    /// cache Step 8 removed:
    ///   1. resolve `characters` ownership → inject into the shared state,
    ///   2/3. the two DURABLE `on_tx` subscriptions (grant-starter/wipe, on the HANDED
    ///        conn so the effect is atomic with the checkpoint commit in the delivery tx),
    ///   4. resolve `config` (HARD — fail-loud, this is why config is in `requires`);
    ///      `grant_starter` reads it directly, no local cache/subscription needed,
    ///   5/6. contribute the player operations (grant dev-gated) + the local admin item,
    ///   and the generated RPC face to the edge slot (applied by `app::run` iff this
    ///   process serves an internal edge).
    fn init(&self, ctx: &Context) -> anyhow::Result<()> {
        let inner = self.inner();

        // 1. The ownership capability backs list_character's authz; the registry
        // resolves it to the real service (monolith) or the generated edge client
        // (split). Inject it into the state Provided in register.
        let ownership = ctx.registry().require::<dyn Ownership>(&key("characters", "ownership"));
        let _ = inner.ownership.set(ownership);

        // 2/3. React to character lifecycle. Two INDEPENDENT durable subscriptions,
        // and the plane's contract is per-subscription ordering only ("ordering is
        // per-subscription in XID-allocation order" — asyncevents README): a
        // character's `deleted` can be delivered before its `created`. Integrity is
        // therefore a consumer-side tombstone, not a cross-module FK: the wipe
        // handler plants `inventory.wiped_characters` in its delivery tx and the
        // grant handler skips tombstoned ids (UUIDs never recur, so the tombstone is
        // permanent truth); a per-character advisory xact-lock serializes concurrent
        // deliveries. Each effect runs on the HANDED conn so it commits atomically
        // with the subscription checkpoint in BOTH topologies.
        let granter = inner.clone();
        ctx.bus().on_tx(
            bus::SubscriptionSpec {
                id: "inventory.character-created.v1",
                start: bus::StartPosition::Genesis,
            },
            &charactersevents::CREATED,
            move |mut delivery, e: charactersevents::Created| {
                let granter = granter.clone();
                Box::pin(async move {
                    let conn = delivery.tx.downcast::<sqlx::PgConnection>()?;
                    granter.grant_starter(conn, &e.character_id).await
                })
            },
        );
        let wiper = inner.clone();
        ctx.bus().on_tx(
            bus::SubscriptionSpec {
                id: "inventory.character-deleted.v1",
                start: bus::StartPosition::Genesis,
            },
            &charactersevents::DELETED,
            move |mut delivery, e: charactersevents::Deleted| {
                let wiper = wiper.clone();
                Box::pin(async move {
                    let conn = delivery.tx.downcast::<sqlx::PgConnection>()?;
                    wiper.wipe_character(conn, &e.character_id).await
                })
            },
        );

        // 4. HARD dependency on config (declared in requires()): every binary that
        // hosts inventory also hosts config, so this fails loud at boot rather than
        // silently degrading to the starter consts. No local cache/subscription to
        // keep fresh (Step 8): `grant_starter` reads this reader directly on every
        // grant, and the reader is itself kept fresh by the app-owned broadcast
        // invalidation plane — so editing inventory/starter_item flows
        // config_changed -> the reader's own refresh -> the next grant sees it.
        let cfg = ctx.registry().require::<dyn Config>(&key("config", "reader"));
        let _ = inner.cfg.set(cfg);

        // 5. Player operations: contribute each generated op (route + HTTP↔wire binding
        // + in-process invoker) so the gateway fronts GET /inventory/me + GET
        // /inventory/character/{id} (and, dev-gated, POST /inventory/me/grant),
        // authenticates once, and dispatches with the verified player_id in identity.
        let dev_grant = env_bool("INVENTORY_DEV_GRANT", false);
        if dev_grant {
            tracing::warn!(
                "INVENTORY_DEV_GRANT is ON — POST /inventory/me/grant (simulated IAP) is enabled; \
                 this is an explicit local-dev opt-in, keep it OFF (the fail-closed default) in production"
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

        // 6. The local admin page (owners list + ?owner= drill-down). The RenderFn is
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

        // Edge exposure, contributed UNCONDITIONALLY — topology-blind: `app::run`
        // applies this iff the entrypoint stood up an internal edge server (then a
        // front gateway in a peer routes `inventory.*` here over QUIC); in the
        // monolith it is never applied. Own glue (sanctioned): the edge-facing
        // register_server lives in the `inventoryrpc` glue crate.
        ctx.contribute(
            edge::EDGE_SLOT,
            edge::EdgeReg::new(move |server| {
                // The admin fan-out face (`admin.adminData`), via this module's OWN
                // glue crate's re-export (no foreign rpc import).
                inventoryrpc::register_admin(server, inner.clone());
                inventoryrpc::holdings_rpc::register_server(server, inner);
            }),
        );
        Ok(())
    }
}

// ============================================================================
// Tests. The authz-mapping test uses a FAKE Ownership (no characters module). The DB
// tests target the local Postgres (the test DB) and SKIP cleanly when it is
// unreachable. In-crate so they can drive the private `Inner`/`Store` directly.
// ============================================================================
#[cfg(test)]
mod tests;
