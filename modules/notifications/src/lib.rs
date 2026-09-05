//! `notifications` — the player-facing in-app inbox: one append-mostly table of messages
//! addressed to exactly one player each, read and mutated only by that player.
//!
//! [`Service::deliver_on`] is the single insert authority. It runs a whole delivery on a
//! CALLER-OWNED connection and never touches transaction control, so operator mail (a
//! pool-owned transaction) and the durable fan-in (the event plane's handed delivery
//! transaction) share one implementation and one input policy.

mod admin;
pub mod conformance;
mod projection;
mod service;
mod store;
#[cfg(test)]
mod tests;

use store::*;

pub use service::Service;

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use lifecycle::{Context, Module};
use notificationsapi::Player;
use registry::key;

/// The column CHECKs below are `octet_length`, not `char_length`, because their Rust twins
/// (`notificationsapi::MAX_*_BYTES`, enforced in [`Service::deliver_on`]) are `str::len()`
/// BYTE counts — a 200-character multibyte title is more than 200 octets. The CHECK is the
/// class fail-safe UNDER those caps, not a second policy, and a bump on one side without
/// the other turns a 400 into an unmapped 23514 (a 500).
///
/// The unique index on `source_event_id` is the ONE dedup authority both write paths share:
/// durable delivery is already exactly-once for a `TransactionalPg` consumer, so its job
/// there is to make an operator re-drive (`eventctl`) idempotent rather than duplicate a
/// player's inbox, and operator mail rides the same column under the render-time
/// `admin-send-mail-` key prefix that keeps the two key spaces disjoint — a prefix a
/// `gen_random_uuid()::text` `event_id` cannot contain. It is PARTIAL as the class fail-safe
/// under `service::validate_new`, which REFUSES a keyless write: a NULL here would opt its
/// row out of dedup entirely, so an operator re-drive would append a second copy.
///
/// `notifications_created_at_idx` exists for the retention sweep alone: `created_at` is not
/// the leading column of the inbox index, so without it the daily prune seq-scans the whole
/// table inside a delivery transaction bounded by `ASYNCEVENTS_HANDLER_TIMEOUT`.
///
/// `player_id` is a plain id column (no cross-module FK) but a `uuid`, matching every other
/// module that carries one, and every statement binds `$n::uuid`. That cast is TOLERANT — an
/// uppercase, braced or unhyphenated spelling folds onto one player — which serves the
/// operator send-mail form: a pasted Windows-style `{ABC…}` id addresses the inbox its owner
/// reads instead of writing a row nobody can ever see, and a genuine typo is a loud 22P02.
/// The DURABLE path is deliberately stricter and never reaches the cast: see
/// `projection::deliverable_player_id` for why a delivery transaction cannot afford one.
const SCHEMA_DDL: &str = r#"
CREATE SCHEMA IF NOT EXISTS notifications;

CREATE TABLE IF NOT EXISTS notifications.messages (
	id              uuid        PRIMARY KEY,
	player_id       uuid        NOT NULL,
	kind            text        NOT NULL,
	title           text        NOT NULL,
	body            text        NOT NULL,
	created_at      timestamptz NOT NULL DEFAULT now(),
	read_at         timestamptz,
	source_event_id text,
	CONSTRAINT notifications_title_len_check CHECK (octet_length(title) <= 200),
	CONSTRAINT notifications_body_len_check  CHECK (octet_length(body)  <= 4000),
	CONSTRAINT notifications_kind_len_check  CHECK (octet_length(kind)  <= 64)
);

CREATE UNIQUE INDEX IF NOT EXISTS notifications_source_event_idx
	ON notifications.messages (source_event_id) WHERE source_event_id IS NOT NULL;

CREATE INDEX IF NOT EXISTS notifications_inbox_idx
	ON notifications.messages (player_id, created_at DESC, id DESC);

CREATE INDEX IF NOT EXISTS notifications_created_at_idx
	ON notifications.messages (created_at);"#;

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
        let svc = Arc::new(Service::new(pool, ctx.push().clone()));
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
        let retention_days = projection::retention_days_from_env()?;
        let svc = self.svc();

        // Independent subscriptions, each with its own checkpoint (audit's shape): one
        // topic's poison event must never stall another's cursor.
        let credit_svc = svc.clone();
        ctx.bus().on_tx(
            projection::WALLET_CHANGED_SUB,
            &walletevents::CHANGED,
            move |delivery, e: walletevents::Changed| {
                projection::on_wallet_changed(credit_svc.clone(), delivery, e)
            },
        );

        let promoted_svc = svc.clone();
        ctx.bus().on_tx(
            projection::PLAYER_PROMOTED_SUB,
            &accountsevents::PLAYER_PROMOTED,
            move |delivery, e: accountsevents::PlayerPromoted| {
                projection::on_player_promoted(promoted_svc.clone(), delivery, e)
            },
        );

        let requested_svc = svc.clone();
        ctx.bus().on_tx(
            projection::FRIEND_REQUESTED_SUB,
            &friendsevents::REQUESTED,
            move |delivery, e: friendsevents::Requested| {
                projection::on_friend_requested(requested_svc.clone(), delivery, e)
            },
        );

        let accepted_svc = svc.clone();
        ctx.bus().on_tx(
            projection::FRIEND_ACCEPTED_SUB,
            &friendsevents::ACCEPTED,
            move |delivery, e: friendsevents::Accepted| {
                projection::on_friend_accepted(accepted_svc.clone(), delivery, e)
            },
        );

        let prune: Arc<dyn bus::TxHandler> = Arc::new(projection::PruneHandler { retention_days });
        ctx.bus()
            .on_tx_raw(projection::PRUNE_SUB, schedulerevents::FIRED.topic(), prune);

        for op in notificationsapi::player_rpc::operations(svc.clone()) {
            ctx.contribute(opsapi::SLOT, op.operation);
            ctx.contribute(opsapi::BINDING_SLOT, op.binding);
            ctx.contribute(opsapi::LOCAL_SLOT, op.local);
        }

        // The local "Inbox" page. The `RenderFn` is synchronous; `admin::admin_render` bridges
        // to the async store reads via `block_in_place`. The extension entries ride the item
        // as pure data — the same vec `admin_data` returns REMOTE.
        let render_svc = svc.clone();
        ctx.contribute(
            adminapi::SLOT,
            adminapi::Item::local(
                admin::ADMIN_ITEM_ID,
                admin::ADMIN_SECTION,
                admin::ADMIN_LABEL,
                Arc::new(move |params: &adminapi::Params| admin::admin_render(&render_svc, params)),
            )
            .with_extensions(admin::extension_entries()),
        );

        // Contributed UNCONDITIONALLY — topology-blind: `app::run` applies it iff this
        // process serves an internal edge; in the monolith it is never applied.
        ctx.contribute(
            edge::EDGE_SLOT,
            edge::EdgeReg::new(move |server| {
                notificationsrpc::player_rpc::register_server(server, svc.clone());
                // The admin fan-out READ face and, ALONGSIDE it, the opt-in WRITE face — both
                // through this module's OWN glue crate's re-exports. The write face is what
                // makes the Inbox page editable from a REMOTE admin process.
                notificationsrpc::register_admin(server, svc.clone());
                notificationsrpc::register_admin_submit(server, svc.clone());
            }),
        );

        ctx.contribute(
            opsapi::DESCRIBE_SLOT,
            notificationsrpc::player_rpc::describe(),
        );
        Ok(())
    }
}
