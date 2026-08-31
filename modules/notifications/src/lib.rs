//! `notifications` — the player-facing in-app inbox: one append-mostly table of messages
//! addressed to exactly one player each, read and mutated only by that player.
//!
//! [`Service::deliver_on`] is the single insert authority. It runs a whole delivery on a
//! CALLER-OWNED connection and never touches transaction control, so operator mail (a
//! pool-owned transaction) and the durable fan-in (the event plane's handed delivery
//! transaction) share one implementation and one input policy.

mod service;
mod store;

use store::*;

pub use service::{NewNotification, Service};

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use lifecycle::{Context, Module};
use notificationsapi::Player;
use registry::key;

/// The three `_len` CHECKs are `octet_length`, not `char_length`, because their Rust twins
/// (`notificationsapi::MAX_*_BYTES`, enforced in [`Service::deliver_on`]) are `str::len()`
/// BYTE counts — a 200-character multibyte title is more than 200 octets. The column CHECK
/// is the class fail-safe UNDER those caps, not a second policy, and a bump on one side
/// without the other turns a 400 into an unmapped 23514 (a 500).
///
/// The partial unique index on `source_event_id` is a BELT: durable delivery is already
/// exactly-once for a `TransactionalPg` consumer, so its job is to make an operator
/// re-drive (`eventctl`) idempotent rather than duplicate a player's inbox. It is partial
/// because operator mail carries no source event and NULLs would otherwise collide.
///
/// `player_id` is a plain `text` id column — no cross-module FK, and no `::uuid` cast, so
/// the identity's own spelling is the key.
const SCHEMA_DDL: &str = r#"
CREATE SCHEMA IF NOT EXISTS notifications;

CREATE TABLE IF NOT EXISTS notifications.messages (
	id              uuid        PRIMARY KEY,
	player_id       text        NOT NULL,
	kind            text        NOT NULL,
	title           text        NOT NULL,
	body            text        NOT NULL,
	created_at      timestamptz NOT NULL DEFAULT now(),
	read_at         timestamptz,
	source_event_id text,
	CONSTRAINT notifications_title_len CHECK (octet_length(title) <= 200),
	CONSTRAINT notifications_body_len  CHECK (octet_length(body)  <= 4000),
	CONSTRAINT notifications_kind_len  CHECK (octet_length(kind)  <= 64)
);

CREATE UNIQUE INDEX IF NOT EXISTS notifications_source_event_idx
	ON notifications.messages (source_event_id) WHERE source_event_id IS NOT NULL;

CREATE INDEX IF NOT EXISTS notifications_inbox_idx
	ON notifications.messages (player_id, created_at DESC, id DESC);"#;

pub(crate) fn internal<E: std::fmt::Display>(e: E) -> opsapi::Error {
    opsapi::Error::internal(e.to_string())
}

pub struct NotificationsModule {
    svc: OnceLock<Arc<Service>>,
}

impl Default for NotificationsModule {
    fn default() -> Self {
        NotificationsModule::new()
    }
}

impl NotificationsModule {
    pub fn new() -> NotificationsModule {
        NotificationsModule {
            svc: OnceLock::new(),
        }
    }

    fn svc(&self) -> Arc<Service> {
        self.svc
            .get()
            .expect("notifications.register must run before init/migrate")
            .clone()
    }
}

#[async_trait]
impl Module for NotificationsModule {
    fn name(&self) -> &str {
        "notifications"
    }

    /// Empty: the inbox consumes no sync capability. Retention is read from
    /// `NOTIFICATIONS_RETENTION_DAYS` rather than `dyn Config`, and the fan-in arrives over
    /// durable events, which are never a declared requirement.
    fn requires(&self) -> Vec<String> {
        vec![]
    }

    fn register(&self, ctx: &Context) -> anyhow::Result<()> {
        let pool = ctx
            .db()
            .ok_or_else(|| anyhow::anyhow!("notifications requires a DB pool"))?
            .clone();
        let svc = Arc::new(Service::new(pool));
        self.svc
            .set(svc.clone())
            .map_err(|_| anyhow::anyhow!("notifications.register ran twice"))?;

        ctx.registry()
            .provide::<dyn Player>(key("notifications", "player"), svc);
        Ok(())
    }

    async fn migrate(&self, ctx: &Context) -> anyhow::Result<()> {
        let pool = ctx
            .db()
            .ok_or_else(|| anyhow::anyhow!("notifications requires a DB pool"))?;
        sqlx::raw_sql(SCHEMA_DDL).execute(pool).await?;
        Ok(())
    }

    fn init(&self, ctx: &Context) -> anyhow::Result<()> {
        let svc = self.svc();

        for op in notificationsapi::player_rpc::operations(svc.clone()) {
            ctx.contribute(opsapi::SLOT, op.operation);
            ctx.contribute(opsapi::BINDING_SLOT, op.binding);
            ctx.contribute(opsapi::LOCAL_SLOT, op.local);
        }

        // Contributed UNCONDITIONALLY — topology-blind: `app::run` applies it iff this
        // process serves an internal edge; in the monolith it is never applied.
        ctx.contribute(
            edge::EDGE_SLOT,
            edge::EdgeReg::new(move |server| {
                notificationsrpc::player_rpc::register_server(server, svc.clone());
            }),
        );

        ctx.contribute(
            opsapi::DESCRIBE_SLOT,
            notificationsrpc::player_rpc::describe(),
        );
        Ok(())
    }
}
