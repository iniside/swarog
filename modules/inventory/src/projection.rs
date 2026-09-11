use std::sync::Arc;

use bus::{Delivery, Error as BusError, TxHandler};
use futures::future::BoxFuture;
use sqlx::PgConnection;

use crate::{validate_quantity, Inner, Owner};

/// The per-key DEFAULT starter spec, used when `inventory/starter_item` /
/// `inventory/starter_qty` are absent from config. `config` is a mandatory
/// dependency (`requires`), so there is no "config isn't hosted" fallback case.
pub(crate) const STARTER_ITEM: &str = "starter_sword";
pub(crate) const STARTER_QTY: i64 = 1;

/// Derives a stable 64-bit advisory-lock key for a character id via FNV-1a (the
/// same hash discipline as `modules/scheduler`'s `lock_key`), reinterpreted as
/// `i64` (pg advisory keys use the full signed bigint range). The seed is
/// NAMESPACED: the hash consumes the `"inventory.character/"` prefix before the
/// id, so inventory's keys cannot collide with scheduler's plain-name keys (or
/// any future module that namespaces differently). Two ids CAN still hash to the
/// same key — they then merely serialize their deliveries, never break anything.
///
/// The id is normalized to Postgres's uuid-EQUALITY form BEFORE hashing so a
/// differently-spelled but DB-equal id (uppercase / braced / unhyphenated) yields
/// the SAME lock: the row SQL is `$1::uuid`-normalized, and this lock MUST agree
/// with it — otherwise a grant and a wipe for the one character take DIFFERENT
/// advisory keys, fail to serialize, and commit an orphan holding alongside a
/// tombstone. Dependency-free (no `uuid` crate — the module deliberately keeps ids
/// as `String` with `::text`/`::uuid` casts): two inputs Postgres's `::uuid` treats
/// as equal share the same 32 ascii-hex digits ignoring case/hyphens/braces. A
/// non-uuid input (never on the DB-validated path) falls back to its raw bytes —
/// still stable, only ever over-serializes, never breaks.
pub(crate) fn lock_key(character_id: &str) -> i64 {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let hex: Vec<u8> = character_id
        .bytes()
        .filter(u8::is_ascii_hexdigit)
        .map(|b| b.to_ascii_lowercase())
        .collect();
    let normalized: &[u8] = if hex.len() == 32 { &hex } else { character_id.as_bytes() };
    let mut h = OFFSET_BASIS;
    for b in b"inventory.character/".iter().copied().chain(normalized.iter().copied()) {
        h ^= b as u64;
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
pub(crate) async fn lock_character(conn: &mut PgConnection, character_id: &str) -> Result<(), sqlx::Error> {
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(lock_key(character_id))
        .execute(conn)
        .await?;
    Ok(())
}

impl Inner {
    /// Reads the starter item + quantity straight off the injected `config` reader.
    /// No inventory-owned second cache (Step 8): the injected reader is already a
    /// replica-local cache kept fresh by the app-owned broadcast invalidation plane,
    /// so another cache here would only add a staleness window without buying anything.
    pub(crate) fn starter_spec(&self) -> (String, i64) {
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
    /// UUIDs never recur, so the tombstone is permanent truth. Cost: the table
    /// grows monotonically, one permanent row per deleted character, with no GC /
    /// retention / watermark. Acceptable in the current "wipe-is-migration" phase;
    /// a long-lived deployment would need a retention policy.
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
    pub(crate) async fn grant_starter(&self, conn: &mut PgConnection, character_id: &str) -> Result<(), bus::Error> {
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
        let (mut item, cfg_qty) = self.starter_spec();
        // Posture A (config-driven, DURABLE subscription): a bad admin value must
        // NEVER poison inventory.character-created.v1 for every subsequent character.
        // ANY validation failure — non-positive (negative would trip the CHECK, zero
        // is a silent no-op) OR above the cap (a huge value overflowed the old int4
        // column to SQLSTATE 22003 inside the delivery tx) — degrades to the compiled
        // default with a warn, never a delivery failure. Contrast posture B in
        // Holdings::grant, which rejects to the client (no checkpoint rides on it).
        let qty = validate_quantity(cfg_qty).unwrap_or_else(|_| {
            tracing::warn!(
                qty = cfg_qty,
                default = STARTER_QTY,
                "inventory: configured starter_qty out of range — using default"
            );
            STARTER_QTY
        });
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
    /// the lock until it) always sees the tombstone. Every plant here is a permanent
    /// row (see the growth-cost note on `grant_starter` above) — this is the only
    /// writer of `inventory.wiped_characters`.
    pub(crate) async fn wipe_character(&self, conn: &mut PgConnection, character_id: &str) -> Result<(), bus::Error> {
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

impl Inner {
    /// Once swept, the row is the permanent record that this player is gone — nothing
    /// deletes it, and a redelivery is `ON CONFLICT DO NOTHING` rather than a reset of
    /// `swept_at`. Deliberately takes no per-character advisory lock: it touches neither
    /// `inventory.holdings` nor `inventory.wiped_characters`, so it serializes against
    /// nothing the two character handlers do.
    pub(crate) async fn plant_purged_player(
        &self,
        conn: &mut PgConnection,
        player_id: &str,
        handle: &str,
    ) -> Result<(), bus::Error> {
        sqlx::query(
            "INSERT INTO inventory.purged_players (player_id, handle) VALUES ($1::uuid, $2) \
             ON CONFLICT DO NOTHING",
        )
        .bind(player_id)
        .bind(handle)
        .execute(&mut *conn)
        .await
        .map(|_| ())
        .map_err(bus::Error::transport)
    }
}

/// The purge sweep's consumer-owned subscription id — a durable contract; renaming it
/// abandons the checkpoint. `Genesis` because the sweep must run on a boot that has never
/// subscribed before.
pub(crate) const PURGE_SWEEP_SUB: bus::SubscriptionSpec = bus::SubscriptionSpec {
    id: "inventory.purge-on-scheduler.v1",
    start: bus::StartPosition::Genesis,
};

pub(crate) const PURGE_SCHEDULE_NAME: &str = schedulerevents::schedule_names::PLAYER_PURGE;

/// Tombstones claimed per selection statement. It bounds PLAYERS, not holdings — the
/// per-player delete is one statement whose size nothing here caps — and the sweep LOOPS
/// batches until a short one, so a backlog above one batch per day still drains.
pub(crate) const PURGE_BATCH: i64 = 256;

/// The sweep's whole wall-clock budget. Exhausting it ends the fire with the players swept
/// so far KEPT — they commit with the subscription checkpoint and the next fire resumes from
/// there. It is a bare constant because a module may not read the plane's env and
/// `bus::Delivery` carries no deadline to derive one from; the operator constraint that
/// comes with it is that `ASYNCEVENTS_HANDLER_TIMEOUT` must stay above 5s (its default is
/// 10s, and nothing in the tree sets it). Below that the sweep is killed rather than
/// budgeted: the plane `pg_terminate_backend`s the delivery backend, so the whole fire's
/// transaction dies with it and 20 such fires pause the subscription.
pub(crate) const SWEEP_BUDGET: std::time::Duration = std::time::Duration::from_secs(5);

/// The scan floor carried across batches, load-bearing rather than an optimisation: every
/// batch runs inside the ONE still-open delivery transaction, where the tombstones this
/// transaction has already stamped are dead-but-unprunable row versions that a
/// `swept_at IS NULL` re-scan still walks — so a floorless scan re-walks `256 x (k-1)` of
/// them and the loop goes quadratic. It buys that only while the batch query's `ORDER BY`
/// names the TABLE's `player_id`: unqualified it resolves to the `player_id::text` output
/// column, which the primary key can supply as neither the ordering nor an index condition,
/// leaving a seq scan plus a sort that spills to disk inside that transaction. Ordering and
/// filtering on the same `uuid` column also drops an unstated dependency on canonical uuid
/// text collating in uuid order. The comparison is `>=` rather than the `>` the unique key
/// would allow: it costs one already-stamped row per batch (`swept_at IS NULL` filters it
/// out) and in exchange keeps the floor value itself reachable.
const PURGE_FLOOR: &str = "00000000-0000-0000-0000-000000000000";

/// Just the `name` of a `scheduler.fired` payload: the sweep subscribes by the contract's
/// topic const without importing the producer's payload type (audit's zero-coupling shape).
#[derive(serde::Deserialize)]
struct FiredName {
    name: String,
}

impl Inner {
    /// Drains the tombstones planted by `plant_purged_player`: the PLAYER-owned holdings of
    /// players whose account is gone. Their CHARACTER-owned holdings are not this sweep's
    /// work — they arrive on the untouched `character.deleted` path, which `accounts`' purge
    /// drives through `characters`, and this module cannot enumerate a player's characters
    /// anyway.
    ///
    /// Every delete, every `swept_at` stamp and the subscription checkpoint commit together
    /// on the handed delivery tx. Takes no per-character advisory lock: `Owner::player` rows
    /// are disjoint from every key `lock_character` guards.
    pub(crate) async fn sweep_purged_players(&self, conn: &mut PgConnection) -> Result<(), BusError> {
        let started = std::time::Instant::now();
        let mut watermark = PURGE_FLOOR.to_string();
        loop {
            let due: Vec<(String,)> = sqlx::query_as(
                "SELECT player_id::text FROM inventory.purged_players \
                  WHERE swept_at IS NULL AND player_id >= $1::uuid \
                  ORDER BY inventory.purged_players.player_id \
                  LIMIT $2 FOR UPDATE SKIP LOCKED",
            )
            .bind(&watermark)
            .bind(PURGE_BATCH)
            .fetch_all(&mut *conn)
            .await
            .map_err(BusError::transport)?;
            for (player_id,) in &due {
                self.store
                    .clear_owner_exec(&mut *conn, &Owner::player(player_id.as_str()))
                    .await
                    .map_err(BusError::transport)?;
                sqlx::query(
                    "UPDATE inventory.purged_players SET swept_at = now() \
                      WHERE player_id = $1::uuid",
                )
                .bind(player_id)
                .execute(&mut *conn)
                .await
                .map_err(BusError::transport)?;
                // The budget is spent only at a player boundary — after the stamp, never
                // between a player's delete and its stamp. The sweeps that emit a terminal
                // event per deleted row depend on that placement; this one keeps it so all
                // six read the same.
                if started.elapsed() >= SWEEP_BUDGET {
                    tracing::warn!(
                        elapsed_ms = started.elapsed().as_millis() as u64,
                        "inventory: purge sweep hit its budget — resuming next fire"
                    );
                    return Ok(());
                }
            }
            if (due.len() as i64) < PURGE_BATCH {
                return Ok(());
            }
            if let Some((last,)) = due.last() {
                watermark.clone_from(last);
            }
        }
    }
}

pub(crate) struct PurgeSweepHandler {
    pub(crate) inner: Arc<Inner>,
}

impl TxHandler for PurgeSweepHandler {
    fn call<'a>(
        &'a self,
        mut delivery: Delivery<'a>,
        payload: Vec<u8>,
    ) -> BoxFuture<'a, Result<(), BusError>> {
        Box::pin(async move {
            let conn = delivery.tx.downcast::<PgConnection>()?;
            let fired: FiredName = serde_json::from_slice(&payload).map_err(BusError::from)?;
            if fired.name != PURGE_SCHEDULE_NAME {
                return Ok(());
            }
            self.inner.sweep_purged_players(conn).await
        })
    }
}
