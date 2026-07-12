//! `leaderboard` — a Postgres-backed win tally (port of Go's `modules/leaderboard`,
//! with the durable delta the plan mandates). It owns schema `leaderboard` and nothing
//! else — full logical isolation (#10): no other module's tables, no cross-module
//! foreign keys. The link to players is the bare player id carried by `match.finished`.
//!
//! Go subscribed with a plain in-process `On` and framed the tally as "best-effort".
//! Under the fortress topology the producer (`match`) lives in its OWN process, so a
//! plain-`emit` subscription would never cross the boundary. The plan retires the
//! best-effort framing with the durable rule: leaderboard subscribes with **`on_tx`**
//! and runs its `wins+1` upsert INSIDE the handed per-`(event_id,"leaderboard")`
//! delivery tx — exactly-once in BOTH topologies. The top-100 read is served as a
//! public `#[http]` op (`GET /leaderboard`, unchanged external shape).

pub mod conformance;

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use leaderboardapi::{Leaderboard, Score};
use lifecycle::{Context, Module};
use opsapi::Error;
use sqlx::{PgConnection, PgPool};

/// Creates this module's OWN schema and nothing else — full logical isolation (#10).
/// Idempotent. `player` is a plain ref to the winner id carried by the event; NO
/// cross-module FK.
const SCHEMA_DDL: &str = r#"
CREATE SCHEMA IF NOT EXISTS leaderboard;
CREATE TABLE IF NOT EXISTS leaderboard.scores (
	player text   PRIMARY KEY,
	wins   bigint NOT NULL DEFAULT 0
);"#;

/// The consumer-owned durable subscription for leaderboard's `match.finished`
/// reaction — the stable checkpoint id (renaming it abandons the checkpoint).
const MATCH_FINISHED_SUB: bus::SubscriptionSpec = bus::SubscriptionSpec {
    id: "leaderboard.match-finished.v1",
    start: bus::StartPosition::Genesis,
};

/// Folds any lower-level error into an `Internal` operation error.
fn internal<E: std::fmt::Display>(e: E) -> Error {
    Error::internal(e.to_string())
}

/// Records one win for `player` on the given connection (the messaging delivery tx,
/// so the tally + the checkpoint commit together). ON CONFLICT ADDS to the existing tally
/// — the exact Go upsert.
async fn record_win(conn: &mut PgConnection, player: &str) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO leaderboard.scores (player, wins) VALUES ($1, 1) \
         ON CONFLICT (player) DO UPDATE SET wins = leaderboard.scores.wins + 1",
    )
    .bind(player)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

// ============================================================================
// Service — backs the Leaderboard capability (the gateway's in-process invoker + the
// generated edge face). Holds the pool for the top-scores read.
// ============================================================================

pub struct Service {
    pool: PgPool,
}

#[async_trait]
impl Leaderboard for Service {
    /// The top-ranked players (wins desc, player asc), capped at 100 — the same query
    /// and shape as the pre-migration handler.
    async fn top_scores(&self) -> Result<Vec<Score>, Error> {
        let rows: Vec<(String, i64)> = sqlx::query_as(
            "SELECT player, wins FROM leaderboard.scores ORDER BY wins DESC, player ASC LIMIT 100",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(internal)?;
        Ok(rows
            .into_iter()
            .map(|(player, wins)| Score { player, wins })
            .collect())
    }
}

// ============================================================================
// Module — the lifecycle wiring.
// ============================================================================

/// The leaderboard module. Holds the pool-backed service (shared between the top-scores
/// op and the edge face). Edge exposure is topology-blind: `init` contributes the
/// generated RPC face to `edge::EDGE_SLOT` unconditionally, and `app::run` installs it
/// iff this process serves an internal QUIC edge.
pub struct LeaderboardModule {
    svc: OnceLock<Arc<Service>>,
}

impl Default for LeaderboardModule {
    fn default() -> Self {
        LeaderboardModule::new()
    }
}

impl LeaderboardModule {
    pub fn new() -> LeaderboardModule {
        LeaderboardModule {
            svc: OnceLock::new(),
        }
    }

    fn svc(&self) -> Arc<Service> {
        self.svc
            .get()
            .expect("leaderboard.register must run before init")
            .clone()
    }
}

#[async_trait]
impl Module for LeaderboardModule {
    fn name(&self) -> &str {
        "leaderboard"
    }

    /// Phase 1, BEFORE any `init`: builds the pool-backed service (needed by the
    /// top-scores op). No subscriptions here — those wire up in `init`.
    fn register(&self, ctx: &Context) -> anyhow::Result<()> {
        let pool = ctx
            .db()
            .ok_or_else(|| anyhow::anyhow!("leaderboard requires a DB pool"))?
            .clone();
        self.svc
            .set(Arc::new(Service { pool }))
            .map_err(|_| anyhow::anyhow!("leaderboard.register ran twice"))?;
        Ok(())
    }

    /// Creates this module's own schema. Idempotent.
    async fn migrate(&self, ctx: &Context) -> anyhow::Result<()> {
        let pool = ctx
            .db()
            .ok_or_else(|| anyhow::anyhow!("leaderboard requires a DB pool"))?;
        sqlx::raw_sql(SCHEMA_DDL).execute(pool).await?;
        Ok(())
    }

    /// Only wires up — no I/O (#8). Subscribes `match.finished` on the DURABLE plane
    /// (`on_tx`): the transport runs the `wins+1` upsert inside a per-`(event_id,
    /// "leaderboard")` delivery tx (exactly-once, atomic with the checkpoint commit).
    /// Contributes the `top_scores` op (so a co-hosted gateway fronts `GET /leaderboard`)
    /// and the generated edge face (so a front door dispatches `leaderboard.topScores`
    /// here over QUIC when this process serves an internal edge).
    fn init(&self, ctx: &Context) -> anyhow::Result<()> {
        let svc = self.svc();

        ctx.bus().on_tx(
            MATCH_FINISHED_SUB,
            &matchevents::FINISHED,
            move |mut delivery, e: matchevents::Finished| {
                Box::pin(async move {
                    let conn = delivery.tx.downcast::<sqlx::PgConnection>()?;
                    record_win(conn, &e.winner).await.map_err(bus::Error::transport)
                })
            },
        );

        for op in leaderboardapi::leaderboard_rpc::operations(svc.clone()) {
            ctx.contribute(opsapi::SLOT, op.operation);
            ctx.contribute(opsapi::BINDING_SLOT, op.binding);
            ctx.contribute(opsapi::LOCAL_SLOT, op.local);
        }

        ctx.contribute(
            edge::EDGE_SLOT,
            edge::EdgeReg::new(move |server| {
                leaderboardrpc::leaderboard_rpc::register_server(server, svc.clone());
            }),
        );
        Ok(())
    }
}

// ============================================================================
// Tests. The upsert is driven directly against a real sqlx tx (the same shape
// messaging's consume uses); the top-scores read against the pool. Live-Postgres tests
// SKIP cleanly when the local DB is unreachable. In-crate so they can drive the private
// `Service` + `record_win` directly.
// ============================================================================
#[cfg(test)]
mod tests;
