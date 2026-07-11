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
use bus::{AnyTx, Bus};
use lifecycle::{Context, Module};
use matchapi::Match;
use opsapi::Error;
use ratingapi::MmrReader;
use registry::key;
use sqlx::{PgConnection, PgPool};

/// Creates this module's OWN schema and nothing else — full logical isolation (#10).
/// Idempotent. The match row is the durable-rule tx source (`report` INSERTs it and
/// `emit_tx`s `match.finished` on the same tx). `winner`/`loser` are plain player id
/// refs — no cross-module foreign key. `report_id` is the client-supplied idempotency
/// key: `UNIQUE (report_id)` is what makes a stub-retried report a no-op instead of a
/// second match (+ second `match.finished`).
const SCHEMA_DDL: &str = r#"
CREATE SCHEMA IF NOT EXISTS match;
CREATE TABLE IF NOT EXISTS match.matches (
	id        uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
	report_id text        NOT NULL,
	winner    text        NOT NULL,
	loser     text        NOT NULL,
	at        timestamptz NOT NULL DEFAULT now(),
	UNIQUE (report_id)
);"#;

/// Folds any lower-level error into an `Internal` operation error.
fn internal<E: std::fmt::Display>(e: E) -> Error {
    Error::internal(e.to_string())
}

// ============================================================================
// Service — backs the Match capability (the gateway's in-process invoker + the
// generated edge face). Holds the store (the domain write), the bus (the atomic
// durable event append), and the resolved MmrReader (the sync pre-emit read).
// ============================================================================

pub struct Service {
    pool: PgPool,
    bus: Arc<Bus>,
    /// The rating capability read before recording a match. Resolved in `init` (phase
    /// 2) — the local `rating` module or a remote stub Provided it in phase 1.
    rating: OnceLock<Arc<dyn MmrReader>>,
}

impl Service {
    const REPORT_ID_CONFLICT: &'static str = "ReportId already used for a different match";

    fn rating(&self) -> &Arc<dyn MmrReader> {
        self.rating
            .get()
            .expect("match.init must resolve the rating MmrReader before report")
    }

    /// Inserts a match row on the given connection (a tx, so the row + its durable
    /// event append commit together) and returns the generated `id` — or `None` when
    /// `report_id` was already recorded (`ON CONFLICT DO NOTHING`): the caller skips
    /// the emit, making a retried report idempotent (pattern:
    /// `inventory.wiped_characters`).
    async fn insert_tx(
        &self,
        conn: &mut PgConnection,
        report_id: &str,
        winner: &str,
        loser: &str,
    ) -> Result<Option<String>, sqlx::Error> {
        let row: Option<(String,)> = sqlx::query_as(
            "INSERT INTO match.matches (report_id, winner, loser) VALUES ($1, $2, $3) \
             ON CONFLICT (report_id) DO NOTHING RETURNING id::text",
        )
        .bind(report_id)
        .bind(winner)
        .bind(loser)
        .fetch_optional(&mut *conn)
        .await?;
        Ok(row.map(|(id,)| id))
    }

    async fn existing_report(
        &self,
        report_id: &str,
    ) -> Result<Option<(String, String)>, sqlx::Error> {
        sqlx::query_as("SELECT winner, loser FROM match.matches WHERE report_id = $1")
            .bind(report_id)
            .fetch_optional(&self.pool)
            .await
    }

    async fn existing_report_tx(
        &self,
        conn: &mut PgConnection,
        report_id: &str,
    ) -> Result<Option<(String, String)>, sqlx::Error> {
        sqlx::query_as("SELECT winner, loser FROM match.matches WHERE report_id = $1")
            .bind(report_id)
            .fetch_optional(&mut *conn)
            .await
    }

    fn duplicate_result(
        existing: &(String, String),
        winner: &str,
        loser: &str,
    ) -> Result<(), Error> {
        if existing.0 == winner && existing.1 == loser {
            Ok(())
        } else {
            Err(Error::conflict(Self::REPORT_ID_CONFLICT))
        }
    }
}

#[async_trait]
impl Match for Service {
    /// Records that `winner` beat `loser`. `report_id` is the REQUIRED idempotency key
    /// (empty ⇒ `Invalid` — a missing key must never silently degrade the dedup). An
    /// existing report is checked before the synchronous MMR dependency: the same
    /// payload is an immediate 202 no-op, while reusing the id for a DIFFERENT
    /// winner/loser pair is a 409 Conflict (`Error::conflict`).
    /// For a new report the MMR read is SYNCHRONOUS (query rating right now — Go read the
    /// winner's MMR; we read both to exercise the wire, doing nothing material with the
    /// values). Then the domain INSERT + the `match.finished` durable event append
    /// commit in ONE tx: the event is durable iff the match is. A duplicate `report_id`
    /// (the explicitly retry-safe RPC may replay after a lost response) inserts nothing, emits nothing, and
    /// returns Ok — at-most-once effect per report. A rating transport failure surfaces
    /// as an error (the sync dep is required).
    async fn report(&self, report_id: String, winner: String, loser: String) -> Result<(), Error> {
        if report_id.trim().is_empty() {
            return Err(Error::invalid("ReportId must be a non-empty idempotency key"));
        }
        if let Some(existing) = self.existing_report(&report_id).await.map_err(internal)? {
            return Self::duplicate_result(&existing, &winner, &loser);
        }

        let winner_mmr = self.rating().mmr(winner.clone()).await?;
        let loser_mmr = self.rating().mmr(loser.clone()).await?;
        tracing::info!(%winner, winner_mmr, %loser, loser_mmr, "match reported");

        let mut tx = self.pool.begin().await.map_err(internal)?;
        let Some(match_id) = self
            .insert_tx(&mut tx, &report_id, &winner, &loser)
            .await
            .map_err(internal)?
        else {
            // Another transaction won after our preflight lookup. The INSERT waits
            // for that transaction, so this statement observes and verifies the
            // committed payload before deciding whether the retry is a no-op.
            let existing = self
                .existing_report_tx(&mut tx, &report_id)
                .await
                .map_err(internal)?
                .ok_or_else(|| Error::internal("conflicting ReportId row disappeared"))?;
            let result = Self::duplicate_result(&existing, &winner, &loser);
            if result.is_ok() {
                tracing::info!(%report_id, %winner, %loser, "duplicate match report deduped");
            }
            // The conflict branch performed no write, but it still owns an open sqlx
            // transaction after the INSERT/SELECT. Roll it back explicitly: relying on
            // Drop defers ROLLBACK and can leave locks held for a following request.
            tx.rollback().await.map_err(internal)?;
            return result;
        };
        let evt = matchevents::Finished {
            match_id,
            winner,
            loser,
        };
        self.bus
            .emit_tx(AnyTx::new(&mut *tx), &matchevents::FINISHED, &evt)
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

    /// `rating` is a SYNC dependency (the pre-emit MMR read), resolved downward via
    /// the registry. The durable plane (`emit_tx`) is app-owned process
    /// infrastructure, not a declared dependency.
    fn requires(&self) -> Vec<String> {
        vec!["rating".to_string()]
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
