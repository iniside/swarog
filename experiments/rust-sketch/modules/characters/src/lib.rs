//! `characters` — owns player characters (a player has N characters). It emits
//! lifecycle events (`character.created` / `character.deleted`) other modules react
//! to, and never knows who. Its player-facing operations (create/list/delete a
//! player's own characters) are exposed as `opsapi` Operations: the gateway fronts
//! the HTTP routes, authenticates ONCE, and dispatches to the service with the
//! verified caller identity threaded in. The service never reads a client-supplied
//! identity — the trust boundary lives at the gateway/edge seam. Port of Go's
//! `modules/characters`.
//!
//! The core pattern (copied by every later module): the domain write and its outbox
//! event commit in ONE transaction, via `bus::emit_tx` on the same `&mut *tx` — the
//! event is durable iff the character is. An impl crate: no other module imports it.

use std::sync::{Arc, Mutex, OnceLock};

use async_trait::async_trait;
use bus::Bus;
use charactersapi::{Character, Ownership, Player};
use lifecycle::{Caps, Context, Module};
use opsapi::{Error, Identity};
use sqlx::{PgConnection, PgPool};

/// The admin surface ids — shared by the contributed `Item` and the `Admin::admin_data`
/// reply so a (future) remote admin fetches the same Section/Label the local render carries.
const ADMIN_ITEM_ID: &str = "characters";
const ADMIN_SECTION: &str = "Game Content";
const ADMIN_LABEL: &str = "Characters";

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
// and its outbox row commit in ONE tx (create/delete); reads use the pool.
// ============================================================================

struct Store {
    pool: PgPool,
}

impl Store {
    /// Inserts a character on the given connection (a tx, so the row + its outbox row
    /// commit together) and returns it (id/created_at from `INSERT ... RETURNING`).
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
            "SELECT {COLS} FROM characters.characters WHERE player_id = $1::uuid ORDER BY created_at"
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

    /// Deletes a character only if it belongs to `player_id`; returns whether a row
    /// was removed. A malformed id is "nothing deleted" (Go's behaviour).
    async fn delete_owned_tx(
        &self,
        conn: &mut PgConnection,
        id: &str,
        player_id: &str,
    ) -> Result<bool, sqlx::Error> {
        let res = sqlx::query(
            "DELETE FROM characters.characters WHERE id = $1::uuid AND player_id = $2::uuid",
        )
        .bind(id)
        .bind(player_id)
        .execute(&mut *conn)
        .await;
        match res {
            Ok(r) => Ok(r.rows_affected() > 0),
            Err(e) if is_invalid_uuid(&e) => Ok(false),
            Err(e) => Err(e),
        }
    }

