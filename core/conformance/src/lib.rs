//! Contract types for the convention-conformance harness (`tools/conformance`,
//! package `conformancecheck`).
//!
//! The harness enforces cross-module *conventions* — behaviors that must be
//! hardened the same way in every fortress (env validation, input byte caps,
//! infra-outage classification, argon2 parity). Per-module tests can't catch a
//! twin module that was skipped: a missing test in module Y looks identical to
//! "the convention doesn't apply to Y". This crate makes that distinction
//! explicit and mandatory.
//!
//! # The harness contract
//!
//! - **Silence = fail.** Every module in `modules/*` must expose
//!   `pub mod conformance { pub fn entry() -> conformance::Entry }` and appear
//!   in the harness's `entries()` list; a drift preflight diffs that list
//!   against the modules on disk and the monolith module set, failing per-entry
//!   on any mismatch.
//! - **Every module declares a stance for every [`Convention::ALL`].** An entry
//!   missing a stance for any convention fails the completeness matrix.
//! - **[`Stance::NotApplicable`] requires a non-empty `why`** — a sentence a
//!   reviewer can check ("no env parsed at init beyond dev-gates, which fail
//!   closed by absence"), not "n/a". An empty `why` fails.
//!
//! This crate is std-only by design: modules import it as a plain dependency
//! (the `asyncevents::testing` precedent — always compiled, no feature flags),
//! so it must stay free of tokio/sqlx/futures. [`BoxFuture`] is hand-rolled for
//! the same reason.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

/// Hand-rolled boxed future so fixture probes can be async without this crate
/// depending on `futures`.
pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// The conventions the harness enforces. Adding a variant here forces every
/// module's entry to declare a stance for it (completeness matrix) — remember
/// to extend [`Convention::ALL`] (the exhaustiveness test in `tests.rs` reminds
/// you at compile time).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Convention {
    /// T6: a bad env value must fail a fresh full `App::build()` with an error
    /// naming the variable — never silently default.
    EnvValidation,
    /// T8: player-supplied input fields carry an enforced byte cap.
    InputByteCaps,
    /// T7: an infrastructure outage in a verifier classifies as unavailable
    /// (503-class), never as a rejection (401-class).
    InfraOutage503,
    /// T2: every argon2 password hasher in the tree uses identical parameters.
    ArgonParity,
}

impl Convention {
    /// Every convention, in matrix order. The harness iterates this to demand
    /// a stance per module; `tests.rs` proves it stays exhaustive.
    pub const ALL: [Convention; 4] = [
        Convention::EnvValidation,
        Convention::InputByteCaps,
        Convention::InfraOutage503,
        Convention::ArgonParity,
    ];
}

/// One module's declared stances, exposed via its `conformance::entry()`.
///
/// `module` must equal the module's `Module::name()` / `modules/<name>`
/// directory name (note the `match` trap: crate `match_module`, module name
/// `"match"`) — it is the drift-diff key.
#[derive(Clone)]
pub struct Entry {
    pub module: &'static str,
    pub stances: Vec<(Convention, Stance)>,
}

impl Entry {
    /// The stance this module declared for `c`, if any. `None` means the
    /// completeness matrix fails — silence is not a stance.
    pub fn stance(&self, c: Convention) -> Option<&Stance> {
        self.stances
            .iter()
            .find(|(convention, _)| *convention == c)
            .map(|(_, stance)| stance)
    }
}

/// A module's explicit position on one convention.
#[derive(Clone)]
pub enum Stance {
    /// The convention applies — here is the fixture the harness executes.
    Applies(Fixture),
    /// The convention does not apply. `why` must be a non-empty, reviewer-
    /// checkable sentence; the harness fails an empty one.
    NotApplicable { why: &'static str },
}

/// The per-convention fixture payloads. Each variant matches one
/// [`Convention`]; probes are `Arc`ed closures so fixtures stay `Clone`.
#[derive(Clone)]
pub enum Fixture {
    /// T6: for each case the harness sets `var=bad_value` and expects a fresh
    /// full `App::build()` to return an `Err` whose message chain names `var`.
    EnvValidation(Vec<EnvCase>),
    /// T8: `probe(len)` returns `true` iff input of `len` bytes is REJECTED;
    /// the harness asserts `!probe(cap) && probe(cap + 1)`.
    InputByteCaps(Vec<CapCase>),
    /// T7: `probe()` runs the module's verifier against an always-failing
    /// dependency; it must resolve to [`OutageClass::Unavailable`]
    /// (503-class), never [`OutageClass::Rejected`] (401-class).
    InfraOutage503(Vec<OutageCase>),
    /// T2: the harness collects every `Applies` and asserts pairwise equality
    /// of the parameters.
    ArgonParity(ArgonParams),
}

/// One env-validation case: setting `var=bad_value` must fail startup with an
/// error naming `var`. `bad_value` must be a value the module actually parses
/// and rejects (not one it silently defaults away).
#[derive(Clone, Copy, Debug)]
pub struct EnvCase {
    pub var: &'static str,
    pub bad_value: &'static str,
}

/// One byte-cap case: `probe(len)` returns `true` iff a `len`-byte input is
/// rejected by the module's shared validation fn (the same fn its handlers
/// call — never a tautology comparing a const to itself).
#[derive(Clone)]
pub struct CapCase {
    pub name: &'static str,
    pub cap: usize,
    pub probe: Arc<dyn Fn(usize) -> bool + Send + Sync>,
}

/// One infra-outage case: `probe()` exercises the module's verifier with a
/// failing dependency and reports how the failure was classified.
#[derive(Clone)]
pub struct OutageCase {
    pub name: &'static str,
    pub probe: Arc<dyn Fn() -> BoxFuture<OutageClass> + Send + Sync>,
}

/// How a verifier classified an infrastructure failure.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum OutageClass {
    /// 503-class: "I can't answer right now" — the required outcome.
    Unavailable,
    /// 401-class: the outage was misreported as an auth rejection — a fail.
    Rejected,
    /// Anything else (carries a description for the report) — a fail.
    Other(String),
}

/// Argon2 parameters for the T2 parity check.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ArgonParams {
    pub m_cost: u32,
    pub t_cost: u32,
    pub p_cost: u32,
    pub output_len: usize,
}

#[cfg(test)]
mod tests;
