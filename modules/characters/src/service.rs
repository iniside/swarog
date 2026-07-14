use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use bus::{AnyTx, Bus};
use charactersapi::{Character, Ownership, Player};
use configapi::Config;
use opsapi::{Error, Identity};

use crate::{internal, Store, LIST_HARD_LIMIT};

pub(crate) const MAX_NAME_BYTES: usize = 128;
pub(crate) const MAX_CLASS_BYTES: usize = 64;

/// Default per-player character cap when `characters/max_per_player` is unset in
/// config. Clamped to [`LIST_HARD_LIMIT`] at read so `create` can never admit more
/// characters than `list` can return.
pub(crate) const MAX_PER_PLAYER: i64 = 10;

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

// ============================================================================
// Service — backs Ownership + Player (the registry capabilities + the generated
// edge faces + the gateway's in-process invokers) and the local admin render.
// ============================================================================

/// What other modules get from `require::<dyn Ownership>` / `require::<dyn Player>`.
/// Holds the store (for the domain writes) and the bus (for the atomic durable event append).
pub struct Service {
    pub(crate) store: Store,
    pub(crate) bus: Arc<Bus>,
    /// The mandatory `config` reader backing `create`'s per-player cap
    /// (`characters/max_per_player`); resolved in `init` (phase 2). This is a
    /// replica-local `CachedConfig`/`Service` kept fresh by the app-owned broadcast
    /// invalidation plane, so `create` reads it directly — no characters-owned second
    /// cache (which would only add staleness).
    pub(crate) config: OnceLock<Arc<dyn Config>>,
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
        // Clamp to [0, LIST_HARD_LIMIT]: the upper bound so `create` can never admit
        // more than `list` returns; the lower bound 0 (NOT 1) so `max_per_player = 0`
        // means "freeze creation" — the gate `n >= cap` rejects even the first create —
        // and a nonsensical negative also clamps to 0 (fail-CLOSED, never "allow 1").
        let cap = self
            .config
            .get()
            .expect("characters.init must resolve config before create")
            .get_int("characters", "max_per_player", MAX_PER_PLAYER)
            .clamp(0, LIST_HARD_LIMIT);
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
