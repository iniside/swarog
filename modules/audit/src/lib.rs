//! `audit` — an append-only ledger of domain events for GameOps visibility (port of
//! Go's `modules/audit`). It owns schema `audit` and touches no other module's tables.
//!
//! It listens to the bus GENERICALLY, by topic string — audit never imports a
//! domain's payload types, it just records the raw event JSON. The cost of that
//! decoupling is that [`DURABLE_TOPICS`] is a conscious, REQUIRED edit point when a new
//! event should be logged (the bus has no wildcard subscribe); generic-subscribe only
//! avoids importing the payload type (and its apidiff coupling), not the edit itself.
//!
//! ## One plane: durable (deliberate deviation from Go)
//! Go split `durableTopics` (its push-relay plane) from `bestEffortTopics` (plain in-process
//! bus), a distinction that assumed audit was CO-HOSTED with the producers. In the
//! fortress topology every producer lives in its own process, so a plain-`emit`
//! subscription would never cross the boundary and silently drop. Therefore ALL audited
//! topics are DURABLE here: audit subscribes with [`bus::Bus::on_tx_raw`] (untyped
//! durable), and the transport hands the raw JSON and runs the ledger insert inside its
//! per-`(event_id,"audit")` delivery tx (effect + checkpoint commit together) —
//! exactly-once in BOTH topologies. The
//! producers each emit durably by their respective steps (characters today; config
//! Step 5; accounts Step 6; match Step 10; wallet Step 3 of the wallet-module plan;
//! friends Step 3 of the friends-module plan; groups Step 5 of the groups-membership
//! plan) — each topic in [`DURABLE_TOPICS`] gets a real producer by its own step: the
//! owning module `emit_tx`'s it atomic with its own domain write,
//! landing it in the shared event log, and audit's own pull subscription drains it
//! from its checkpoint — no relay, no HTTP hop.
//!
//! Retention is enforced by REACTING to `scheduler.fired{name:"audit-prune"}` on the
//! durable plane (Step 9 seeds the schedule). audit subscribes to `scheduler.fired`
//! raw (via the `schedulerevents::FIRED` descriptor's topic const — a sanctioned
//! CONTRACT import that removes the drift risk of a pinned literal, WITHOUT importing
//! the payload type: the handler still parses `name` out of the raw JSON), and prunes
//! in its own schema inside the handed tx.


use std::sync::Arc;

use async_trait::async_trait;
use bus::{Delivery, Error as BusError, TxHandler};
use futures::future::BoxFuture;
use lifecycle::{Context, Module};
use sqlx::PgConnection;

/// The domain events audit records, each on the DURABLE plane via `on_tx_raw`. This is
/// a conscious edit point (the bus has no wildcard subscribe). The anti-drift test
/// (`tests.rs`) diffs this set against the producers' declared topics (including
/// `matchevents::FINISHED`, Step 10), so a rename on either side fails the build.
const DURABLE_TOPICS: &[&str] = &[
    "character.created",
    "character.deleted",
    "player.registered",
    "player.promoted",
    "player.deleted",
    "config.changed",
    "match.finished",
    "admin.action",
    "wallet.changed",
    "friend.requested",
    "friend.accepted",
    "friend.removed",
    "group.created",
    "group.member_joined",
    "group.member_left",
    "group.role_changed",
];

/// The per-topic subscription ids, zipped positionally with [`DURABLE_TOPICS`]:
/// one independent consumer-owned checkpoint per topic replaces the old single shared
/// `"audit"` subscriber name, so one topic's delivery failure never stalls
/// another's cursor. The anti-drift test pins the zip (equal length, id ↔ topic).
const DURABLE_SPEC_IDS: &[&str] = &[
    "audit.character-created.v1",
    "audit.character-deleted.v1",
    "audit.player-registered.v1",
    "audit.player-promoted.v1",
    "audit.player-deleted.v1",
    "audit.config-changed.v1",
    "audit.match-finished.v1",
    "audit.admin-action.v1",
    "audit.wallet-changed.v1",
    "audit.friend-requested.v1",
    "audit.friend-accepted.v1",
    "audit.friend-removed.v1",
    "audit.group-created.v1",
    "audit.group-member-joined.v1",
    "audit.group-member-left.v1",
    "audit.group-role-changed.v1",
];

