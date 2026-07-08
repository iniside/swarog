//! `configapi` — the capability trait `config` exposes to other modules (the
//! interface-crate demanded by rule 4). Go's `config` consumers `Require` the
//! `"config"` service and downcast it to their OWN 1-method structural interface;
//! Rust's nominal traits can't do that, so the provider and every consumer share
//! THIS trait. Inventory (Step 9) will
//! `require::<dyn Config>(&registry::key("config", "reader"))`.
//!
//! Note the split: only the READ subset lives here. `set` stays on the concrete
//! `config` service (its own admin uses it), NOT on this trait — a reader depends
//! on a capability it needs (getters), nothing more.

/// The read-mostly config capability: namespaced `key=value` getters with a
/// code-default fallback, backed by an in-memory cache kept fresh by config's
/// listener. All getters degrade to `default` on a cache miss.
pub trait Config: Send + Sync {
    /// The cached value, or `default` on a miss.
    fn get_string(&self, ns: &str, key: &str, default: &str) -> String;

    /// Truthiness mirrors the repo's `envBool` (`"1"`/`"true"`/`"on"`, case-
    /// insensitive); a miss returns `default`.
    fn get_bool(&self, ns: &str, key: &str, default: bool) -> bool;

    /// Parses the cached value as an integer; a miss OR a parse error returns
    /// `default`.
    fn get_int(&self, ns: &str, key: &str, default: i64) -> i64;

    /// The raw cache lookup: `Some(value)` when present, else `None`.
    fn get(&self, ns: &str, key: &str) -> Option<String>;
}
