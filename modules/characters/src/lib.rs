//! `characters` — owns player characters (a player has N characters). It emits
//! lifecycle events (`character.created` / `character.deleted`) other modules react
//! to, and never knows who. Its player-facing operations (create/list/delete a
//! player's own characters) are exposed as `opsapi` Operations: the gateway fronts
//! the HTTP routes, authenticates ONCE, and dispatches to the service with the
//! verified caller identity threaded in. The service never reads a client-supplied
//! identity — the trust boundary lives at the gateway/edge seam. Port of Go's
//! `modules/characters`.
//!
//! The core pattern (copied by every later module): the domain write and its durable
//! event append commit in ONE transaction, via `bus::emit_tx` on the same `&mut *tx` — the
//! event is durable iff the character is. An impl crate: no other module imports it.

pub mod conformance;

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use bus::{AnyTx, Bus};
use charactersapi::{Character, Ownership, Player};
use configapi::Config;
use lifecycle::{Context, Module};
use opsapi::{Error, Identity};
use registry::key;
use sqlx::{PgConnection, PgPool};

/// The admin surface ids — shared by the contributed `Item` and the `Admin::admin_data`
/// reply so a (future) remote admin fetches the same Section/Label the local render carries.
const ADMIN_ITEM_ID: &str = "characters";
const ADMIN_SECTION: &str = "Game Content";
const ADMIN_LABEL: &str = "Characters";

const MAX_NAME_BYTES: usize = 128;
const MAX_CLASS_BYTES: usize = 64;

/// Hard safety-belt ceiling on a single list response so it is never unbounded across
/// topologies (monolith direct call has no frame; split has 16 MiB internal / 1 MiB player
/// frame caps). A belt, not the policy limit: the configurable per-player cap in `create()`
/// (added by the P2 cap step, and clamped to this ceiling so `create` can never admit more
/// characters than `list` can return) is the primary bound. Until that cap lands this ceiling
/// is also the de-facto per-player limit and truncates silently beyond it.
const LIST_HARD_LIMIT: i64 = 1000;

/// Default per-player character cap when `characters/max_per_player` is unset in
/// config. Clamped to [`LIST_HARD_LIMIT`] at read so `create` can never admit more
/// characters than `list` can return.
const MAX_PER_PLAYER: i64 = 10;

/// Per-PLAYER transaction-scoped advisory-lock key for `create`'s cap gate: two
/// concurrent creates for one player must serialize their count-then-insert, or both
/// SELECT `count` below the cap (neither committed yet, READ COMMITTED) and both
/// insert past it. FNV-1a over a DISTINCT namespace prefix (`characters.player/`) so
/// the key can NEVER collide with inventory's `inventory.character/` keys or
/// scheduler's plain-name keys — a collision would only serialize unrelated players,
/// never break correctness.
///
/// The player_id is normalized to Postgres's uuid-EQUALITY form BEFORE hashing (the
/// same discipline as inventory's `lock_key`, DELIBERATELY DUPLICATED across the two
/// fortresses — characters cannot import inventory's impl crate, so a shared helper
/// is impossible and this small copy is correct): the row SQL is `$1::uuid`, so a
/// differently-spelled but DB-equal id (uppercase / braced / unhyphenated) must yield
/// the SAME lock. Two inputs Postgres's `::uuid` treats as equal share the same 32
/// ascii-hex digits ignoring case/hyphens/braces; a non-uuid input (never on the
/// identity-validated path) falls back to its raw bytes — still stable, only ever
/// over-serializes, never breaks.
fn player_lock_key(player_id: &str) -> i64 {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let hex: Vec<u8> = player_id
        .bytes()
        .filter(u8::is_ascii_hexdigit)
        .map(|b| b.to_ascii_lowercase())
        .collect();
    let normalized: &[u8] = if hex.len() == 32 { &hex } else { player_id.as_bytes() };
    let mut h = OFFSET_BASIS;
    for b in b"characters.player/".iter().copied().chain(normalized.iter().copied()) {
        h ^= b as u64;
        h = h.wrapping_mul(PRIME);
    }
    h as i64
}

