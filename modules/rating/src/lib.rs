//! `rating` — a Postgres-backed matchmaking-rating (MMR) projection. It PROVIDES the
//! wire-only `MmrReader` capability (a sync read of a player's MMR, resolved by `match`
//! over the registry / QUIC edge) and REACTS to `match.finished` on the durable plane
//! (+15 winner / -15 loser). `match` has zero knowledge that `rating` exists — it
//! publishes durably, rating subscribes.
//!
//! ## Persistent projection (durable, restart-safe)
//! Ratings live in schema `rating`, table `rating.ratings(player, mmr)`, defaulting to
//! 1000 for an unseen player. The `match.finished` handler upserts BOTH players inside
//! the handed delivery transaction, so the effect commits atomically with the
//! subscription checkpoint: a rating-svc restart resumes from the checkpoint over a
//! projection that already reflects every delivered event. Under the durable plane an
//! in-memory effect would be dishonest — the checkpoint would advance past events whose
//! effect the restart erases.

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use lifecycle::{Context, Module};
use opsapi::Error;
use ratingapi::MmrReader;
use registry::key;
use sqlx::{PgConnection, PgPool};

/// A player's starting rating when never seen before.
const DEFAULT_MMR: i32 = 1000;
/// The delta applied to each player on a finished match (win / loss).
const WIN_DELTA: i32 = 15;
const LOSS_DELTA: i32 = 15;

/// Creates this module's OWN schema and nothing else — full logical isolation (#10).
/// Idempotent. `player` is a plain ref to a player id carried by the event; NO
/// cross-module FK.
const SCHEMA_DDL: &str = r#"
CREATE SCHEMA IF NOT EXISTS rating;
CREATE TABLE IF NOT EXISTS rating.ratings (
	player text    PRIMARY KEY,
	mmr    integer NOT NULL
);"#;

/// The consumer-owned durable subscription for rating's `match.finished`
/// reaction — the stable checkpoint id (renaming it abandons the checkpoint).
const MATCH_FINISHED_SUB: bus::SubscriptionSpec = bus::SubscriptionSpec {
    id: "rating.match-finished.v1",
    start: bus::StartPosition::Genesis,
};

/// Folds any lower-level error into an `Internal` operation error.
fn internal<E: std::fmt::Display>(e: E) -> Error {
    Error::internal(e.to_string())
}

/// Applies a finished-match result on the handed connection (the delivery transaction, so
/// the projection + the subscription checkpoint commit together): winner +15, loser -15,
/// each from the 1000 default when unseen. The single mutation path.
async fn apply_result(
    conn: &mut PgConnection,
    winner: &str,
    loser: &str,
) -> Result<(), sqlx::Error> {
    upsert_delta(conn, winner, WIN_DELTA).await?;
    upsert_delta(conn, loser, -LOSS_DELTA).await?;
    Ok(())
}

/// Upserts one player's rating: INSERT `DEFAULT_MMR + delta` for an unseen player,
/// otherwise ADD `delta` to the existing value. The initial and the increment carry the
/// same delta, so a first-seen player lands exactly `1000 + delta` and a returning player
/// keeps accumulating from their live value.
async fn upsert_delta(conn: &mut PgConnection, player: &str, delta: i32) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO rating.ratings (player, mmr) VALUES ($1, $2) \
         ON CONFLICT (player) DO UPDATE SET mmr = rating.ratings.mmr + $3",
    )
    .bind(player)
    .bind(DEFAULT_MMR + delta)
    .bind(delta)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

// ============================================================================
// Service — the Postgres-backed MMR projection. Backs the `MmrReader` capability (registry
// + edge face) and the `match.finished` reaction. Holds the pool for the sync read.
// ============================================================================

pub struct Service {
    pool: PgPool,
}

