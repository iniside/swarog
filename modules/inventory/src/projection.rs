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