    async fn count(&self) -> Result<i64, sqlx::Error> {
        let (n,): (i64,) = sqlx::query_as("SELECT count(*) FROM characters.characters")
            .fetch_one(&self.pool)
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
/// Holds the store (for the domain writes) and the bus (for the atomic outbox emit).
pub struct Service {
    store: Store,
    bus: Arc<Bus>,
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
    /// argument). The domain INSERT + the `character.created` outbox row commit in
    /// ONE tx: the event is durable iff the character is. A missing identity or empty
    /// name is `Status::Invalid`; class defaults to `"novice"`.
    async fn create(&self, identity: Identity, name: String, class: String) -> Result<Character, Error> {
        let player_id = identity
            .player_id()
            .ok_or_else(|| Error::invalid("missing player identity"))?
            .to_string();
        if name.trim().is_empty() {
            return Err(Error::invalid("name is required"));
        }
        let class = if class.is_empty() { "novice".to_string() } else { class };

        let mut tx = self.store.pool.begin().await.map_err(internal)?;
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
            .emit_tx(&mut tx, &charactersevents::CREATED, &evt)
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
    /// Otherwise the DELETE + the `character.deleted` outbox row commit atomically.
    async fn delete(&self, identity: Identity, character_id: String) -> Result<(), Error> {
        let player_id = identity
            .player_id()
            .ok_or_else(|| Error::invalid("missing player identity"))?
            .to_string();

        let mut tx = self.store.pool.begin().await.map_err(internal)?;
        let deleted = self
            .store
            .delete_owned_tx(&mut tx, &character_id, &player_id)
            .await
            .map_err(internal)?;
        if !deleted {
            // Nothing deleted (not found or not owned) → no event, 404. Roll back
            // EXPLICITLY (not via drop): sqlx defers a dropped tx's ROLLBACK, which
            // can leave the DELETE's locks held and deadlock a following writer. This
            // is the deterministic twin of Go's `defer tx.Rollback()`.
            tx.rollback().await.map_err(internal)?;
            return Err(Error::not_found("character not found"));
        }
        let evt = charactersevents::Deleted {
            character_id: character_id.clone(),
            player_id,
        };
        self.bus
            .emit_tx(&mut tx, &charactersevents::DELETED, &evt)
            .await
            .map_err(internal)?;
        tx.commit().await.map_err(internal)?;
        Ok(())
    }
}

#[async_trait]
impl charactersapi::Admin for Service {
    /// The admin fan-out: this module's page as `adminapi::ItemData` (same
    /// Section/Label the local `Item` carries).
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
/// the operations, and the admin render) and, in a split that hosts this module, the
/// shared QUIC edge server onto which the generated RPC faces are installed.
pub struct Characters {
    svc: OnceLock<Arc<Service>>,
    /// When set, the process-wide QUIC RPC server (built by `main`). `init` installs
    /// the `characters.ownerOf` + player-op handlers on it so a peer can resolve
    /// ownership / front the player operations over the mutually-authenticated edge.
    /// `None` in the monolith — no edge exposure. Behind `Mutex` because
    /// `register_server` needs `&mut edge::Server` while `init` has only `&self`.
    edge: Option<Arc<Mutex<edge::Server>>>,
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
            edge: None,
        }
    }

    /// A characters module that exposes its capabilities over the shared edge server
    /// (a split process that hosts this module). `main` (Step 11) builds the server,
    /// hands it here, then `listen`s it after Build.
    pub fn with_edge(edge: Arc<Mutex<edge::Server>>) -> Characters {
        Characters {
            svc: OnceLock::new(),
            edge: Some(edge),
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

    fn requires(&self) -> Vec<String> {
        vec!["messaging".to_string()]
    }

    fn caps(&self) -> Caps {
        Caps::REGISTER | Caps::MIGRATE
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
    /// local admin `Item`, and (c), if a shared edge server is held, the generated
    /// Ownership + Player RPC faces so a peer can reach `characters.*` over QUIC.
    fn init(&self, ctx: &Context) -> anyhow::Result<()> {
        let svc = self.svc();

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

        // (c) Split topology: expose Ownership + Player over the shared QUIC edge so a
        // peer resolves ownership or fronts the player ops. Pure wiring; main starts
        // the listener after all Inits.
        if let Some(edge) = &self.edge {
            let mut server = edge.lock().unwrap();
            charactersapi::ownership_rpc::register_server(&mut server, svc.clone());
            charactersapi::player_rpc::register_server(&mut server, svc);
        }
        Ok(())
    }
}

// ============================================================================
// Tests. Unit tests need no DB (validation runs before any DB work); integration
// tests target the local Postgres (the test DB) and SKIP cleanly when it is
// unreachable. In-crate so they can drive the private `Service`/`Store` directly.
// ============================================================================
#[cfg(test)]
mod tests {
    use super::*;
    use opsapi::Status;
    use std::time::Duration;

    /// Fallback DSN for the lazy-pool unit tests (the live tests read `DATABASE_URL`).
    const DEFAULT_DSN: &str =
        "postgres://gamebackend:gamebackend@localhost:5432/gamebackend?sslmode=disable";

    /// A service over a lazy pool + a transport-less bus — for the validation tests
    /// that return BEFORE any DB work or emit.
    fn lazy_service() -> Arc<Service> {
        Arc::new(Service {
            store: Store {
                pool: PgPool::connect_lazy(DEFAULT_DSN).unwrap(),
            },
            bus: Arc::new(Bus::new()),
        })
    }

    /// create validates identity then name BEFORE touching the DB, so a lazy pool
    /// that would fail on connect still yields the typed `Invalid`.
    #[tokio::test]
    async fn create_rejects_missing_identity_and_empty_name() {
        let svc = lazy_service();
        let e = svc
            .create(Identity::none(), "Aragorn".into(), String::new())
            .await
            .unwrap_err();
        assert_eq!(e.status, Status::Invalid);
        let e = svc
            .create(Identity::player("p1"), "   ".into(), String::new())
            .await
            .unwrap_err();
        assert_eq!(e.status, Status::Invalid);
    }

    /// list/delete also reject a missing identity before any DB work.
    #[tokio::test]
    async fn list_and_delete_require_identity() {
        let svc = lazy_service();
        assert_eq!(
            svc.list(Identity::none()).await.unwrap_err().status,
            Status::Invalid
        );
        assert_eq!(
            svc.delete(Identity::none(), "whatever".into())
                .await
                .unwrap_err()
                .status,
            Status::Invalid
        );
    }

    // ---- Live Postgres integration (the local DB is the test DB) ----------

    /// Opens the local Postgres; returns `None` (printing a skip line) when
    /// unreachable, so the suite RUNS but SKIPs cleanly with no DB.
    async fn test_pool() -> Option<PgPool> {
        let dsn = std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DSN.to_string());
        let pool = match tokio::time::timeout(Duration::from_secs(3), PgPool::connect(&dsn)).await {
            Ok(Ok(p)) => p,
            _ => {
                eprintln!("SKIP: postgres unreachable at {dsn} — characters DB tests skipped");
                return None;
            }
        };
        Some(pool)
    }

    /// Migrates BOTH the messaging (durable transport's outbox) and characters schemas
    /// EXACTLY ONCE per test binary. Concurrent `CREATE INDEX`/`CREATE OR REPLACE
    /// TRIGGER` across parallel tests take catalog locks that cycle into a Postgres
    /// deadlock, so the idempotent DDL must be serialized to a single run.
    static SCHEMA_READY: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();

    async fn ensure_schema(pool: &PgPool) {
        SCHEMA_READY
            .get_or_init(|| async {
                let ctx = Context::with_db(pool.clone());
                let m = messaging::Messaging::new();
                m.register(&ctx).unwrap();
                m.migrate(&ctx).await.unwrap();
                let c = Characters::new();
                c.register(&ctx).unwrap();
                c.migrate(&ctx).await.unwrap();
            })
            .await;
    }

    /// Builds a real durable plane over the live pool: schemas are migrated once
    /// (`ensure_schema`), then messaging's phase-1 `register` installs the
    /// `bus::Transport` on THIS ctx's bus (needed before any `emit_tx`), and
    /// characters registers/inits against the same ctx. Returns the ctx (owns the bus
    /// + registry) and the wired service.
    async fn wired(pool: &PgPool) -> (Context, Arc<Service>) {
        ensure_schema(pool).await;
        let ctx = Context::with_db(pool.clone());

        let messaging = messaging::Messaging::new();
        messaging.register(&ctx).unwrap();

        let chars = Characters::new();
        chars.register(&ctx).unwrap();
        chars.init(&ctx).unwrap();

        let svc = chars.svc();
        (ctx, svc)
    }

    /// A fresh random player_id (a valid uuid) so parallel test runs never collide.
    async fn unique_player(pool: &PgPool) -> String {
        let (id,): (String,) = sqlx::query_as("SELECT gen_random_uuid()::text")
            .fetch_one(pool)
            .await
            .unwrap();
        id
    }

    async fn cleanup(pool: &PgPool, players: &[&str]) {
        for pid in players {
            let _ = sqlx::query("DELETE FROM characters.characters WHERE player_id = $1::uuid")
                .bind(pid)
                .execute(pool)
                .await;
            let _ = sqlx::query("DELETE FROM messaging.outbox WHERE payload->>'player_id' = $1")
                .bind(pid)
                .execute(pool)
                .await;
        }
    }

    async fn outbox_count(pool: &PgPool, topic: &str, character_id: &str) -> i64 {
        let (n,): (i64,) = sqlx::query_as(
            "SELECT count(*) FROM messaging.outbox WHERE topic = $1 AND payload->>'character_id' = $2",
        )
        .bind(topic)
        .bind(character_id)
        .fetch_one(pool)
        .await
        .unwrap();
        n
    }

    async fn char_count(pool: &PgPool, id: &str) -> i64 {
        let (n,): (i64,) = sqlx::query_as("SELECT count(*) FROM characters.characters WHERE id = $1::uuid")
            .bind(id)
            .fetch_one(pool)
            .await
            .unwrap();
        n
    }

    /// THE ATOMIC EMIT PROOF: create writes BOTH a `characters.characters` row AND a
    /// `messaging.outbox` row (topic `character.created`) in one tx — proving
    /// `emit_tx` rode the domain transaction. Also proves the class default.
    #[tokio::test]
    async fn create_persists_character_and_outbox_event_atomically() {
        let Some(pool) = test_pool().await else { return };
        let (_ctx, svc) = wired(&pool).await;
        let pid = unique_player(&pool).await;

        // Empty class -> "novice" default.
        let c = svc
            .create(Identity::player(&pid), "Aragorn".into(), String::new())
            .await
            .unwrap();
        assert_eq!(c.player_id, pid);
        assert_eq!(c.class, "novice");

        assert_eq!(char_count(&pool, &c.id).await, 1, "character row must exist");
        assert_eq!(
            outbox_count(&pool, "character.created", &c.id).await,
            1,
            "outbox row (character.created) must exist — atomic emit_tx"
        );

        cleanup(&pool, &[&pid]).await;
    }

    /// delete of an OWNED character emits `character.deleted` (in the same tx as the
    /// delete) and removes the row; delete of a character owned by SOMEONE ELSE is a
    /// NotFound with NO event.
    #[tokio::test]
    async fn delete_emits_event_owned_and_is_notfound_unowned() {
        let Some(pool) = test_pool().await else { return };
        let (_ctx, svc) = wired(&pool).await;
        let owner = unique_player(&pool).await;
        let other = unique_player(&pool).await;

        let c = svc
            .create(Identity::player(&owner), "Legolas".into(), "archer".into())
            .await
            .unwrap();

        // Unowned delete: NotFound, no character.deleted event, row survives.
        let e = svc
            .delete(Identity::player(&other), c.id.clone())
            .await
            .unwrap_err();
        assert_eq!(e.status, Status::NotFound);
        assert_eq!(outbox_count(&pool, "character.deleted", &c.id).await, 0);
        assert_eq!(char_count(&pool, &c.id).await, 1);

        // Owned delete: succeeds, emits the event, row gone.
        svc.delete(Identity::player(&owner), c.id.clone())
            .await
            .unwrap();
        assert_eq!(outbox_count(&pool, "character.deleted", &c.id).await, 1);
        assert_eq!(char_count(&pool, &c.id).await, 0);

        cleanup(&pool, &[&owner, &other]).await;
    }

    /// owner_of: a hit returns the owner, a valid-but-absent uuid AND a malformed uuid
    /// both return `Ok(None)` (distinct from an infra error).
    #[tokio::test]
    async fn owner_of_hit_miss_and_invalid_uuid() {
        let Some(pool) = test_pool().await else { return };
        let (_ctx, svc) = wired(&pool).await;
        let pid = unique_player(&pool).await;

        let c = svc
            .create(Identity::player(&pid), "Gimli".into(), String::new())
            .await
            .unwrap();

        assert_eq!(svc.owner_of(c.id.clone()).await.unwrap(), Some(pid.clone()));

        let absent = unique_player(&pool).await; // a valid uuid, not present
        assert_eq!(svc.owner_of(absent).await.unwrap(), None);

        // Malformed id → Ok(None), NOT an error.
        assert_eq!(svc.owner_of("not-a-uuid".into()).await.unwrap(), None);

        cleanup(&pool, &[&pid]).await;
    }

    /// list returns only the caller's own characters, newest-insertion order.
    #[tokio::test]
    async fn list_returns_only_callers_characters() {
        let Some(pool) = test_pool().await else { return };
        let (_ctx, svc) = wired(&pool).await;
        let me = unique_player(&pool).await;
        let them = unique_player(&pool).await;

        svc.create(Identity::player(&me), "A".into(), String::new())
            .await
            .unwrap();
        svc.create(Identity::player(&me), "B".into(), String::new())
            .await
            .unwrap();
        svc.create(Identity::player(&them), "C".into(), String::new())
            .await
            .unwrap();

        let mine = svc.list(Identity::player(&me)).await.unwrap();
        assert_eq!(mine.len(), 2);
        assert!(mine.iter().all(|c| c.player_id == me));

        cleanup(&pool, &[&me, &them]).await;
    }
}
