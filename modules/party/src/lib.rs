//! `party` — short-lived player parties: a lease-bound membership row per player (its primary
//! key is the one-party-per-player rule), pending invites, and one global revision sequence
//! ordering every snapshot a client sees.
//!
//! State lives in Postgres; real time is a best-effort `party.state` push after commit. No
//! durable topics.

pub mod conformance;
mod service;
mod store;
mod sweep;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod service_tests;
#[cfg(test)]
mod race_tests;
#[cfg(test)]
mod sweep_tests;
#[cfg(test)]
mod purge_tests;

pub use service::Service;

use std::sync::{Arc, Mutex, OnceLock};

use accountsapi::Directory;
use async_trait::async_trait;
use lifecycle::{Context, Module};
use partyapi::Player;
use registry::key;
use tokio::sync::watch;
use tokio::task::JoinHandle;

/// `members.player_id` as PRIMARY KEY is the one-party-per-player rule; a second insert is a
/// `23505` the service answers as `Conflict`. Plain `uuid` columns, no cross-module FK.
const SCHEMA_DDL: &str = r#"
CREATE SCHEMA IF NOT EXISTS party;
CREATE SEQUENCE IF NOT EXISTS party.revision_seq;
CREATE TABLE IF NOT EXISTS party.player_revisions (
    player_id  uuid   PRIMARY KEY,
    revision   bigint NOT NULL
);
CREATE TABLE IF NOT EXISTS party.parties (
    id         uuid        PRIMARY KEY,
    leader_id  uuid        NOT NULL,
    revision   bigint      NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE TABLE IF NOT EXISTS party.members (
    player_id   uuid        PRIMARY KEY,
    party_id    uuid        NOT NULL,
    ready       text        NOT NULL,
    joined_at   timestamptz NOT NULL,
    lease_until timestamptz NOT NULL,
    CONSTRAINT members_ready_check CHECK (ready IN ('ready','not_ready'))
);
CREATE INDEX IF NOT EXISTS members_party_idx ON party.members (party_id);
CREATE INDEX IF NOT EXISTS members_lease_idx ON party.members (lease_until);
CREATE TABLE IF NOT EXISTS party.invites (
    id          uuid        PRIMARY KEY,
    party_id    uuid        NOT NULL,
    inviter_id  uuid        NOT NULL,
    invitee_id  uuid        NOT NULL,
    expires_at  timestamptz NOT NULL,
    UNIQUE (party_id, invitee_id)
);
CREATE INDEX IF NOT EXISTS invites_invitee_idx ON party.invites (invitee_id);
CREATE INDEX IF NOT EXISTS invites_inviter_idx ON party.invites (inviter_id);
CREATE INDEX IF NOT EXISTS invites_expires_idx ON party.invites (expires_at);"#;

/// The durable purge subscription: a contract, renaming it abandons the checkpoint.
/// `AfterRegistration`: party rows are lease-bound, so a deletion from before this module
/// first subscribed names no row that can still exist.
const PLAYER_DELETED_SUB: bus::SubscriptionSpec = bus::SubscriptionSpec {
    id: "party.player-deleted.v1",
    start: bus::StartPosition::AfterRegistration,
};

pub(crate) const LEASE_ENV: &str = "PARTY_LEASE_SECS";
pub(crate) const DEFAULT_LEASE_SECS: i64 = 60;
pub(crate) const MIN_LEASE_SECS: i64 = 10;
pub(crate) const MAX_LEASE_SECS: i64 = 600;

/// ONLY an unset variable takes the default; anything present must parse and be in range,
/// the retention-env convention — `PARTY_LEASE_SECS=${X}` with `X` unset expands to empty.
pub(crate) fn lease_secs_from_env() -> anyhow::Result<i64> {
    let raw = match std::env::var(LEASE_ENV) {
        Ok(v) => v,
        Err(std::env::VarError::NotPresent) => return Ok(DEFAULT_LEASE_SECS),
        Err(std::env::VarError::NotUnicode(v)) => anyhow::bail!(
            "party: {LEASE_ENV} is not valid unicode ({v:?}); unset it for the default \
             {DEFAULT_LEASE_SECS}"
        ),
    };
    let secs: i64 = raw.trim().parse().map_err(|_| {
        anyhow::anyhow!(
            "party: {LEASE_ENV} must be a whole number of seconds (got {raw:?}); unset it for \
             the default {DEFAULT_LEASE_SECS}"
        )
    })?;
    if !(MIN_LEASE_SECS..=MAX_LEASE_SECS).contains(&secs) {
        anyhow::bail!(
            "party: {LEASE_ENV} must be between {MIN_LEASE_SECS} and {MAX_LEASE_SECS} (got \
             {secs}); unset it for the default {DEFAULT_LEASE_SECS}"
        );
    }
    Ok(secs)
}

pub(crate) fn internal<E: std::fmt::Display>(e: E) -> opsapi::Error {
    opsapi::Error::internal(e.to_string())
}

pub struct Party {
    svc: OnceLock<Arc<Service>>,
    liveness: sweep::Liveness,
    stop_tx: Mutex<Option<watch::Sender<bool>>>,
    task: Mutex<Option<JoinHandle<()>>>,
}

impl Default for Party {
    fn default() -> Self {
        Party::new()
    }
}

impl Party {
    pub fn new() -> Party {
        Party {
            svc: OnceLock::new(),
            liveness: sweep::Liveness::default(),
            stop_tx: Mutex::new(None),
            task: Mutex::new(None),
        }
    }

    fn svc(&self) -> Arc<Service> {
        self.svc
            .get()
            .expect("party.register must run before init/migrate/start")
            .clone()
    }
}

#[async_trait]
impl Module for Party {
    fn name(&self) -> &str {
        "party"
    }

    /// `accounts` is a hard sync dependency: `invite` resolves its target through
    /// `accountsapi::Directory` and every view hydrates its handles through it.
    fn requires(&self) -> Vec<String> {
        vec!["accounts".into()]
    }

    fn register(&self, ctx: &Context) -> anyhow::Result<()> {
        let lease_secs = lease_secs_from_env()?;
        let pool = ctx
            .db()
            .ok_or_else(|| anyhow::anyhow!("party requires a DB pool"))?
            .clone();
        let svc = Arc::new(Service::new(pool, ctx.push().clone(), lease_secs));
        self.svc
            .set(svc.clone())
            .map_err(|_| anyhow::anyhow!("party.register ran twice"))?;
        ctx.registry()
            .provide::<dyn Player>(key("party", "player"), svc);
        Ok(())
    }

    async fn migrate(&self, ctx: &Context) -> anyhow::Result<()> {
        let pool = ctx
            .db()
            .ok_or_else(|| anyhow::anyhow!("party requires a DB pool"))?;
        sqlx::raw_sql(SCHEMA_DDL).execute(pool).await?;
        Ok(())
    }

    fn init(&self, ctx: &Context) -> anyhow::Result<()> {
        let svc = self.svc();

        let directory = ctx
            .registry()
            .require::<dyn Directory>(&key("accounts", "directory"));
        let _ = svc.directory.set(directory);

        let purger = svc.clone();
        ctx.bus().on_tx(
            PLAYER_DELETED_SUB,
            &accountsevents::PLAYER_DELETED,
            move |mut delivery, e: accountsevents::PlayerDeleted| {
                let purger = purger.clone();
                Box::pin(async move {
                    let conn = delivery.tx.downcast::<sqlx::PgConnection>()?;
                    purger.purge_player_tx(conn, &e.player_id).await
                })
            },
        );

        for op in partyapi::player_rpc::operations(svc.clone()) {
            ctx.contribute(opsapi::SLOT, op.operation);
            ctx.contribute(opsapi::BINDING_SLOT, op.binding);
            ctx.contribute(opsapi::LOCAL_SLOT, op.local);
        }

        // Contributed UNCONDITIONALLY — topology-blind: `app::run` applies it iff this
        // process serves an internal edge.
        ctx.contribute(
            edge::EDGE_SLOT,
            edge::EdgeReg::new(move |server| {
                partyrpc::player_rpc::register_server(server, svc.clone());
            }),
        );

        ctx.contribute(opsapi::DESCRIBE_SLOT, partyrpc::player_rpc::describe());

        // Dark until `start` seeds the stamp: an unstarted sweep reads ready.
        let liveness = self.liveness.clone();
        ctx.contribute(
            httpmw::READINESS_SLOT,
            httpmw::ReadyCheck::new("party", move || {
                let liveness = liveness.clone();
                async move { liveness.check(sweep::STALL_MAX) }
            }),
        );
        Ok(())
    }

    /// The sweep runs on a fresh task, not tied to the `start` ctx.
    async fn start(&self, _ctx: &Context) -> anyhow::Result<()> {
        // Seeded before the first pass: HTTP serves before it completes on a cold boot.
        self.liveness.mark_pass_ok();
        let (stop_tx, stop_rx) = watch::channel(false);
        let task = sweep::spawn(self.svc(), self.liveness.clone(), stop_rx);
        *self.stop_tx.lock().unwrap() = Some(stop_tx);
        *self.task.lock().unwrap() = Some(task);
        Ok(())
    }

    /// Marks the liveness stopping BEFORE signalling, so the supervision wrapper reads a
    /// controlled exit and `/readyz` never counts a stopping process as stalled.
    async fn stop(&self, _ctx: &Context) -> anyhow::Result<()> {
        self.liveness.set_stopping();
        let stop_tx = self.stop_tx.lock().unwrap().take();
        let task = self.task.lock().unwrap().take();
        sweep::stop_task(stop_tx, task).await;
        Ok(())
    }
}
