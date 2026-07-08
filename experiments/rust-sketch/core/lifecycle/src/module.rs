use crate::Context;

/// The optional lifecycle phases a module opts into. Go detects an optional
/// capability at runtime with a type assertion (`m.(Registrar)`); Rust cannot
/// runtime-detect an extra trait impl on a `dyn Module`, so a module declares its
/// phases explicitly via [`Module::caps`]. `App::build`/`migrate`/`start`/`stop`
/// invoke a phase ONLY when its flag is set — so a plain module (just `init`) is
/// never asked to migrate, and the DB-less unit tests never touch a pool.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Caps(u8);

impl Caps {
    pub const NONE: Caps = Caps(0);
    pub const REGISTER: Caps = Caps(1 << 0);
    pub const MIGRATE: Caps = Caps(1 << 1);
    pub const START: Caps = Caps(1 << 2);
    pub const STOP: Caps = Caps(1 << 3);

    /// True when every flag in `other` is present in `self`.
    pub fn contains(self, other: Caps) -> bool {
        self.0 & other.0 == other.0
    }
}

impl std::ops::BitOr for Caps {
    type Output = Caps;
    fn bitor(self, rhs: Caps) -> Caps {
        Caps(self.0 | rhs.0)
    }
}

/// The contract every module implements. The foundations NEVER import a module;
/// modules import them. Dependency points one way only.
///
/// Phase discipline (mirrors the Go docs):
///   - `register` (phase 1) — construct and `provide` services. Runs before ANY
///     `init`, so every service exists by the time inits run. Opt in via
///     [`Caps::REGISTER`].
///   - `init` (phase 2) — only WIRE UP: subscribe to the bus, mount routes,
///     `require` services, contribute admin items. No I/O, no background work.
///   - `migrate` — create/upgrade this module's OWN schema, idempotent
///     (`CREATE ... IF NOT EXISTS`). Opt in via [`Caps::MIGRATE`].
///   - `start` — background work (tickers, workers), after every module's `init`,
///     in registration order. Opt in via [`Caps::START`].
///   - `stop` — release resources, in REVERSE registration order. Opt in via
///     [`Caps::STOP`]. Don't emit events here — the bus has already drained.
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

    /// Which optional phases this module opts into. Default: none.
    fn caps(&self) -> Caps {
        Caps::NONE
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
