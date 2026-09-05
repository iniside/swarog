//! `mail` — the outbound-email channel: a durable outbox, one configured provider, and a
//! drain that owns every retry. It is the backend's first channel that talks to the world.
//!
//! Its ingress is the durable `mail.send_requested` topic, which this module both defines
//! and consumes — the deviation recorded in `mailevents`' crate doc. A sender appends the
//! event inside its own transaction and never waits for a relay, so there is no sync
//! capability here (no `mailapi`) and nothing `requires()` mail.
//!
//! **Enqueue is exactly-once per `idempotency_key`; delivery to the recipient is
//! at-least-once.** A process that dies after the relay accepted the message but before
//! the status write commits re-sends it — inherent to an outbox that is not in a
//! distributed transaction with the relay, and the reason a link this channel carries must
//! be safe to follow twice.

mod address;
mod admin;
pub mod config;
pub mod conformance;
mod projection;
pub mod providers;
mod service;
pub mod smtp;
mod store;
mod worker;

// ============================================================================
// Tests target the local Postgres (the test DB) and SKIP cleanly when it is
// unreachable. In-crate so they can drive the private `Service`/`Store`/`Drain`
// directly.
// ============================================================================
#[cfg(test)]
mod config_tests;
#[cfg(test)]
mod projection_tests;
#[cfg(test)]
mod smtp_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod worker_tests;

use std::sync::{Arc, Mutex, OnceLock};

use async_trait::async_trait;
use lifecycle::{Context, Module};
use sqlx::PgPool;
use tokio::sync::watch;
use tokio::task::JoinHandle;

pub use config::MailConfig;
pub use service::Service;

/// Creates this module's OWN schema and nothing else — full logical isolation (#10).
/// Idempotent.
///
/// The five `_len_check` CHECKs are `octet_length`, not `char_length`, because their Rust
/// twins (`store::COLUMN_CAPS`, enforced in `service::validate_new`) are `str::len()` BYTE
/// counts. The CHECK is the class fail-safe UNDER those caps, not a second policy: a field
/// capped on one side only reaches the durable handler as an unmapped 23514, which is
/// infrastructure trouble and pauses the subscription.
///
/// **There is deliberately no `sending` state.** A process that died mid-send would strand
/// rows in it forever; instead the claim is a committed `UPDATE` that bumps `attempts` and
/// pushes `next_attempt_at` out by a lease, so a crash makes the row due again when the
/// lease expires, at the cost of one burnt attempt — fail-closed toward parking rather
/// than toward an unbounded resend loop.
///
/// `generation` is the row's MONOTONE claim counter and the only ABA guard a status write
/// may CAS on. Every transition that hands the row back to the drain — the claim and the
/// operator requeue — bumps it, and nothing ever lowers it, so a write from a superseded
/// attempt can never match. `attempts` cannot serve this: the requeue resets it to 0, which
/// makes an older attempt's value reachable again.
///
/// Each index has a named consumer: `mail_outbox_due_idx` serves the drain's claim and the
/// operator page's pending count and queue-head age, `mail_outbox_parked_idx` the
/// parked-count gauge, the bulk requeue and the page's parked count, `mail_outbox_sent_idx`
/// the page's 24h-delivered count, and `mail_outbox_recent_idx` both the page's capped
/// listing and the retention sweep's `created_at` range predicate — btrees scan in either
/// direction, so the DESC index serves the ascending sweep too, and a sweep running inside
/// a delivery transaction cannot afford a seq scan.
const SCHEMA_DDL: &str = r#"
CREATE SCHEMA IF NOT EXISTS mail;

CREATE TABLE IF NOT EXISTS mail.outbox (
	id              uuid PRIMARY KEY DEFAULT gen_random_uuid(),
	idempotency_key text        NOT NULL UNIQUE,
	recipient       text        NOT NULL,
	subject         text        NOT NULL,
	body            text        NOT NULL,
	kind            text        NOT NULL,
	state           text        NOT NULL DEFAULT 'pending',
	attempts        int         NOT NULL DEFAULT 0,
	generation      int         NOT NULL DEFAULT 0,
	next_attempt_at timestamptz NOT NULL DEFAULT now(),
	last_error      text,
	provider        text,
	sent_at         timestamptz,
	created_at      timestamptz NOT NULL DEFAULT now(),
	updated_at      timestamptz NOT NULL DEFAULT now(),
	CONSTRAINT mail_outbox_state_check
		CHECK (state IN ('pending','sent','parked','cancelled')),
	CONSTRAINT mail_outbox_recipient_len_check CHECK (octet_length(recipient) <= 320),
	CONSTRAINT mail_outbox_subject_len_check   CHECK (octet_length(subject)   <= 200),
	CONSTRAINT mail_outbox_body_len_check      CHECK (octet_length(body)      <= 65536),
	CONSTRAINT mail_outbox_kind_len_check      CHECK (octet_length(kind)      <= 64),
	CONSTRAINT mail_outbox_key_len_check       CHECK (octet_length(idempotency_key) <= 128)
);

