use crate::Context;

/// The contract every module implements. The foundations NEVER import a module;
/// modules import them. Dependency points one way only.
///
/// Phase discipline (mirrors the Go docs):
///   - `register` (phase 1) — construct and `provide` services. Runs before ANY
///     `init`, so every service exists by the time inits run.
///   - `init` (phase 2) — only WIRE UP: subscribe to the bus, mount routes,
///     `require` services, contribute admin items. No I/O, no background work.
///   - `migrate` — create/upgrade this module's OWN schema, idempotent
///     (`CREATE ... IF NOT EXISTS`).
///   - `start` — background work (tickers, workers), after every module's `init`,
///     in registration order.
///   - `stop` — release resources, in REVERSE registration order. Don't emit
///     events here — the bus has already drained. Guarantee: `stop` is only ever
///     invoked on a module whose `start` completed successfully — when a startup
///     fails partway, `App::start` stops the already-started prefix (in reverse)
///     and modules whose `start` never ran are NOT stopped.
///
/// Every phase is invoked unconditionally for every module; the default no-op
/// impls below make a phase a no-op for modules that don't need it (e.g. a
/// plain module with only `init` still gets `migrate`/`start`/`stop` called,
/// but they do nothing).
///
/// `register`/`init` are synchronous (wiring only). `migrate`/`start`/`stop` are
/// async (I/O) and reach the DB pool through the [`Context`] — Go passes the pool
/// to `Migrate` as a param; here it travels inside `Context` (`ctx.db()`) so the
/// DB-less lifecycle unit tests can still exercise phase ordering.
#[async_trait::async_trait]
pub trait Module: Send + Sync {
    fn name(&self) -> &str;

    /// The service names this module requires — a MANIFEST for `validate_requires`
    /// (Step 3), orthogonal to the derived capability keys. Does NOT order startup:
    /// with full logical isolation no `init` consumes a required service, so init
    /// order is commutative and `build` runs modules in registration order.
    fn requires(&self) -> Vec<String> {
        Vec::new()
    }

    fn register(&self, _ctx: &Context) -> anyhow::Result<()> {
        Ok(())
    }

    fn init(&self, ctx: &Context) -> anyhow::Result<()>;

    async fn migrate(&self, _ctx: &Context) -> anyhow::Result<()> {
        Ok(())
    }

    async fn start(&self, _ctx: &Context) -> anyhow::Result<()> {
        Ok(())
    }

    async fn stop(&self, _ctx: &Context) -> anyhow::Result<()> {
        Ok(())
    }
}
