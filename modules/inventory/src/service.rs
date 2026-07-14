use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use charactersapi::Ownership;
use configapi::Config;
use inventoryapi::{Holding, Holdings};
use opsapi::{Error, Identity};

use crate::{internal, is_holdings_cap_violation, validate_quantity, Owner, Store, MAX_HOLDING_QTY};

// ============================================================================
// Inner — the shared service state. Backs the `Holdings` capability (registry +
// generated edge face + gateway invokers), the two durable event effects
// (grant-starter/wipe), and the local admin render. One `Arc<Inner>` is handed to
// every path so they share the same store, ownership dep, and materialized starter.
// ============================================================================

pub struct Inner {
    pub(crate) store: Store,
    /// Whether the simulated-IAP grant is enabled (`INVENTORY_DEV_GRANT`, resolved in
    /// `register`). Gates `grant` at the SERVICE level — the single authority every
    /// exposure path traverses (gateway HTTP route, player-QUIC allow-list, raw mTLS
    /// edge face), so the trust model cannot diverge between the monolith and the
    /// split. The op itself is contributed UNCONDITIONALLY (the monolith slot set and
    /// the split route set stay structurally equal); with the gate off the impl
    /// answers NotFound (→ 404) to a fully-authed caller.
    pub(crate) dev_grant: bool,
    /// The `characters` ownership capability backing `list_character`'s authz.
    /// Resolved in `init` (phase 2) — the service is Provided in `register` (phase 1)
    /// BEFORE `require` can run, exactly as Go sets `m.svc.characters` in Init.
    pub(crate) ownership: OnceLock<Arc<dyn Ownership>>,
    /// The mandatory `config` reader; resolved in `init` (a hard dependency). Read
    /// directly on every grant — no inventory-owned second cache: this reader is a
    /// replica-local `CachedConfig`/`Service` kept fresh by the app-owned broadcast
    /// invalidation plane, so a second cache here would only add staleness risk.
    pub(crate) cfg: OnceLock<Arc<dyn Config>>,
}

impl Inner {
    fn ownership(&self) -> &Arc<dyn Ownership> {
        self.ownership
            .get()
            .expect("inventory.init must resolve characters ownership before use")
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
    /// non-positive or out-of-range qty, or an unknown item, is Invalid (→ 400).
    /// Returns the updated holdings, matching the old handler's respond-with-list
    /// behaviour.
    ///
    /// NOT a reference-grade mutation pattern — DEV-ONLY (gated by
    /// `INVENTORY_DEV_GRANT`, simulated IAP) and deliberately unhardened. It runs
    /// three SEPARATE pool-connection autocommits (`item_exists` → `grant_pool` →
    /// `list`; see `store.rs:107-110,162-165`), carries NO idempotency key, and
    /// `grant_pool` accumulates (`ON CONFLICT ... quantity = quantity +
    /// EXCLUDED.quantity`, `store.rs:93-94`) — so if the final `list` read fails
    /// AFTER `grant_pool` already committed, the caller sees an error despite the
    /// mutation having applied, and a manual retry double-grants. The starter-grant
    /// path (`Inner::grant_starter` in `projection.rs`) is the reference pattern:
    /// one handed delivery tx, exactly-once via the durable subscription.
    async fn grant(&self, identity: Identity, item_id: String, qty: i64) -> Result<Vec<Holding>, Error> {
        // The dev-grant gate, checked FIRST (before any input handling or DB touch):
        // the op is contributed/served unconditionally in both topologies, so this
        // impl-side guard is the single fail-closed authority.
        if !self.dev_grant {
            return Err(Error::not_found("grant is not enabled"));
        }
        let pid = identity
            .player_id()
            .ok_or_else(|| Error::invalid("missing player identity"))?
            .to_string();
        // Posture B (HTTP IAP): no durable checkpoint rides on the result, so an
        // out-of-range qty is REJECTED to the caller as Invalid (400) — the client
        // gets actionable feedback (previously a huge qty 500'd on the int4 overflow).
        // Contrast posture A in grant_starter, which degrades to a default to protect
        // the durable subscription.
        let qty = validate_quantity(qty)
            .map_err(|_| Error::invalid(format!("qty must be between 1 and {MAX_HOLDING_QTY}")))?;
        if !self.store.item_exists(&item_id).await.map_err(internal)? {
            return Err(Error::invalid("unknown item"));
        }
        let owner = Owner::player(pid);
        // The accumulated-state ceiling: a per-grant-LEGAL qty can still push the
        // stored `ON CONFLICT` sum past the DB CHECK's 2_000_000 (repeated IAP
        // grants) — SQLSTATE 23514 here is a definitive answer about durable state,
        // not an infrastructure fault, so it maps to Conflict (409: "the request
        // conflicts with existing durable state" — opsapi::Status docs; Invalid/400
        // would mislabel a well-formed request as malformed). HTTP-path only: the
        // durable grant_starter path CANNOT reach this CHECK — each character gets
        // exactly one starter grant (exactly-once + the tombstone guard), so its
        // stored quantity never accumulates toward the ceiling.
        self.store.grant_pool(&owner, &item_id, qty).await.map_err(|e| {
            if is_holdings_cap_violation(&e) {
                Error::conflict("holding cap reached for this item")
            } else {
                internal(e)
            }
        })?;
        self.store.list(&owner).await.map_err(internal)
    }
}
