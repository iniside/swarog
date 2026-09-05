//! `friends` — the social graph. One row per player PAIR, canonically ordered
//! (`low_id < high_id`), so symmetry and pair-uniqueness are database facts: either player
//! addresses the SAME row, and the unique index — not a read-then-write — is what makes a
//! crossing pair of requests one relation.
//!
//! `requester_id` is separate because the pair is symmetric while the TRANSITIONS are not:
//! only the party who did not author a pending request may answer it. Which side of the pair
//! that is depends on uuid ordering, so every consent predicate tests `requester_id`, never
//! `high_id`.
//!
//! The domain write and its durable event append commit in ONE transaction — the event is
//! durable iff the relation change is.

mod admin;
mod service;
mod store;
#[cfg(test)]
mod tests;

pub use service::Service;

use std::sync::{Arc, OnceLock};

use accountsapi::Directory;
use async_trait::async_trait;
use friendsapi::Player;
use lifecycle::{Context, Module};
use registry::key;

/// `CHECK (low_id < high_id)` plus `friends_pair_idx` are the pair authority: the ordered
/// pair is computed in SQL (`least`/`greatest`) by every statement, so a caller's spelling of
/// an id can never decide which column it lands in.
///
/// The two side indexes serve the two branches of the paged UNION ALL; a single `low_id = $1
/// OR high_id = $1` is a BitmapOr plus a sort that `LIMIT n+1` does not bound.
///
/// Plain `uuid` columns, no cross-module FK (constraint #10).
const SCHEMA_DDL: &str = r#"
CREATE SCHEMA IF NOT EXISTS friends;
CREATE TABLE IF NOT EXISTS friends.edges (
    id           uuid PRIMARY KEY,
    low_id       uuid NOT NULL,
    high_id      uuid NOT NULL,
    requester_id uuid NOT NULL,
    state        text NOT NULL,
    created_at   timestamptz NOT NULL DEFAULT now(),
    accepted_at  timestamptz,
    CONSTRAINT friends_pair_ordered CHECK (low_id < high_id),
    CONSTRAINT friends_state_check  CHECK (state IN ('pending','accepted'))
);
CREATE UNIQUE INDEX IF NOT EXISTS friends_pair_idx ON friends.edges (low_id, high_id);
CREATE INDEX IF NOT EXISTS friends_low_idx  ON friends.edges (low_id,  state, created_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS friends_high_idx ON friends.edges (high_id, state, created_at DESC, id DESC);"#;

pub(crate) fn internal<E: std::fmt::Display>(e: E) -> opsapi::Error {
    opsapi::Error::internal(e.to_string())
}

pub struct Friends {
    svc: OnceLock<Arc<Service>>,
}

impl Default for Friends {
    fn default() -> Self {
        Friends::new()
    }
}

impl Friends {
    pub fn new() -> Friends {
        Friends {
            svc: OnceLock::new(),
        }
    }

    fn svc(&self) -> Arc<Service> {
        self.svc
            .get()
            .expect("friends.register must run before init/migrate")
            .clone()
    }
}

#[async_trait]
impl Module for Friends {
    fn name(&self) -> &str {
        "friends"
    }

    /// `accounts` is a hard sync dependency: every op resolves an invite target or hydrates
    /// a page through `accountsapi::Directory`. A process hosting friends without the
    /// accounts capability FAILS STARTUP (`app::validate_requires`).
    fn requires(&self) -> Vec<String> {
        vec!["accounts".into()]
    }

    fn register(&self, ctx: &Context) -> anyhow::Result<()> {
        let pool = ctx
            .db()
            .ok_or_else(|| anyhow::anyhow!("friends requires a DB pool"))?
            .clone();
        let svc = Arc::new(Service::new(pool, ctx.bus().clone()));
        self.svc
            .set(svc.clone())
            .map_err(|_| anyhow::anyhow!("friends.register ran twice"))?;

        ctx.registry()
            .provide::<dyn Player>(key("friends", "player"), svc);
        Ok(())
    }

    async fn migrate(&self, ctx: &Context) -> anyhow::Result<()> {
        let pool = ctx
            .db()
            .ok_or_else(|| anyhow::anyhow!("friends requires a DB pool"))?;
        sqlx::raw_sql(SCHEMA_DDL).execute(pool).await?;
        Ok(())
    }

    fn init(&self, ctx: &Context) -> anyhow::Result<()> {
        let svc = self.svc();

        // Phase 2: in the split a `remote::Stub` swaps an edge-backed client under the SAME
        // key, so this line is topology-blind.
        let directory = ctx
            .registry()
            .require::<dyn Directory>(&key("accounts", "directory"));
        let _ = svc.directory.set(directory);

        for op in friendsapi::player_rpc::operations(svc.clone()) {
            ctx.contribute(opsapi::SLOT, op.operation);
            ctx.contribute(opsapi::BINDING_SLOT, op.binding);
            ctx.contribute(opsapi::LOCAL_SLOT, op.local);
        }

        // Contributed UNCONDITIONALLY — topology-blind: `app::run` applies it iff this
        // process serves an internal edge.
        ctx.contribute(
            edge::EDGE_SLOT,
            edge::EdgeReg::new(move |server| {
                friendsrpc::player_rpc::register_server(server, svc.clone());
                // The read-only admin page over the edge, through friends' OWN glue crate's
                // re-export — archcheck forbids a module→foreign-rpc edge. No
                // `register_admin_submit`: the page has no write surface.
                friendsrpc::register_admin(server, svc.clone());
            }),
        );

        ctx.contribute(opsapi::DESCRIBE_SLOT, friendsrpc::player_rpc::describe());

        // The local admin page. `RenderFn` is synchronous while the store reads are not;
        // the closure bridges via `block_in_place` on the multi-thread runtime.
        let render_svc = self.svc();
        ctx.contribute(
            adminapi::SLOT,
            adminapi::Item::local(
                admin::ADMIN_ITEM_ID,
                admin::ADMIN_SECTION,
                admin::ADMIN_LABEL,
                Arc::new(move |params: &adminapi::Params| {
                    admin::admin_render(&render_svc, params)
                }),
            ),
        );
        Ok(())
    }
}
