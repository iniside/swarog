//! `storage` — player cloud-save: opaque, caller-authored JSON documents addressed by
//! `(owner_id, collection, key)` and written under optimistic concurrency control.
//!
//! The value is opaque to the server, so nothing downstream can project it and this
//! module publishes no durable event. It consumes no other module's capability either:
//! the owner id is the caller's own identity, resolved at the front door.

mod admin;
pub mod conformance;
mod projection;
mod service;
mod store;

#[cfg(test)]
mod service_tests;
#[cfg(test)]
mod store_tests;

pub use service::Service;

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use lifecycle::{Context, Module};
use registry::key;
use storageapi::Player;

pub(crate) fn internal<E: std::fmt::Display>(e: E) -> opsapi::Error {
    opsapi::Error::internal(e.to_string())
}

pub struct Storage {
    svc: OnceLock<Arc<Service>>,
}

impl Default for Storage {
    fn default() -> Self {
        Storage::new()
    }
}

impl Storage {
    pub fn new() -> Storage {
        Storage {
            svc: OnceLock::new(),
        }
    }

    fn svc(&self) -> Arc<Service> {
        self.svc
            .get()
            .expect("storage.register must run before init/migrate")
            .clone()
    }
}

#[async_trait]
impl Module for Storage {
    fn name(&self) -> &str {
        "storage"
    }

    fn register(&self, ctx: &Context) -> anyhow::Result<()> {
        let pool = ctx
            .db()
            .ok_or_else(|| anyhow::anyhow!("storage requires a DB pool"))?
            .clone();
        let svc = Arc::new(Service::new(pool));
        self.svc
            .set(svc.clone())
            .map_err(|_| anyhow::anyhow!("storage.register ran twice"))?;

        ctx.registry()
            .provide::<dyn Player>(key("storage", "player"), svc);
        Ok(())
    }

    async fn migrate(&self, _ctx: &Context) -> anyhow::Result<()> {
        self.svc().store.migrate().await
    }

    fn init(&self, ctx: &Context) -> anyhow::Result<()> {
        let svc = self.svc();

        for op in storageapi::player_rpc::operations(svc.clone()) {
            ctx.contribute(opsapi::SLOT, op.operation);
            ctx.contribute(opsapi::BINDING_SLOT, op.binding);
            ctx.contribute(opsapi::LOCAL_SLOT, op.local);
        }

        // Contributed UNCONDITIONALLY — topology-blind: `app::run` applies it iff this
        // process serves an internal edge.
        ctx.contribute(
            edge::EDGE_SLOT,
            edge::EdgeReg::new(move |server| {
                storagerpc::player_rpc::register_server(server, svc.clone());
                // The read-only admin page over the edge, through storage's OWN glue crate's
                // re-export — archcheck forbids a module→foreign-rpc edge. No
                // `register_admin_submit`: the page has no write surface.
                storagerpc::register_admin(server, svc.clone());
            }),
        );

        ctx.contribute(
            opsapi::DESCRIBE_SLOT,
            storagerpc::player_rpc::describe(),
        );

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

        // The tombstone, planted on the HANDED delivery conn so it commits with the
        // subscription checkpoint, and doing no other work: an owner holds up to
        // `MAX_OBJECTS_PER_OWNER` objects, while `ASYNCEVENTS_HANDLER_TIMEOUT` bounds this
        // handler at 10s. The deleting is the scheduled sweep's, which resumes across fires;
        // this row is that sweep's work item, the guard `Service::create` reads, and, once
        // swept, the permanent record that the player is gone — redelivery is
        // `ON CONFLICT DO NOTHING`, never a reset of `swept_at`.
        let purger = self.svc();
        ctx.bus().on_tx(
            projection::PLAYER_DELETED_SUB,
            &accountsevents::PLAYER_DELETED,
            move |mut delivery, e: accountsevents::PlayerDeleted| {
                let purger = purger.clone();
                Box::pin(async move {
                    let conn = delivery.tx.downcast::<sqlx::PgConnection>()?;
                    purger
                        .store
                        .plant_purged_player_tx(conn, &e.player_id, &e.handle)
                        .await
                        .map_err(bus::Error::transport)
                })
            },
        );

        // The drain of what that tombstone records, on its OWN subscription: a raw sink on
        // `scheduler.fired` guarding on the shared daily `player-purge` name.
        let sweeper: Arc<dyn bus::TxHandler> = Arc::new(projection::PurgeSweepHandler {
            svc: self.svc(),
        });
        ctx.bus().on_tx_raw(
            projection::PURGE_SWEEP_SUB,
            schedulerevents::FIRED.topic(),
            sweeper,
        );
        Ok(())
    }
}