#[async_trait]
impl MmrReader for Service {
    /// Reads a player's MMR from the projection. An unseen player is the 1000 default,
    /// never an error — an `Err` is an infrastructure failure, not an absent row.
    async fn mmr(&self, player_id: String) -> Result<i64, Error> {
        let row: Option<(i32,)> = sqlx::query_as("SELECT mmr FROM rating.ratings WHERE player = $1")
            .bind(&player_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(internal)?;
        Ok(row.map(|(m,)| m as i64).unwrap_or(DEFAULT_MMR as i64))
    }
}

// ============================================================================
// Module — the lifecycle wiring.
// ============================================================================

/// The rating module. Holds the pool-backed service (shared between `register`, the
/// `match.finished` reaction, and the edge face). Edge exposure is topology-blind:
/// `init` contributes the generated RPC face to `edge::EDGE_SLOT` unconditionally, and
/// `app::run` installs it iff this process serves an internal QUIC edge.
pub struct Rating {
    svc: OnceLock<Arc<Service>>,
}

impl Default for Rating {
    fn default() -> Self {
        Rating::new()
    }
}

impl Rating {
    pub fn new() -> Rating {
        Rating {
            svc: OnceLock::new(),
        }
    }

    fn svc(&self) -> Arc<Service> {
        self.svc
            .get()
            .expect("rating.register must run before init")
            .clone()
    }
}

#[async_trait]
impl Module for Rating {
    fn name(&self) -> &str {
        "rating"
    }

    /// `REGISTER` (it Provides `MmrReader` in phase 1) + `MIGRATE` (it owns schema
    /// `rating`); `init` wires the subscription + edge face.
    /// Phase 1, BEFORE any `init`: builds the pool-backed service and offers it under the
    /// canonical `rating.mmr_reader` key, so `match`'s `require::<dyn MmrReader>`
    /// resolves regardless of registration order.
    fn register(&self, ctx: &Context) -> anyhow::Result<()> {
        let pool = ctx
            .db()
            .ok_or_else(|| anyhow::anyhow!("rating requires a DB pool"))?
            .clone();
        let svc = Arc::new(Service { pool });
        self.svc
            .set(svc.clone())
            .map_err(|_| anyhow::anyhow!("rating.register ran twice"))?;
        ctx.registry()
            .provide::<dyn MmrReader>(key("rating", "mmr_reader"), svc);
        Ok(())
    }

    /// Creates this module's own schema. Idempotent.
    async fn migrate(&self, ctx: &Context) -> anyhow::Result<()> {
        let pool = ctx
            .db()
            .ok_or_else(|| anyhow::anyhow!("rating requires a DB pool"))?;
        sqlx::raw_sql(SCHEMA_DDL).execute(pool).await?;
        Ok(())
    }

    /// Only wires up — no I/O (#8). Subscribes `match.finished` on the DURABLE plane
    /// (`on_tx`): the transport runs the handler inside the delivery transaction, and the
    /// handler upserts BOTH players on the handed connection (+15/-15) — so the projection
    /// and the subscription checkpoint commit atomically. Also contributes the generated
    /// `MmrReader` edge face so a peer's `match` resolves `rating.mmr` over QUIC when this
    /// process serves an internal edge.
    fn init(&self, ctx: &Context) -> anyhow::Result<()> {
        let svc = self.svc();

        ctx.bus().on_tx(
            MATCH_FINISHED_SUB,
            &matchevents::FINISHED,
            move |mut delivery, e: matchevents::Finished| {
                Box::pin(async move {
                    let conn = delivery.tx.downcast::<sqlx::PgConnection>()?;
                    apply_result(conn, &e.winner, &e.loser)
                        .await
                        .map_err(bus::Error::transport)
                })
            },
        );

        // Edge exposure, contributed UNCONDITIONALLY — topology-blind: `app::run`
        // applies this iff the entrypoint stood up an internal edge server (then a
        // peer's `match` resolves `rating.mmr` over QUIC); in the monolith it is never
        // applied. Own glue (sanctioned): the generated register_server face lives in
        // `ratingrpc`.
        ctx.contribute(
            edge::EDGE_SLOT,
            edge::EdgeReg::new(move |server| {
                ratingrpc::mmr_reader_rpc::register_server(server, svc.clone());
            }),
        );
        Ok(())
    }
}

// ============================================================================
// Tests. Live-Postgres: the durable upsert is driven directly against a real sqlx tx (the
// same shape the asyncevents plane's delivery runs the handler in), the MMR read against
// the pool. In-crate so they drive the private `Service` + `apply_result` directly.
// ============================================================================
#[cfg(test)]
mod tests;