/// The `scheduler.fired` `name` audit prunes on. Shared vocabulary (a string, like a
/// topic): the scheduler seeds this schedule name (Step 9), audit reacts to it.
const PRUNE_SCHEDULE_NAME: &str = schedulerevents::schedule_names::AUDIT_PRUNE;

/// The prune reaction's own checkpoint — independent of the ledger subscriptions.
const PRUNE_SUB: bus::SubscriptionSpec = bus::SubscriptionSpec {
    id: "audit.prune-on-scheduler.v1",
    start: bus::StartPosition::Genesis,
};

const DEFAULT_RETENTION_DAYS: i32 = 30;

/// Creates this module's OWN schema and nothing else — full logical isolation (#10).
/// Idempotent DDL only (`CREATE … IF NOT EXISTS`) — schema evolution is wipe +
/// fresh CREATE (`DROP SCHEMA audit CASCADE`, or a full DB wipe), never `ALTER`;
/// see CLAUDE.md's "wipe is the migration strategy".
const SCHEMA_DDL: &str = r#"
CREATE SCHEMA IF NOT EXISTS audit;
CREATE TABLE IF NOT EXISTS audit.log (
	id       bigserial   PRIMARY KEY,
	topic    text        NOT NULL,
	payload  jsonb       NOT NULL,
	at       timestamptz NOT NULL DEFAULT now(),
	event_id text
);
CREATE INDEX IF NOT EXISTS log_at_idx ON audit.log(at);"#;

// ============================================================================
// Durable handlers — invoked by messaging inside its per-(event_id,"audit")
// delivery tx, so the ledger effect commits atomically with the checkpoint.
// ============================================================================

/// Records one durable event to the ledger: the raw event JSON, verbatim, under its
/// topic, alongside the delivery's `event_id` — a durable cross-reference from the
/// ledger row back to the subscription's checkpoint. Effects are exactly-once because the write
/// commits atomically with the checkpoint on the delivery tx (downcast from
/// `Delivery` — audit shares the plane's Postgres engine). No payload-type import — the
/// untyped `on_tx_raw` hands the bytes (Go's `record`).
struct RecordHandler {
    topic: String,
}

impl TxHandler for RecordHandler {
    fn call<'a>(
        &'a self,
        mut delivery: Delivery<'a>,
        payload: Vec<u8>,
    ) -> BoxFuture<'a, Result<(), BusError>> {
        Box::pin(async move {
            // Bind event_id before the downcast consumes `delivery.tx` (a &str, cheap).
            let event_id = delivery.event_id;
            let conn = delivery.tx.downcast::<PgConnection>()?;
            // The bus JSON-encoded the payload, so it is valid UTF-8; bind as text so
            // `::jsonb` parses it (a bytea bind would cast raw bytes).
            let text = std::str::from_utf8(&payload).map_err(BusError::transport)?;
            sqlx::query(
                "INSERT INTO audit.log (topic, payload, event_id) VALUES ($1, $2::jsonb, $3)",
            )
            .bind(&self.topic)
            .bind(text)
            .bind(event_id)
            .execute(&mut *conn)
            .await
            .map_err(BusError::transport)?;
            Ok(())
        })
    }
}

/// Prunes ledger rows past the retention window as a REACTION to
/// `scheduler.fired{name:"audit-prune"}`, on the delivery tx (downcast from `Delivery`).
/// A non-prune schedule name is a committed no-op (the tick is marked processed, nothing
/// to do); a redelivered tick is idempotent (Go's `prune`).
struct PruneHandler {
    retention_days: i32,
}

/// Just the `name` field of a `scheduler.fired` payload — audit parses this out of the
/// raw JSON rather than importing `schedulerevents::Fired` into the handler (the
/// zero-coupling design: it subscribes by the descriptor's topic const but never
/// deserializes through the producer's payload type).
#[derive(serde::Deserialize)]
struct FiredName {
    name: String,
}

