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
pub mod config;
mod projection;
pub mod providers;
mod service;
mod store;

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use lifecycle::{Context, Module};

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
/// Each index has a named consumer: `mail_outbox_due_idx` serves the drain's claim,
/// `mail_outbox_parked_idx` the parked-count gauge and the bulk requeue, and
/// `mail_outbox_recent_idx` both the admin page's keyset listing and the retention sweep's
/// `created_at` range predicate — btrees scan in either direction, so the DESC index
/// serves the ascending sweep too, and a sweep running inside a delivery transaction
/// cannot afford a seq scan.
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
        if ctx.db().is_none() {
            anyhow::bail!("mail requires a DB pool");
        }
        self.svc
            .set(Arc::new(Service::new()))
            .map_err(|_| anyhow::anyhow!("mail.register ran twice"))?;
        self.cfg
            .set(Arc::new(MailConfig::from_env()?))
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
        ctx.bus().on_tx(
            projection::SEND_REQUESTED_SUB,
            &mailevents::SEND_REQUESTED,
            move |delivery, e: mailevents::SendRequested| {
                projection::on_send_requested(svc.clone(), delivery, e)
            },
        );

        if self.cfg().provider.is_none() {
            tracing::warn!("mail: {NO_PROVIDER_READY}");
            ctx.contribute(
                httpmw::READINESS_SLOT,
                httpmw::ReadyCheck::new("mail", || async {
                    Err(NO_PROVIDER_READY.to_string())
                }),
            );
        }
        Ok(())
    }
}
