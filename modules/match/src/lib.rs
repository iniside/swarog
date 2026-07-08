//! `match_module` — records match results and announces them (port of Go's
//! `modules/match`, with the fortress + durable deltas the plan mandates).
//!
//! Go kept `match` monolith-only and persistence-free: `Report` read the winner's MMR
//! for a log line and fire-and-forget `bus.Emit`'d `match.finished` on the plain
//! in-process bus. Under the fortress topology the reactors (`rating`, `leaderboard`,
//! `audit`) each live in their OWN process, so a plain `emit` would never cross the
//! boundary. Therefore match now:
//!   - **owns schema `match`** (`match.matches` — recording a result is the natural
//!     domain write), and
//!   - in `report`, **INSERTs the match row and `emit_tx`s `match.finished` in ONE tx**
//!     — the durable rule's required tx source: the event is durable iff the match is.
//!
//! The MMR read stays synchronous and material-free (Go read the winner's MMR only, for
//! a log line): match resolves `dyn MmrReader` from the registry — the local `rating`
//! module (monolith) or a remote `ratingrpc` stub (split) — and logs both players'
//! ratings before recording. It never blocks on the reactors: whoever cares about a
//! finished match subscribes durably; match never learns who.

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use bus::Bus;
use lifecycle::{Caps, Context, Module};
use matchapi::Match;
use opsapi::Error;
use ratingapi::MmrReader;
use registry::key;
use sqlx::{PgConnection, PgPool};

/// Creates this module's OWN schema and nothing else — full logical isolation (#10).
/// Idempotent. The match row is the durable-rule tx source (`report` INSERTs it and
/// `emit_tx`s `match.finished` on the same tx). `winner`/`loser` are plain player id
/// refs — no cross-module foreign key.
const SCHEMA_DDL: &str = r#"
CREATE SCHEMA IF NOT EXISTS match;
CREATE TABLE IF NOT EXISTS match.matches (
	id     uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
	winner text        NOT NULL,
	loser  text        NOT NULL,
	at     timestamptz NOT NULL DEFAULT now()
);"#;

/// Folds any lower-level error into an `Internal` operation error.
fn internal<E: std::fmt::Display>(e: E) -> Error {
    Error::internal(e.to_string())
}

// ============================================================================
// Service — backs the Match capability (the gateway's in-process invoker + the
// generated edge face). Holds the store (the domain write), the bus (the atomic
// outbox emit), and the resolved MmrReader (the sync pre-emit read).
// ============================================================================

pub struct Service {
    pool: PgPool,
    bus: Arc<Bus>,
    /// The rating capability read before recording a match. Resolved in `init` (phase
    /// 2) — the local `rating` module or a remote stub Provided it in phase 1.
    rating: OnceLock<Arc<dyn MmrReader>>,
}

impl Service {
    fn rating(&self) -> &Arc<dyn MmrReader> {
        self.rating
            .get()
            .expect("match.init must resolve the rating MmrReader before report")
    }

    /// Inserts a match row on the given connection (a tx, so the row + its outbox row
    /// commit together) and returns the generated `id`.
    async fn insert_tx(
        &self,
        conn: &mut PgConnection,
        winner: &str,
        loser: &str,
    ) -> Result<String, sqlx::Error> {
        let (id,): (String,) = sqlx::query_as(
            "INSERT INTO match.matches (winner, loser) VALUES ($1, $2) RETURNING id::text",
        )
        .bind(winner)
        .bind(loser)
        .fetch_one(&mut *conn)
        .await?;
        Ok(id)
    }
}

#[async_trait]
impl Match for Service {
    /// Records that `winner` beat `loser`. The MMR read is SYNCHRONOUS (query rating
    /// right now, for the log line — Go read the winner's MMR; we read both to exercise
    /// the wire, doing nothing material with the values). Then the domain INSERT + the
    /// `match.finished` outbox row commit in ONE tx: the event is durable iff the match
    /// is. A rating transport failure surfaces as an error (the sync dep is required).
    async fn report(&self, winner: String, loser: String) -> Result<(), Error> {
        let winner_mmr = self.rating().mmr(winner.clone()).await?;
        let loser_mmr = self.rating().mmr(loser.clone()).await?;
        tracing::info!(%winner, winner_mmr, %loser, loser_mmr, "match reported");

        let mut tx = self.pool.begin().await.map_err(internal)?;
        let match_id = self.insert_tx(&mut tx, &winner, &loser).await.map_err(internal)?;
        let evt = matchevents::Finished {
            match_id,
            winner,
            loser,
        };
        self.bus
            .emit_tx(&mut tx, &matchevents::FINISHED, &evt)
            .await
            .map_err(internal)?;
        tx.commit().await.map_err(internal)?;
        Ok(())
    }
}

