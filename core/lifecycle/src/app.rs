use std::collections::HashSet;
use std::sync::Arc;

use anyhow::Context as _;

use crate::{Context, Module};

/// Session-level advisory-lock key serializing concurrent `App::migrate` runs
/// across replicas/processes (parallel test binaries, split-process boots) against
/// one shared DB: idempotent module DDL racing itself can still deadlock on catalog
/// locks or fail `CREATE OR REPLACE` with "tuple concurrently updated". ASCII
/// `"lifemigg"` as a positive i64 — distinct from asyncevents' plane migrate lock
/// (`0x6173_796E_636D_6967`, "asyncmig") so the two planes never contend.
const MODULE_MIGRATE_LOCK_KEY: i64 = 0x6C69_6665_6D69_6767;

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
    ///
    /// On a DB-backed process the whole module loop runs under a session-level
    /// advisory lock ([`MODULE_MIGRATE_LOCK_KEY`]) so concurrent replica/process
    /// boots serialize their idempotent DDL instead of racing it (catalog-lock
    /// deadlocks / "tuple concurrently updated"). A DB-less process has no DDL to
    /// run and loops unlocked.
    pub async fn migrate(&self) -> anyhow::Result<()> {
        let Some(pool) = self.ctx.db() else {
            // DB-less process: nothing persists, so there is no DDL to serialize.
            return self.run_migrations().await;
        };

        // Hold the lock on a DEDICATED connection for the entire loop. A session
        // lock (`pg_advisory_lock`, not `_xact`) because the loop spans many
        // independent per-module transactions, not one. INVARIANT: this connection
        // is held while every module's `migrate` acquires FURTHER pool connections,
        // so the pool max MUST be >= 2 during migrate or the process self-deadlocks
        // (the default pool size is comfortably above 2).
        let mut lock_conn = pool
            .acquire()
            .await
            .context("acquire module-migrate lock connection")?;
        sqlx::query("SELECT pg_advisory_lock($1)")
            .bind(MODULE_MIGRATE_LOCK_KEY)
            .execute(&mut *lock_conn)
            .await
            .context("acquire module-migrate advisory lock")?;

        // Run the loop, capture its Result, then ALWAYS unlock on the same
        // connection — success and error alike — before propagating.
        let loop_result = self.run_migrations().await;
        let unlock_result = sqlx::query("SELECT pg_advisory_unlock($1)")
            .bind(MODULE_MIGRATE_LOCK_KEY)
            .execute(&mut *lock_conn)
            .await;
        drop(lock_conn); // return the connection to the pool after unlock

        loop_result?;
        unlock_result.context("release module-migrate advisory lock")?;
        Ok(())
    }

    /// The bare module `migrate` loop, in registration order. Wrapped by
    /// [`App::migrate`] under the advisory lock on a DB-backed process.
    async fn run_migrations(&self) -> anyhow::Result<()> {
        for m in &self.modules {
            m.migrate(&self.ctx)
                .await
                .with_context(|| format!("migrate {:?}", m.name()))?;
            tracing::info!(module = m.name(), "module migrated");
        }
        Ok(())
    }

    /// Runs `start` on every module, in registration order. On module N failing,
    /// the already-started prefix (modules 0..N, exclusive) is stopped in REVERSE
    /// order — best-effort, log-and-continue per module, the same policy as
    /// [`App::stop`] — and the original error is returned. Modules whose `start`
    /// never ran (the failing module itself, and everything after it) do NOT get
    /// `stop`: a module's `stop` is only ever invoked after its `start` succeeded.
    pub async fn start(&self) -> anyhow::Result<()> {
        for (i, m) in self.modules.iter().enumerate() {
            if let Err(err) = m.start(&self.ctx).await {
                tracing::error!(
                    module = m.name(),
                    %err,
                    "module start failed; stopping the started prefix"
                );
                for started in self.modules[..i].iter().rev() {
                    match started.stop(&self.ctx).await {
                        Ok(()) => tracing::info!(
                            module = started.name(),
                            "module stopped (start unwind)"
                        ),
                        Err(stop_err) => tracing::error!(
                            module = started.name(),
                            %stop_err,
                            "module stop failed during start unwind"
                        ),
                    }
                }
                return Err(err).with_context(|| format!("start {:?}", m.name()));
            }
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
