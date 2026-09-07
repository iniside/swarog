//! `groups` — social groups: one row per group, one per membership, two roles and three
//! membership states.
//!
//! The composite primary key `(group_id, player_id)` is what makes a DOUBLE JOIN one row —
//! that case alone. Every other decision is a read-then-write UNDER the group's advisory
//! lock: the ops that decide from a count (`join`, `invite`, `leave`) and the ones that
//! decide from the subject's state (`respond`, `decide`) all take it before their read,
//! because a concurrent writer would invalidate what they read. `create` takes no lock,
//! and that is not an omission: the group id it mints is unreachable by any other caller
//! until the transaction commits, so there is nothing to serialize against.
//!
//! The domain write and its durable event append commit in ONE transaction — the event is
//! durable iff the membership change is.

mod projection;
mod service;
mod store;

pub use service::Service;

use std::sync::{Arc, OnceLock};

use accountsapi::Directory;
use async_trait::async_trait;
use groupsapi::{Membership, Player};
use lifecycle::{Context, Module};
use registry::key;

/// `memberships_role_check` is an EQUIVALENCE, not an implication: a `member` row must
/// carry a role AND a non-`member` row must not. The forward half alone would admit
/// `state='member' AND role=''`, so an accept that updated the state and forgot the role
/// would commit and leave a real member carrying no role at all.
///
/// `memberships_pending_idx` is partial and serves no reader yet: it exists for the
/// retention sweep that plan Step 5 adds, which would otherwise seq-scan a table whose
/// live rows dominate.
///
/// Plain `uuid` columns, no cross-module FK (constraint #10).
const SCHEMA_DDL: &str = r#"
CREATE SCHEMA IF NOT EXISTS groups;
CREATE TABLE IF NOT EXISTS groups.groups (
    id           uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    name         text        NOT NULL,
    join_policy  text        NOT NULL,
    creator_id   uuid        NOT NULL,
    created_at   timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT groups_name_len_check    CHECK (octet_length(name) <= 64),
    CONSTRAINT groups_join_policy_check CHECK (join_policy IN ('open','request','invite'))
);
CREATE TABLE IF NOT EXISTS groups.memberships (
    group_id   uuid        NOT NULL,
    player_id  uuid        NOT NULL,
    state      text        NOT NULL,
    role       text        NOT NULL DEFAULT '',
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (group_id, player_id),
    CONSTRAINT memberships_state_check CHECK (state IN ('member','invited','requested')),
    CONSTRAINT memberships_role_check
        CHECK ((state = 'member') = (role <> '')
               AND (role = '' OR role IN ('admin','member')))
);
CREATE INDEX IF NOT EXISTS memberships_player_idx
    ON groups.memberships (player_id, state, created_at DESC, group_id DESC);
CREATE INDEX IF NOT EXISTS memberships_group_idx
    ON groups.memberships (group_id, state, created_at DESC, player_id DESC);
CREATE INDEX IF NOT EXISTS memberships_pending_idx
    ON groups.memberships (created_at) WHERE state <> 'member';"#;

pub(crate) fn internal<E: std::fmt::Display>(e: E) -> opsapi::Error {
    opsapi::Error::internal(e.to_string())
}

pub struct Groups {
    svc: OnceLock<Arc<Service>>,
}

impl Default for Groups {
    fn default() -> Self {
        Groups::new()
    }
}

impl Groups {
    pub fn new() -> Groups {
        Groups {
            svc: OnceLock::new(),
        }
    }

    fn svc(&self) -> Arc<Service> {
        self.svc
            .get()
            .expect("groups.register must run before init/migrate")
            .clone()
    }
}

#[async_trait]
impl Module for Groups {
    fn name(&self) -> &str {
        "groups"
    }

    /// `accounts` is a hard sync dependency: `invite` resolves its target through
    /// `accountsapi::Directory` and every page hydrates its handles through it. A process
    /// hosting groups without the accounts capability FAILS STARTUP
    /// (`app::validate_requires`).
    fn requires(&self) -> Vec<String> {
        vec!["accounts".into()]
    }

    fn register(&self, ctx: &Context) -> anyhow::Result<()> {
        let pool = ctx
            .db()
            .ok_or_else(|| anyhow::anyhow!("groups requires a DB pool"))?
            .clone();
        let svc = Arc::new(Service::new(pool, ctx.bus().clone()));
        self.svc
            .set(svc.clone())
            .map_err(|_| anyhow::anyhow!("groups.register ran twice"))?;

        ctx.registry()
            .provide::<dyn Membership>(key("groups", "membership"), svc.clone());
        ctx.registry()
            .provide::<dyn Player>(key("groups", "player"), svc);
        Ok(())
    }

    async fn migrate(&self, ctx: &Context) -> anyhow::Result<()> {
        let pool = ctx
            .db()
            .ok_or_else(|| anyhow::anyhow!("groups requires a DB pool"))?;
        sqlx::raw_sql(SCHEMA_DDL).execute(pool).await?;
        Ok(())
    }

    fn init(&self, ctx: &Context) -> anyhow::Result<()> {
        let retention_days = projection::retention_days_from_env()?;
        let svc = self.svc();

        let prune: Arc<dyn bus::TxHandler> = Arc::new(projection::PruneHandler { retention_days });
        ctx.bus()
            .on_tx_raw(projection::PRUNE_SUB, schedulerevents::FIRED.topic(), prune);

        // Phase 2: in the split a `remote::Stub` swaps an edge-backed client under the
        // SAME key, so this line is topology-blind.
        let directory = ctx
            .registry()
            .require::<dyn Directory>(&key("accounts", "directory"));
        let _ = svc.directory.set(directory);

        for op in groupsapi::player_rpc::operations(svc.clone()) {
            ctx.contribute(opsapi::SLOT, op.operation);
            ctx.contribute(opsapi::BINDING_SLOT, op.binding);
            ctx.contribute(opsapi::LOCAL_SLOT, op.local);
        }

        // Contributed UNCONDITIONALLY — topology-blind: `app::run` applies it iff this
        // process serves an internal edge.
        ctx.contribute(
            edge::EDGE_SLOT,
            edge::EdgeReg::new(move |server| {
                groupsrpc::player_rpc::register_server(server, svc.clone());
                // The wire-only roster predicate: reachable ONLY here, on the internal
                // mTLS edge, never from the front door.
                groupsrpc::membership_rpc::register_server(server, svc.clone());
            }),
        );

        // `membership_rpc` is wire-only, so its `describe()` is empty; concatenated anyway
        // so a future `#[http]` op on either contract flows through with no edit here.
        ctx.contribute(
            opsapi::DESCRIBE_SLOT,
            opsapi::DescribeManifest::concat([
                groupsrpc::player_rpc::describe(),
                groupsrpc::membership_rpc::describe(),
            ]),
        );
        Ok(())
    }
}
