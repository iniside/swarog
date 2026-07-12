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
//! producers already emit all six durably by their respective steps (characters today;
//! config Step 5; accounts Step 6; match Step 10 — `match.finished` now has a real
//! producer: the `match` module emit_tx's it atomic with the `match.matches` insert,
//! landing it in the shared event log, and audit's own pull subscription drains it
//! from its checkpoint — no relay, no HTTP hop).
//!
//! Retention is enforced by REACTING to `scheduler.fired{name:"audit-prune"}` on the
//! durable plane (Step 9 seeds the schedule). audit subscribes to `scheduler.fired`
//! raw (via the `schedulerevents::FIRED` descriptor's topic const — a sanctioned
//! CONTRACT import that removes the drift risk of a pinned literal, WITHOUT importing
//! the payload type: the handler still parses `name` out of the raw JSON), and prunes
//! in its own schema inside the handed tx.

pub mod conformance;

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use bus::{Delivery, Error as BusError, TxHandler};
use futures::future::BoxFuture;
use lifecycle::{Context, Module};
use opsapi::Error;
use sqlx::{PgConnection, PgPool};

/// The domain events audit records, each on the DURABLE plane via `on_tx_raw`. This is
/// a conscious edit point (the bus has no wildcard subscribe). The anti-drift test
/// (`tests.rs`) diffs this set against the producers' declared topics (including
/// `matchevents::FINISHED`, Step 10), so a rename on either side fails the build.
const DURABLE_TOPICS: &[&str] = &[
    "character.created",
    "character.deleted",
    "player.registered",
    "config.changed",
    "match.finished",
    "admin.action",
];

/// The per-topic subscription ids, zipped positionally with [`DURABLE_TOPICS`]:
/// six independent consumer-owned checkpoints replace the old single shared
/// `"audit"` subscriber name, so one topic's delivery failure never stalls
/// another's cursor. The anti-drift test pins the zip (equal length, id ↔ topic).
const DURABLE_SPEC_IDS: &[&str] = &[
    "audit.character-created.v1",
    "audit.character-deleted.v1",
    "audit.player-registered.v1",
    "audit.config-changed.v1",
    "audit.match-finished.v1",
    "audit.admin-action.v1",
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

/// The admin surface ids — shared by the contributed local `Item` and the
/// `admin.adminData` edge reply so a remote admin renders the same Section/Label.
const ADMIN_ITEM_ID: &str = "audit";
const ADMIN_SECTION: &str = "Platform";
const ADMIN_LABEL: &str = "Audit Log";

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

/// Folds any lower-level error into an `Internal` operation error (for the admin face).
fn internal<E: std::fmt::Display>(e: E) -> Error {
    Error::internal(e.to_string())
}

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

// ============================================================================
// Service — backs the admin face (reads the pool for the read-only "Audit Log" view).
// The durable handlers never touch it: they run on the transport-handed tx.
// ============================================================================

/// Holds the pool for the admin read-only view. Constructed in phase-1 `register`.
pub struct Service {
    pool: PgPool,
}

impl Service {
    /// The most recent 100 ledger entries as admin widgets (Go's `adminRender`): a
    /// read-only table of Topic / Payload (truncated to 80 chars, mono) / At.
    async fn admin_content(&self) -> anyhow::Result<adminapi::Content> {
        let rows: Vec<(String, String, String)> = sqlx::query_as(
            "SELECT topic, payload::text, at::text FROM audit.log ORDER BY at DESC, id DESC LIMIT 100",
        )
        .fetch_all(&self.pool)
        .await?;

        let mut table = adminapi::Table {
            columns: vec!["Topic".into(), "Payload".into(), "At".into()],
            rows: Vec::with_capacity(rows.len()),
        };
        for (topic, payload, at) in rows {
            table.rows.push(vec![
                adminapi::Cell::mono(&topic),
                adminapi::Cell::mono(truncate(&payload, 80)),
                adminapi::Cell::text(&at),
            ]);
        }
        Ok(adminapi::Content {
            kpis: Vec::new(),
            table: Some(table),
            form: None,
        })
    }
}

#[async_trait]
impl adminapi::AdminData for Service {
    /// The admin fan-out: this module's page as [`adminapi::ItemData`] (same
    /// Section/Label the local `Item` carries), served on the edge as `admin.adminData`
    /// so a remote admin process renders it cross-process.
    async fn admin_data(&self) -> Result<adminapi::ItemData, Error> {
        let content = self.admin_content().await.map_err(internal)?;
        Ok(adminapi::ItemData {
            id: ADMIN_ITEM_ID.into(),
            section: ADMIN_SECTION.into(),
            label: ADMIN_LABEL.into(),
            content,
        })
    }
}

/// Shortens `s` to at most `n` chars, appending an ellipsis when cut (rune-safe so a
/// multibyte payload never splits mid-character — Go's `truncate`).
fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_string();
    }
    let mut out: String = s.chars().take(n).collect();
    out.push('…');
    out
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

