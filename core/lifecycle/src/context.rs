use std::any::Any;
use std::sync::{Arc, Mutex};

use axum::Router;
use bus::Bus;
use contrib::Slots;
use registry::Registry;
use sqlx::PgPool;

/// The slice of the core handed to each module. It exposes only primitives: the
/// event bus, the service registry, the contribution slots, an HTTP route handle,
/// the shared DB pool and (via `tracing`) logging.
///
/// Everything is behind `Arc`/`Mutex` so the SAME `&Context` can be shared across
/// every module's `register`/`init` (which take `&self`), matching Go's shared
/// `*Context` pointer. A module clones the `Arc<Bus>`/`Arc<Registry>` handles it
/// needs onto itself during wiring for later use in `start`.
pub struct Context {
    bus: Arc<Bus>,
    registry: Arc<Registry>,
    slots: Arc<Slots>,
    /// The accumulating axum router. **Choice:** a `Mutex<Router>` that modules
    /// merge into (over a route-contribution `contrib` slot) because axum's own
    /// `Router::merge` already IS the compose-many-into-one primitive — mounting a
    /// module's sub-router is one `merge`, and the app runner (Step 3) takes the
    /// finished `Router` to serve. A slot of boxed route closures would re-invent
    /// merge with less type safety.
    router: Mutex<Router>,
    /// The shared Postgres pool. OFFERED, not mandated: a module may use it (owning
    /// its own schema) or ignore it. `None` in unit tests — the lifecycle core
    /// never requires a live DB.
    db: Option<PgPool>,
}

impl Context {
    /// A DB-less context (unit tests, or a process with no persistence).
    pub fn new() -> Self {
        Context {
            bus: Arc::new(Bus::new()),
            registry: Arc::new(Registry::new()),
            slots: Arc::new(Slots::new()),
            router: Mutex::new(Router::new()),
            db: None,
        }
    }

    /// A context backed by a live pool.
    pub fn with_db(db: PgPool) -> Self {
        Context {
            db: Some(db),
            ..Context::new()
        }
    }

    /// A context backed by a live pool AND a durable-events transport, injected
    /// at construction so every module's `on_tx`/`emit_tx` finds the plane live
    /// (there is no later installer). This is the shape `app::run` builds for
    /// every DB-backed process (DB ⇒ plane); [`Context::new`]/[`Context::with_db`]
    /// stay plane-less for unit tests and DB-less processes.
    pub fn with_db_and_transport(db: PgPool, transport: Arc<dyn bus::Transport>) -> Self {
        Context {
            bus: Arc::new(Bus::with_transport(transport)),
            db: Some(db),
            ..Context::new()
        }
    }

    pub fn bus(&self) -> &Arc<Bus> {
        &self.bus
    }

    pub fn registry(&self) -> &Arc<Registry> {
        &self.registry
    }

    pub fn slots(&self) -> &Arc<Slots> {
        &self.slots
    }

    /// The shared pool, or `None` if this process has no persistence.
    pub fn db(&self) -> Option<&PgPool> {
        self.db.as_ref()
    }

    /// Forwards to the slot registry: adds a value to a named slot. Unlike a
    /// service (one per name), a slot collects MANY contributors.
    pub fn contribute<T: Any + Send + Sync>(&self, slot: impl Into<String>, v: T) {
        self.slots.contribute(slot, v);
    }

    /// Forwards to the slot registry: everything contributed to a slot, downcast
    /// to `T`, in registration order. Read lazily, after all modules have wired up.
    pub fn contributions<T: Clone + 'static>(&self, slot: &str) -> Vec<T> {
        self.slots.contributions(slot)
    }

    /// Merges a module's routes into the shared router. `Router::merge` consumes
    /// both sides, so we take-and-replace under the lock.
    pub fn mount(&self, routes: Router) {
        let mut guard = self.router.lock().unwrap();
        let current = std::mem::take(&mut *guard);
        *guard = current.merge(routes);
    }

    /// Takes the accumulated router for serving (leaves an empty one behind). The
    /// app runner (Step 3) calls this once, after `build`.
    pub fn take_router(&self) -> Router {
        std::mem::take(&mut *self.router.lock().unwrap())
    }
}

impl Default for Context {
    fn default() -> Self {
        Context::new()
    }
}