pub(crate) fn name_within_cap(name: &str) -> bool {
    name.len() <= MAX_NAME_BYTES
}

pub(crate) fn class_within_cap(class: &str) -> bool {
    class.len() <= MAX_CLASS_BYTES
}

/// Creates this module's OWN schema and nothing else — full logical isolation (#10).
/// Idempotent. Verbatim from Go's `schemaDDL`: `player_id` is a plain ref to
/// accounts.players with NO cross-module FK; the index keeps a player's list cheap.
const SCHEMA_DDL: &str = r#"
CREATE SCHEMA IF NOT EXISTS characters;
CREATE TABLE IF NOT EXISTS characters.characters (
	id         uuid PRIMARY KEY DEFAULT gen_random_uuid(),
	player_id  uuid        NOT NULL,
	name       text        NOT NULL,
	class      text        NOT NULL DEFAULT 'novice',
	created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS characters_player_idx ON characters.characters(player_id);"#;

/// The column list every read/insert projects, `created_at` rendered as text so it
/// flows through as the `Character::created_at` String. Kept in one place so the
/// tuple shape below matches every query.
const COLS: &str = "id::text, player_id::text, name, class, created_at::text";

/// One scanned row — the five text columns of [`COLS`], in order.
type Row = (String, String, String, String, String);

fn to_character((id, player_id, name, class, created_at): Row) -> Character {
    Character { id, player_id, name, class, created_at }
}

/// `true` for a Postgres "invalid text representation" (22P02) — a malformed uuid in
/// the request — so callers treat it as not-found rather than a 500 (Go's `invalidUUID`).
fn is_invalid_uuid(e: &sqlx::Error) -> bool {
    matches!(e, sqlx::Error::Database(db) if db.code().as_deref() == Some("22P02"))
}

/// Folds any lower-level error into an `Internal` operation error.
fn internal<E: std::fmt::Display>(e: E) -> Error {
    Error::internal(e.to_string())
}

// ============================================================================
// Store — the SQL layer. Write paths take `&mut PgConnection` so the domain row
// and its durable event append commit in ONE tx (create/delete); reads use the pool.
// ============================================================================

struct Store {
    pool: PgPool,
}

impl Store {
    /// Inserts a character on the given connection (a tx, so the row + its durable
    /// event append commit together) and returns it (id/created_at from `INSERT ... RETURNING`).
    async fn create_tx(
        &self,
        conn: &mut PgConnection,
        player_id: &str,
        name: &str,
        class: &str,
    ) -> Result<Character, sqlx::Error> {
        let row: Row = sqlx::query_as(&format!(
            "INSERT INTO characters.characters (player_id, name, class) \
             VALUES ($1::uuid, $2, $3) RETURNING {COLS}"
        ))
        .bind(player_id)
        .bind(name)
        .bind(class)
        .fetch_one(&mut *conn)
        .await?;
        Ok(to_character(row))
    }

    async fn list_by_player(&self, player_id: &str) -> Result<Vec<Character>, sqlx::Error> {
        let rows: Vec<Row> = sqlx::query_as(&format!(
            "SELECT {COLS} FROM characters.characters WHERE player_id = $1::uuid \
             ORDER BY created_at, id LIMIT {LIST_HARD_LIMIT}"
        ))
        .bind(player_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(to_character).collect())
    }

    /// Fetches one character. A malformed id (22P02) is treated as `Ok(None)`, like a
    /// genuine miss — a real DB error propagates.
    async fn get(&self, id: &str) -> Result<Option<Character>, sqlx::Error> {
        let res = sqlx::query_as::<_, Row>(&format!(
            "SELECT {COLS} FROM characters.characters WHERE id = $1::uuid"
        ))
        .bind(id)
        .fetch_optional(&self.pool)
        .await;
        match res {
            Ok(row) => Ok(row.map(to_character)),
            Err(e) if is_invalid_uuid(&e) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Deletes a character only if it belongs to `player_id`; returns the removed
    /// row's canonical `(id, player_id)`, or `None` if nothing matched. A malformed id
    /// is "nothing deleted" (Go's behaviour). The `RETURNING id::text, player_id::text`
    /// yields BOTH DB-canonical uuids (lowercase, unbraced), so the caller emits the
    /// event with the canonical form of BOTH fields regardless of how the client
    /// spelled the id/player_id arguments — at full parity with `create_tx` (which
    /// emits `c.id`/`c.player_id` from its own `RETURNING`).
    async fn delete_owned_tx(
        &self,
        conn: &mut PgConnection,
        id: &str,
        player_id: &str,
    ) -> Result<Option<(String, String)>, sqlx::Error> {
        let res = sqlx::query_as::<_, (String, String)>(
            "DELETE FROM characters.characters WHERE id = $1::uuid AND player_id = $2::uuid \
             RETURNING id::text, player_id::text",
        )
        .bind(id)
        .bind(player_id)
        .fetch_optional(&mut *conn)
        .await;
        match res {
            Ok(Some(row)) => Ok(Some(row)),
            Ok(None) => Ok(None),
            Err(e) if is_invalid_uuid(&e) => Ok(None),
            Err(e) => Err(e),
        }
    }

    async fn count(&self) -> Result<i64, sqlx::Error> {
        let (n,): (i64,) = sqlx::query_as("SELECT count(*) FROM characters.characters")
            .fetch_one(&self.pool)
            .await?;
        Ok(n)
    }

    /// Counts how many characters a player owns, ON THE GIVEN connection (the create
    /// tx) so the count runs AFTER and UNDER the per-player advisory lock, within the
    /// same snapshot as the subsequent insert — the cap gate is only race-safe if this
    /// count and the `create_tx` insert share one serialized transaction.
    async fn count_owned_tx(
        &self,
        conn: &mut PgConnection,
        player_id: &str,
    ) -> Result<i64, sqlx::Error> {
        let (n,): (i64,) =
            sqlx::query_as("SELECT count(*) FROM characters.characters WHERE player_id = $1::uuid")
                .bind(player_id)
                .fetch_one(&mut *conn)
                .await?;
        Ok(n)
    }

    async fn list_all(&self, limit: i64) -> Result<Vec<Character>, sqlx::Error> {
        let rows: Vec<Row> = sqlx::query_as(&format!(
            "SELECT {COLS} FROM characters.characters ORDER BY created_at DESC LIMIT $1"
        ))
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(to_character).collect())
    }
}

// ============================================================================
// Service — backs Ownership + Player (the registry capabilities + the generated
// edge faces + the gateway's in-process invokers) and the local admin render.
// ============================================================================

/// What other modules get from `require::<dyn Ownership>` / `require::<dyn Player>`.
/// Holds the store (for the domain writes) and the bus (for the atomic durable event append).
pub struct Service {
    store: Store,
    bus: Arc<Bus>,
    /// The mandatory `config` reader backing `create`'s per-player cap
    /// (`characters/max_per_player`); resolved in `init` (phase 2). This is a
    /// replica-local `CachedConfig`/`Service` kept fresh by the app-owned broadcast
    /// invalidation plane, so `create` reads it directly — no characters-owned second
    /// cache (which would only add staleness).
    config: OnceLock<Arc<dyn Config>>,
}

#[async_trait]
impl Ownership for Service {
    /// The owning player of a character; a genuine miss (including a malformed id) is
    /// `Ok(None)`, an infrastructure failure is `Err` — so a consumer tells a real
    /// 404 apart from an outage.
    async fn owner_of(&self, character_id: String) -> Result<Option<String>, Error> {
        Ok(self
            .store
            .get(&character_id)
            .await
            .map_err(internal)?
            .map(|c| c.player_id))
    }
}

#[async_trait]
impl Player for Service {
    /// Adds a character owned by the caller (player_id from `identity`, NEVER an
    /// argument). The domain INSERT + the `character.created` durable event append commit in
    /// ONE tx: the event is durable iff the character is. A missing identity or empty
    /// name is `Status::Invalid`; class defaults to `"novice"`. The persisted name
    /// and class are capped at 128 and 64 UTF-8 bytes respectively.
    async fn create(&self, identity: Identity, name: String, class: String) -> Result<Character, Error> {
        let player_id = identity
            .player_id()
            .ok_or_else(|| Error::invalid("missing player identity"))?
            .to_string();
        if name.trim().is_empty() {
            return Err(Error::invalid("name is required"));
        }
        let class = if class.is_empty() { "novice".to_string() } else { class };

        if !name_within_cap(&name) {
            return Err(Error::invalid(format!(
                "name exceeds {MAX_NAME_BYTES} bytes"
            )));
        }
        if !class_within_cap(&class) {
            return Err(Error::invalid(format!(
                "class exceeds {MAX_CLASS_BYTES} bytes"
            )));
        }

        let mut tx = self.store.pool.begin().await.map_err(internal)?;

        // Per-player cap gate, race-safe by construction. Take the per-player
        // transaction-scoped advisory lock FIRST (released at commit/rollback), so two
        // concurrent creates for the same player serialize their count-then-insert:
        // whichever acquires the lock second sees the committed count and is rejected.
        // Without the lock both would SELECT `count` below the cap under READ COMMITTED
        // (neither committed yet) and both insert past it. The lock is taken ON THE TX
        // CONNECTION (`&mut *tx`) — a separate pool connection would lose serialization.
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(player_lock_key(&player_id))
            .execute(&mut *tx)
            .await
            .map_err(internal)?;
        // Clamp to LIST_HARD_LIMIT so `create` can never admit more than `list` returns.
        let cap = self
            .config
            .get()
            .expect("characters.init must resolve config before create")
            .get_int("characters", "max_per_player", MAX_PER_PLAYER)
            .clamp(1, LIST_HARD_LIMIT);
        let n = self
            .store
            .count_owned_tx(&mut tx, &player_id)
            .await
            .map_err(internal)?;
        if n >= cap {
            // Roll back EXPLICITLY (not via drop): sqlx defers a dropped tx's ROLLBACK,
            // which can leave the advisory lock held and stall a following writer — the
            // same deterministic-teardown rationale as delete's not-found arm.
            tx.rollback().await.map_err(internal)?;
            return Err(Error::conflict(format!("character limit reached ({cap})")));
        }

        let c = self
            .store
            .create_tx(&mut tx, &player_id, &name, &class)
            .await
            .map_err(internal)?;
        let evt = charactersevents::Created {
            character_id: c.id.clone(),
            player_id: c.player_id.clone(),
            name: c.name.clone(),
            class: c.class.clone(),
        };
        self.bus
            .emit_tx(AnyTx::new(&mut *tx), &charactersevents::CREATED, &evt)
            .await
            .map_err(internal)?;
        tx.commit().await.map_err(internal)?;
        Ok(c)
    }

    /// The caller's own characters (player_id from `identity`).
    async fn list(&self, identity: Identity) -> Result<Vec<Character>, Error> {
        let player_id = identity
            .player_id()
            .ok_or_else(|| Error::invalid("missing player identity"))?;
        self.store.list_by_player(player_id).await.map_err(internal)
    }

    /// Removes one of the caller's characters. Deleting a non-owned/absent character
    /// is `Status::NotFound` — and emits NO event (the tx is dropped/rolled back).
    /// Otherwise the DELETE + the `character.deleted` durable event append commit atomically.
    async fn delete(&self, identity: Identity, character_id: String) -> Result<(), Error> {
        let player_id = identity
            .player_id()
            .ok_or_else(|| Error::invalid("missing player identity"))?
            .to_string();

        let mut tx = self.store.pool.begin().await.map_err(internal)?;
        let removed = self
            .store
            .delete_owned_tx(&mut tx, &character_id, &player_id)
            .await
            .map_err(internal)?;
        let (canonical_id, canonical_player_id) = match removed {
            None => {
                // Nothing deleted (not found or not owned) → no event, 404. Roll back
                // EXPLICITLY (not via drop): sqlx defers a dropped tx's ROLLBACK, which
                // can leave the DELETE's locks held and deadlock a following writer. This
                // is the deterministic twin of Go's `defer tx.Rollback()`.
                tx.rollback().await.map_err(internal)?;
                return Err(Error::not_found("character not found"));
            }
            // Emit the DB-canonical id AND player_id (the `RETURNING` values), NOT the
            // client-echoed `character_id`/identity `player_id` arguments — so
            // `character.deleted` matches the canonical `character.created` for the same
            // row on BOTH fields (inventory lock_key + audit stay consistent even when
            // the client spelled either id uppercased/braced).
            Some(pair) => pair,
        };
        let evt = charactersevents::Deleted {
            character_id: canonical_id,
            player_id: canonical_player_id,
        };
        self.bus
            .emit_tx(AnyTx::new(&mut *tx), &charactersevents::DELETED, &evt)
            .await
            .map_err(internal)?;
        tx.commit().await.map_err(internal)?;
        Ok(())
    }
}

#[async_trait]
impl adminapi::AdminData for Service {
    /// The admin fan-out: this module's page as `adminapi::ItemData` (same
    /// Section/Label the local `Item` carries), served on the edge as
    /// `admin.adminData` so a remote admin process renders it cross-process.
    async fn admin_data(&self) -> Result<adminapi::ItemData, Error> {
        let content = admin_content(&self.store).await.map_err(internal)?;
        Ok(adminapi::ItemData {
            id: ADMIN_ITEM_ID.into(),
            section: ADMIN_SECTION.into(),
            label: ADMIN_LABEL.into(),
            content,
        })
    }
}

/// The live "Characters" block: a count KPI + a table of the newest 50 characters.
/// Reads only its own data and returns the admin's declarative widgets (the admin
/// owns the look). Async because it queries the store.
async fn admin_content(store: &Store) -> anyhow::Result<adminapi::Content> {
    let n = store.count().await?;
    let rows = store.list_all(50).await?;

    let mut table = adminapi::Table {
        columns: vec!["NAME".into(), "CLASS".into(), "PLAYER".into(), "CREATED".into()],
        rows: Vec::with_capacity(rows.len()),
    };
    for c in rows {
        table.rows.push(vec![
            adminapi::Cell::text(&c.name),
            adminapi::Cell {
                text: c.class,
                badge: "blue".into(),
                ..Default::default()
            },
            adminapi::Cell::mono(&c.player_id),
            adminapi::Cell::text(&c.created_at),
        ]);
    }

    Ok(adminapi::Content {
        kpis: vec![adminapi::Kpi {
            label: "Characters".into(),
            value: n.to_string(),
            sub: String::new(),
        }],
        table: Some(table),
        form: None,
    })
}

// ============================================================================
// Module — the lifecycle wiring.
// ============================================================================

/// The characters module. Holds the constructed service (shared between `register`,
/// the operations, and the admin render). Edge exposure is topology-blind: `init`
/// contributes the generated RPC faces to `edge::EDGE_SLOT` unconditionally, and
/// `app::run` installs them iff this process serves an internal QUIC edge — the
/// module never knows.
pub struct Characters {
    svc: OnceLock<Arc<Service>>,
}

impl Default for Characters {
    fn default() -> Self {
        Characters::new()
    }
}

impl Characters {
    pub fn new() -> Characters {
        Characters {
            svc: OnceLock::new(),
        }
    }

    fn svc(&self) -> Arc<Service> {
        self.svc
            .get()
            .expect("characters.register must run before init/start")
            .clone()
    }
}

#[async_trait]
impl Module for Characters {
    fn name(&self) -> &str {
        "characters"
    }

    /// `config` is a hard sync dependency: `create`'s per-player cap reads
    /// `characters/max_per_player` on every call. A process hosting characters without
    /// the config capability FAILS STARTUP (`app::validate_requires`).
    fn requires(&self) -> Vec<String> {
        vec!["config".into()]
    }

    /// Phase 1, BEFORE any `init`: builds the store-backed service (from `ctx.db()` +
    /// `ctx.bus()`) and offers it under BOTH capability keys — `characters.ownership`
    /// (inventory resolves it) and `characters.player` (the gateway routes it) — so a
    /// dependent's `require` resolves regardless of registration order.
    fn register(&self, ctx: &Context) -> anyhow::Result<()> {
        let pool = ctx
            .db()
            .ok_or_else(|| anyhow::anyhow!("characters requires a DB pool"))?
            .clone();
        let svc = Arc::new(Service {
            store: Store { pool },
            bus: ctx.bus().clone(),
            config: OnceLock::new(),
        });
        self.svc
            .set(svc.clone())
            .map_err(|_| anyhow::anyhow!("characters.register ran twice"))?;

        ctx.registry()
            .provide::<dyn Ownership>(registry::key("characters", "ownership"), svc.clone());
        ctx.registry()
            .provide::<dyn Player>(registry::key("characters", "player"), svc);
        Ok(())
    }

    /// Creates this module's own schema. Idempotent.
    async fn migrate(&self, ctx: &Context) -> anyhow::Result<()> {
        let pool = ctx
            .db()
            .ok_or_else(|| anyhow::anyhow!("characters requires a DB pool"))?;
        sqlx::raw_sql(SCHEMA_DDL).execute(pool).await?;
        Ok(())
    }

    /// Only wires up — no I/O (#8). Contributes (a) the three player operations into
    /// the opsapi slots so the gateway fronts POST/GET/DELETE /characters, (b) the
    /// local admin `Item`, and (c) the generated Ownership + Player RPC faces to the
    /// edge slot so a peer can reach `characters.*` over QUIC when this process
    /// serves an internal edge.
    fn init(&self, ctx: &Context) -> anyhow::Result<()> {
        let svc = self.svc();

        // Resolve the mandatory `config` reader (phase 2 — provided in some module's
        // `register` in phase 1, so it is always present here). `create`'s cap gate
        // reads it directly; in the split a `remote::Stub` swaps a `CachedConfig` under
        // the SAME key, so this line is topology-blind.
        let cfg = ctx.registry().require::<dyn Config>(&key("config", "reader"));
        let _ = svc.config.set(cfg);

        // (a) Player operations: the generated `operations()` yields one OpSet per
        // #[http] method; contribute each half to its slot (LocalBackend + the future
        // RemoteBackend consume the SAME wire envelopes).
        for op in charactersapi::player_rpc::operations(svc.clone()) {
            ctx.contribute(opsapi::SLOT, op.operation);
            ctx.contribute(opsapi::BINDING_SLOT, op.binding);
            ctx.contribute(opsapi::LOCAL_SLOT, op.local);
        }

        // (b) The local admin page. RenderFn is synchronous, but the store reads are
        // async; no admin PORTAL renders this in M1, so the closure bridges via
        // block_in_place (requires the multi-thread runtime the app boots on).
        let render_svc = svc.clone();
        ctx.contribute(
            adminapi::SLOT,
            adminapi::Item::local(
                ADMIN_ITEM_ID,
                ADMIN_SECTION,
                ADMIN_LABEL,
                Arc::new(move |_params: &adminapi::Params| {
                    let svc = render_svc.clone();
                    tokio::task::block_in_place(|| {
                        tokio::runtime::Handle::current().block_on(admin_content(&svc.store))
                    })
                }),
            ),
        );

        // (c) Edge exposure, contributed UNCONDITIONALLY — topology-blind: `app::run`
        // applies this iff the entrypoint stood up an internal edge server (then a
        // peer resolves ownership / fronts the player ops over QUIC); in the monolith
        // it is never applied. Own glue (sanctioned): the generated register_server
        // faces live in `charactersrpc`.
        ctx.contribute(
            edge::EDGE_SLOT,
            edge::EdgeReg::new(move |server| {
                charactersrpc::ownership_rpc::register_server(server, svc.clone());
                // The admin fan-out face (`admin.adminData`), registered through this
                // module's OWN glue crate's re-export so no foreign rpc is imported.
                charactersrpc::register_admin(server, svc.clone());
                charactersrpc::player_rpc::register_server(server, svc);
            }),
        );
        Ok(())
    }
}

// ============================================================================
// Tests. Unit tests need no DB (validation runs before any DB work); integration
// tests target the local Postgres (the test DB) and SKIP cleanly when it is
// unreachable. In-crate so they can drive the private `Service`/`Store` directly.
// ============================================================================
#[cfg(test)]
mod tests;