CREATE INDEX IF NOT EXISTS mail_outbox_due_idx
	ON mail.outbox (next_attempt_at) WHERE state = 'pending';

CREATE INDEX IF NOT EXISTS mail_outbox_parked_idx
	ON mail.outbox (created_at) WHERE state = 'parked';

CREATE INDEX IF NOT EXISTS mail_outbox_sent_idx
	ON mail.outbox (sent_at) WHERE state = 'sent';

CREATE INDEX IF NOT EXISTS mail_outbox_recent_idx
	ON mail.outbox (created_at DESC, id DESC);"#;

/// The `/readyz` verdict of a process that accepts mail it can never deliver. A boot
/// warning that scrolled past is not a signal: with no provider the channel still enqueues
/// and still checkpoints, so the only honest report is red. A deployment that wants no mail
/// leaves this module out of its process's module list.
pub(crate) const NO_PROVIDER_READY: &str =
    "MAIL_PROVIDER is not set: requests are accepted and enqueued, and nothing will ever \
     send them";

pub(crate) fn internal<E: std::fmt::Display>(e: E) -> opsapi::Error {
    opsapi::Error::internal(e.to_string())
}

pub struct MailModule {
    svc: OnceLock<Arc<Service>>,
    cfg: OnceLock<Arc<MailConfig>>,
    pool: OnceLock<PgPool>,
    /// `Some` iff a provider is configured. Built in `register`, so an unbuildable
    /// provider is a startup failure rather than a per-message one.
    sender: OnceLock<Arc<dyn providers::Sender>>,
    /// Drain health for the `"mail"` `/readyz` check — cloned into both the `init`-time
    /// check and the `start`-time supervision wrapper.
    liveness: worker::Liveness,
    stop_tx: Mutex<Option<watch::Sender<bool>>>,
    tasks: Mutex<Vec<JoinHandle<()>>>,
}

impl Default for MailModule {
    fn default() -> Self {
        MailModule::new()
    }
}

impl MailModule {
    pub fn new() -> MailModule {
        MailModule {
            svc: OnceLock::new(),
            cfg: OnceLock::new(),
            pool: OnceLock::new(),
            sender: OnceLock::new(),
            liveness: worker::Liveness::default(),
            stop_tx: Mutex::new(None),
            tasks: Mutex::new(Vec::new()),
        }
    }

    fn svc(&self) -> Arc<Service> {
        self.svc
            .get()
            .expect("mail.register must run before init/migrate")
            .clone()
    }

    fn cfg(&self) -> Arc<MailConfig> {
        self.cfg
            .get()
            .expect("mail.register must run before init/migrate")
            .clone()
    }
}

#[async_trait]
impl Module for MailModule {
    fn name(&self) -> &str {
        "mail"
    }

    /// Empty: mail consumes no sync capability. Its ingress is a durable subscription,
    /// which is never a declared requirement, and its provider is configuration.
    fn requires(&self) -> Vec<String> {
        vec![]
    }

    /// Phase 1: builds the outbox service and parses the environment ONCE, so a
    /// misconfigured provider is a startup failure rather than a per-message error the
    /// drain discovers hours later. Reading env is not I/O; nothing here dials anything.
    fn register(&self, ctx: &Context) -> anyhow::Result<()> {
        // Fail at BUILD rather than at the first migrate statement: a process that lists
        // mail without a DB has no outbox to write to.
        let pool = ctx
            .db()
            .ok_or_else(|| anyhow::anyhow!("mail requires a DB pool"))?
            .clone();
        self.pool
            .set(pool.clone())
            .map_err(|_| anyhow::anyhow!("mail.register ran twice"))?;
        self.svc
            .set(Arc::new(Service::new(pool)))
            .map_err(|_| anyhow::anyhow!("mail.register ran twice"))?;
        let cfg = MailConfig::from_env()?;
        // The transport is built HERE, not in `start`: construction is pure (no socket,
        // no DNS), and a host the transport cannot represent must fail the boot rather
        // than every send.
        if let Some(settings) = &cfg.provider {
            let sender = settings
                .provider
                .sender(&settings.from, cfg.send_timeout)?;
            let _ = self.sender.set(sender);
        }
        self.cfg
            .set(Arc::new(cfg))
            .map_err(|_| anyhow::anyhow!("mail.register ran twice"))?;
        Ok(())
    }

    async fn migrate(&self, ctx: &Context) -> anyhow::Result<()> {
        let pool = ctx
            .db()
            .ok_or_else(|| anyhow::anyhow!("mail requires a DB pool"))?;
        sqlx::raw_sql(SCHEMA_DDL).execute(pool).await?;
        Ok(())
    }