/// The audit module. Holds the pool-backed service (shared between the admin render and
/// the edge fan-out face). Edge exposure is topology-blind: `init` contributes the
/// `admin.adminData` face to `edge::EDGE_SLOT` unconditionally, and `app::run` installs
/// it iff this process serves an internal QUIC edge.
pub struct Audit {
    svc: OnceLock<Arc<Service>>,
}

impl Default for Audit {
    fn default() -> Self {
        Audit::new()
    }
}

impl Audit {
    pub fn new() -> Audit {
        Audit {
            svc: OnceLock::new(),
        }
    }

    fn svc(&self) -> Arc<Service> {
        self.svc
            .get()
            .expect("audit.register must run before init")
            .clone()
    }
}

#[async_trait]
impl Module for Audit {
    fn name(&self) -> &str {
        "audit"
    }

    /// Phase 1, BEFORE any `init`: builds the pool-backed service (needed by the admin
    /// face + render). No subscriptions here — those wire up in `init`.
    fn register(&self, ctx: &Context) -> anyhow::Result<()> {
        let pool = ctx
            .db()
            .ok_or_else(|| anyhow::anyhow!("audit requires a DB pool"))?
            .clone();
        self.svc
            .set(Arc::new(Service { pool }))
            .map_err(|_| anyhow::anyhow!("audit.register ran twice"))?;
        Ok(())
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
    /// the handed delivery tx), the `scheduler.fired` prune reaction, the local admin
    /// item, and the `admin.adminData` edge face (topology-blind; applied by `app::run`
    /// iff this process serves an internal edge).
    fn init(&self, ctx: &Context) -> anyhow::Result<()> {
        // Validated BEFORE touching `self.svc()` so a bad value fails startup loudly
        // (fail-closed at init — the apikeys/gateway explicit-gate convention; the old
        // admin `ADMIN_USER` bail this once mirrored is gone, replaced by DB-backed
        // session auth) without requiring `register`
        // to have run first — a non-positive retention would delete the ledger outright
        // on the next prune tick, so a typo must stop the process, not silently truncate
        // history. `env_int` already falls back to `DEFAULT_RETENTION_DAYS` when unset or
        // unparseable; only a *parseable* non-positive value reaches this bail.
        let retention_days = env_int("AUDIT_RETENTION_DAYS", DEFAULT_RETENTION_DAYS);
        if retention_days <= 0 {
            anyhow::bail!(
                "audit: AUDIT_RETENTION_DAYS must be > 0 (got {retention_days}) — a non-positive \
                 retention would delete the ledger; unset it for the default {DEFAULT_RETENTION_DAYS}"
            );
        }
        let svc = self.svc();

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

        // The local admin page. RenderFn is synchronous, but the store read is async;
        // the closure bridges via block_in_place (requires the multi-thread runtime the
        // app boots on) — the same pattern characters/inventory use.
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
                        tokio::runtime::Handle::current().block_on(svc.admin_content())
                    })
                }),
            ),
        );

        // Edge exposure, contributed UNCONDITIONALLY — topology-blind: `app::run`
        // applies this iff the entrypoint stood up an internal edge server (then a
        // remote admin pulls audit's page over QUIC); in the monolith it is never
        // applied. Registered through audit's OWN glue crate's re-export so no foreign
        // rpc is imported.
        ctx.contribute(
            edge::EDGE_SLOT,
            edge::EdgeReg::new(move |server| {
                auditrpc::register_admin(server, svc.clone());
            }),
        );
        Ok(())
    }
}

// ============================================================================
// Tests. The durable handlers are driven directly against a real sqlx tx (the same
// shape messaging's consume uses — an insert/prune inside a tx that then commits), so
// they exercise the ledger SQL + atomicity without the transport internals. The
// anti-drift topic-set test needs no DB. In-crate so they can drive the private
// handlers. Live-Postgres tests SKIP cleanly when the local DB is unreachable.
// ============================================================================
#[cfg(test)]
mod tests;
