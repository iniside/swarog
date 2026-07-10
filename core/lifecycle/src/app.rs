use std::collections::HashSet;
use std::sync::Arc;

use anyhow::Context as _;

use crate::{Context, Module};

/// Collects modules and drives their lifecycle. Modules run in REGISTRATION order
/// — there is NO topological sort: full logical isolation makes init order
/// commutative, and the two-phase [`App::build`] (register → init) guarantees
/// every provided service exists before any init requires it.
pub struct App {
    modules: Vec<Box<dyn Module>>,
    names: HashSet<String>,
    ctx: Arc<Context>,
}

impl App {
    pub fn new(ctx: Arc<Context>) -> Self {
        App {
            modules: Vec::new(),
            names: HashSet::new(),
            ctx,
        }
    }

    /// Adds a module. Panics on a duplicate name — a wiring bug, loud at startup.
    pub fn add(&mut self, module: Box<dyn Module>) {
        let name = module.name().to_string();
        if !self.names.insert(name.clone()) {
            panic!("module {name:?} registered twice");
        }
        self.modules.push(module);
    }

    /// The shared context, for the app runner (Step 3) to serve the router / pass
    /// to `migrate`.
    pub fn context(&self) -> &Arc<Context> {
        &self.ctx
    }

    /// Wires every module in two phases, both in registration order:
    ///   - phase 1 (`register`): each provider constructs and registers its
    ///     service — FIRST, so every service exists before any init runs.
    ///   - phase 2 (`init`): each module mounts routes, subscribes, contributes
    ///     items and requires the services it needs.
    ///
    /// Every phase is called unconditionally for every module — default no-op
    /// impls make a phase a no-op for modules that don't need it. A genuinely
    /// missing required service still fails loudly — the eager `require` in
    /// phase 2 panics.
    pub fn build(&self) -> anyhow::Result<()> {
        for m in &self.modules {
            m.register(&self.ctx)
                .with_context(|| format!("register {:?}", m.name()))?;
        }
        for m in &self.modules {
            m.init(&self.ctx)
                .with_context(|| format!("init {:?}", m.name()))?;
            tracing::info!(module = m.name(), "module ready");
        }
        Ok(())
    }

    /// Runs `migrate` on every module, in registration order. Call after
    /// `build`, before `start`.
    pub async fn migrate(&self) -> anyhow::Result<()> {
        for m in &self.modules {
            m.migrate(&self.ctx)
                .await
                .with_context(|| format!("migrate {:?}", m.name()))?;
            tracing::info!(module = m.name(), "module migrated");
        }
        Ok(())
    }

    /// Runs `start` on every module, in registration order. Fails fast.
    pub async fn start(&self) -> anyhow::Result<()> {
        for m in &self.modules {
            m.start(&self.ctx)
                .await
                .with_context(|| format!("start {:?}", m.name()))?;
            tracing::info!(module = m.name(), "module started");
        }
        Ok(())
    }

    /// Runs `stop` on every module, in REVERSE registration order. Best-effort:
    /// logs and continues on error so one stuck module can't strand the rest.
    pub async fn stop(&self) {
        for m in self.modules.iter().rev() {
            match m.stop(&self.ctx).await {
                Ok(()) => tracing::info!(module = m.name(), "module stopped"),
                Err(err) => {
                    tracing::error!(module = m.name(), %err, "module stop failed")
                }
            }
        }
    }
}
