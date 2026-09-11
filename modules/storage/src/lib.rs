//! `storage` — player cloud-save: opaque, caller-authored JSON documents addressed by
//! `(owner_id, collection, key)` and written under optimistic concurrency control.
//!
//! The value is opaque to the server, so nothing downstream can project it and this
//! module publishes no durable event. It consumes no other module's capability either:
//! the owner id is the caller's own identity, resolved at the front door.

pub mod conformance;
mod service;
mod store;

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
            }),
        );

        ctx.contribute(
            opsapi::DESCRIBE_SLOT,
            storagerpc::player_rpc::describe(),
        );
        Ok(())
    }
}