// ============================================================================
// Module — the lifecycle wiring.
// ============================================================================

/// The match module. Holds the constructed service (shared between `register`, the
/// operations, and the edge face). Edge exposure is topology-blind: `init` contributes
/// the generated RPC face to `edge::EDGE_SLOT` unconditionally, and `app::run` installs
/// it iff this process serves an internal QUIC edge.
pub struct MatchModule {
    svc: OnceLock<Arc<Service>>,
}

impl Default for MatchModule {
    fn default() -> Self {
        MatchModule::new()
    }
}

impl MatchModule {
    pub fn new() -> MatchModule {
        MatchModule {
            svc: OnceLock::new(),
        }
    }

    fn svc(&self) -> Arc<Service> {
        self.svc
            .get()
            .expect("match.register must run before init")
            .clone()
    }
}

#[async_trait]
impl Module for MatchModule {
    fn name(&self) -> &str {
        "match"
    }

    /// `rating` is a SYNC dependency (the pre-emit MMR read); `messaging` provides the
    /// durable transport for `emit_tx`. Both are resolved downward (registry / bus).
    fn requires(&self) -> Vec<String> {
        vec!["rating".to_string(), "messaging".to_string()]
    }

    fn caps(&self) -> Caps {
        Caps::REGISTER | Caps::MIGRATE
    }

    /// Phase 1, BEFORE any `init`: builds the store-backed service (from `ctx.db()` +
    /// `ctx.bus()`). The rating dep is injected later in `init` (phase 2) — the local
    /// `rating` module or a remote stub has Provided it by then.
    fn register(&self, ctx: &Context) -> anyhow::Result<()> {
        let pool = ctx
            .db()
            .ok_or_else(|| anyhow::anyhow!("match requires a DB pool"))?
            .clone();
        self.svc
            .set(Arc::new(Service {
                pool,
                bus: ctx.bus().clone(),
                rating: OnceLock::new(),
            }))
            .map_err(|_| anyhow::anyhow!("match.register ran twice"))?;
        Ok(())
    }

    /// Creates this module's own schema. Idempotent.
    async fn migrate(&self, ctx: &Context) -> anyhow::Result<()> {
        let pool = ctx
            .db()
            .ok_or_else(|| anyhow::anyhow!("match requires a DB pool"))?;
        sqlx::raw_sql(SCHEMA_DDL).execute(pool).await?;
        Ok(())
    }

    /// Only wires up — no I/O (#8). Resolves the `rating` MmrReader (the registry
    /// resolves it to the real service in the monolith or the edge client in a split),
    /// contributes the single `report` operation to the opsapi slots (so a co-hosted
    /// gateway fronts `POST /match/report`), and contributes the generated edge face so
    /// a front door can dispatch `match.report` here over QUIC when this process serves
    /// an internal edge.
    fn init(&self, ctx: &Context) -> anyhow::Result<()> {
        let svc = self.svc();

        // The rating capability, resolved to the real service (monolith) or the
        // generated edge client (split), and injected into the service Provided in
        // register.
        let rating = ctx.registry().require::<dyn MmrReader>(&key("rating", "mmr_reader"));
        let _ = svc.rating.set(rating);

        // The single public op (report). The generated `operations()` yields one OpSet;
        // contribute each half to its slot (LocalBackend + the future RemoteBackend
        // consume the SAME wire envelopes).
        for op in matchapi::match_rpc::operations(svc.clone()) {
            ctx.contribute(opsapi::SLOT, op.operation);
            ctx.contribute(opsapi::BINDING_SLOT, op.binding);
            ctx.contribute(opsapi::LOCAL_SLOT, op.local);
        }

        // Edge exposure, contributed UNCONDITIONALLY — topology-blind: `app::run`
        // applies this iff the entrypoint stood up an internal edge server (then a
        // front gateway dispatches `match.report` here over QUIC); in the monolith it is
        // never applied. Own glue (sanctioned): the generated register_server face lives
        // in `matchrpc`.
        ctx.contribute(
            edge::EDGE_SLOT,
            edge::EdgeReg::new(move |server| {
                matchrpc::match_rpc::register_server(server, svc.clone());
            }),
        );
        Ok(())
    }
}

// ============================================================================
// Tests. Unit tests need no DB; integration tests target the local Postgres (the test
// DB) and SKIP cleanly when it is unreachable. In-crate so they can drive the private
// `Service` directly.
// ============================================================================
#[cfg(test)]
mod tests;