impl TxHandler for PruneHandler {
    fn call<'a>(
        &'a self,
        mut delivery: Delivery<'a>,
        payload: Vec<u8>,
    ) -> BoxFuture<'a, Result<(), BusError>> {
        Box::pin(async move {
            let conn = delivery.tx.downcast::<PgConnection>()?;
            let fired: FiredName = serde_json::from_slice(&payload).map_err(BusError::from)?;
            if fired.name != PRUNE_SCHEDULE_NAME {
                return Ok(()); // some other schedule — marked processed, nothing to do
            }
            // Belt-and-braces: `init` already bails on a non-positive configured value,
            // so a `PruneHandler` should never be constructed with one — this is a
            // debug-only tripwire against a future construction site that skips `init`.
            debug_assert!(
                self.retention_days > 0,
                "PruneHandler constructed with non-positive retention_days: {}",
                self.retention_days
            );
            sqlx::query("DELETE FROM audit.log WHERE at < now() - make_interval(days => $1)")
                .bind(self.retention_days)
                .execute(&mut *conn)
                .await
                .map_err(BusError::transport)?;
            Ok(())
        })
    }
}

/// Reads `key` as an `i32`, returning `def` when unset or unparseable (Go's `envInt`).
fn env_int(key: &str, def: i32) -> i32 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.trim().parse::<i32>().ok())
        .unwrap_or(def)
}

// ============================================================================
// Module — the lifecycle wiring.
// ============================================================================

/// The audit module.
pub struct Audit;

impl Default for Audit {
    fn default() -> Self {
        Audit::new()
    }
}

impl Audit {
    pub fn new() -> Audit {
        Audit
    }
}

#[async_trait]
impl Module for Audit {
    fn name(&self) -> &str {
        "audit"
    }

    /// Creates this module's own schema. Idempotent.
    async fn migrate(&self, ctx: &Context) -> anyhow::Result<()> {
        let pool = ctx
            .db()
            .ok_or_else(|| anyhow::anyhow!("audit requires a DB pool"))?;
        sqlx::raw_sql(SCHEMA_DDL).execute(pool).await?;
        Ok(())
    }

    /// Only wires up — no I/O (#8). Subscribes each durable topic (raw JSON, inserted on
    /// the handed delivery tx) and the `scheduler.fired` prune reaction.
    fn init(&self, ctx: &Context) -> anyhow::Result<()> {
        // Validated FIRST so a bad value fails startup loudly (fail-closed at init — the
        // apikeys/gateway explicit-gate convention) — a non-positive retention would delete
        // the ledger outright on the next prune tick, so a typo must stop the process, not
        // silently truncate history. `env_int` already falls back to
        // `DEFAULT_RETENTION_DAYS` when unset or unparseable; only a *parseable*
        // non-positive value reaches this bail.
        let retention_days = env_int("AUDIT_RETENTION_DAYS", DEFAULT_RETENTION_DAYS);
        if retention_days <= 0 {
            anyhow::bail!(
                "audit: AUDIT_RETENTION_DAYS must be > 0 (got {retention_days}) — a non-positive \
                 retention would delete the ledger; unset it for the default {DEFAULT_RETENTION_DAYS}"
            );
        }
        // Durable plane: the producer emitted via emit_tx; messaging delivers here
        // through its per-(event_id,"audit") delivery tx, in BOTH topologies. We
        // subscribe by raw string (no payload-type import) and insert the raw JSON on
        // the HANDED tx, so the ledger row commits atomically with the checkpoint.
        for (topic, id) in DURABLE_TOPICS.iter().zip(DURABLE_SPEC_IDS) {
            let handler: Arc<dyn TxHandler> = Arc::new(RecordHandler {
                topic: (*topic).to_string(),
            });
            ctx.bus().on_tx_raw(
                bus::SubscriptionSpec {
                    id,
                    start: bus::StartPosition::Genesis,
                },
                topic,
                handler,
            );
        }

        // Retention prune as a REACTION to scheduler.fired on the durable plane. Raw
        // subscribe by the CONTRACT descriptor's topic const (no payload-type import):
        // the handler parses `name` and prunes only for "audit-prune", inside the handed
        // delivery tx.
        let prune: Arc<dyn TxHandler> = Arc::new(PruneHandler { retention_days });
        ctx.bus()
            .on_tx_raw(PRUNE_SUB, schedulerevents::FIRED.topic(), prune);
        Ok(())
    }
}

// ============================================================================
// Tests. The durable handlers are driven directly against a real sqlx tx (the same
// shape messaging's consume uses — an insert/prune inside a tx that then commits), so
// they exercise the ledger SQL + atomicity without the transport internals. The
// anti-drift topic-set test needs no DB. In-crate so they can drive the private
// handlers. The live-Postgres tests take their pool from `testdb`, which FAILS the run
// when the local DB is unreachable.
// ============================================================================
#[cfg(test)]
mod tests;