    fn init(&self, ctx: &Context) -> anyhow::Result<()> {
        let svc = self.svc();

        // Contributed UNCONDITIONALLY, provider or not: the ingress is the same code in
        // both topologies, and an env-gated subscription would report the topic as
        // sinkless to `topiccheck`, which builds the module set under a bare environment.
        let ingress_svc = svc.clone();
        ctx.bus().on_tx(
            projection::SEND_REQUESTED_SUB,
            &mailevents::SEND_REQUESTED,
            move |delivery, e: mailevents::SendRequested| {
                projection::on_send_requested(ingress_svc.clone(), delivery, e)
            },
        );

        let prune: Arc<dyn bus::TxHandler> = Arc::new(projection::PruneHandler {
            retention_days: self.cfg().retention_days,
        });
        ctx.bus()
            .on_tx_raw(projection::PRUNE_SUB, schedulerevents::FIRED.topic(), prune);

        // The local "Mail" page. The `RenderFn` is synchronous; `admin::admin_render`
        // bridges to the async store reads via `block_in_place`.
        let render_svc = svc.clone();
        ctx.contribute(
            adminapi::SLOT,
            adminapi::Item::local(
                admin::ADMIN_ITEM_ID,
                admin::ADMIN_SECTION,
                admin::ADMIN_LABEL,
                Arc::new(move |params: &adminapi::Params| admin::admin_render(&render_svc, params)),
            ),
        );

        // Contributed UNCONDITIONALLY — topology-blind: `app::run` applies it iff this
        // process serves an internal edge; in the monolith it is never applied. The admin
        // fan-out READ face and, alongside it, the opt-in WRITE face, both through this
        // module's OWN glue crate's re-exports — the write face is what makes the Mail page
        // editable from a REMOTE admin process.
        ctx.contribute(
            edge::EDGE_SLOT,
            edge::EdgeReg::new(move |server| {
                mailrpc::register_admin(server, svc.clone());
                mailrpc::register_admin_submit(server, svc.clone());
            }),
        );

        // The two arms of ONE `/readyz` check, mutually exclusive by construction: an
        // unconfigured channel is permanently not-ready, a configured one reports its
        // drain's health.
        if self.cfg().provider.is_none() {
            tracing::warn!("mail: {NO_PROVIDER_READY}");
            ctx.contribute(
                httpmw::READINESS_SLOT,
                httpmw::ReadyCheck::new("mail", || async {
                    Err(NO_PROVIDER_READY.to_string())
                }),
            );
        } else {
            let liveness = self.liveness.clone();
            let stall_max = worker::stall_max(self.cfg().send_timeout);
            ctx.contribute(
                httpmw::READINESS_SLOT,
                httpmw::ReadyCheck::new("mail", move || {
                    let liveness = liveness.clone();
                    async move { liveness.check(stall_max) }
                }),
            );
        }
        Ok(())
    }

    /// Launches the drain on a FRESH `tokio::spawn` task (not tied to the `start` ctx), so
    /// a short start deadline cannot kill the loop. A process with no provider configured
    /// starts nothing — its `/readyz` already says the channel is undrained.
    async fn start(&self, _ctx: &Context) -> anyhow::Result<()> {
        let cfg = self.cfg();
        let Some(settings) = cfg.provider.as_ref() else {
            return Ok(());
        };
        // Never a silent return: a configured provider whose sender is missing would leave
        // the channel undrained behind a readiness check that reports drain health.
        let sender = self.sender.get().ok_or_else(|| {
            anyhow::anyhow!("mail.register must build a sender for a configured provider")
        })?;
        let drain = worker::Drain {
            pool: self
                .pool
                .get()
                .expect("mail.register must run before start")
                .clone(),
            sender: sender.clone(),
            from: settings.from.clone(),
            send_timeout: cfg.send_timeout,
            max_attempts: cfg.max_attempts,
        };
        let (stop_tx, stop_rx) = watch::channel(false);
        let task = worker::spawn(drain, store::Store, self.liveness.clone(), stop_rx);
        *self.stop_tx.lock().unwrap() = Some(stop_tx);
        self.tasks.lock().unwrap().push(task);
        Ok(())
    }

    /// Signals the drain and awaits its exit, bounded by the worker's stop grace with an
    /// abort fallback — see the drain worker's `stop_tasks`.
    async fn stop(&self, _ctx: &Context) -> anyhow::Result<()> {
        // Before signalling, so the supervision wrapper reads a controlled exit and the
        // readiness probe never counts a stopping process as stalled.
        self.liveness.set_stopping();
        let stop_tx = self.stop_tx.lock().unwrap().take();
        let tasks = std::mem::take(&mut *self.tasks.lock().unwrap());
        worker::stop_tasks(stop_tx, tasks).await;
        Ok(())
    }
}
