//! `checkmodules` — the single source for the monolith module set both `topiccheck`
//! and `requirecheck` build to observe real subscribe/require call sites (Step 3,
//! abstraction-leak closures). Before this crate, the identical 12-entry vec was
//! hand-duplicated in both tools' `main.rs`, a rot vector: a 13th module added to
//! `cmd/server` but missed in either copy would silently drop out of that harness's
//! coverage and still report a clean PASS.

use lifecycle::Module;

/// The module set both checker harnesses run. MUST track `cmd/server`'s list
/// (minus core-infra `metrics` and the `demos/webui` demo, which have no
/// `requires()`, no topics, and no schema — they add nothing to either harness;
/// archcheck also forbids any non-`cmd/server` consumer of a `demos/*` crate).
/// When adding a module: update `cmd/server`, the new svc main, `split-proof`,
/// AND this list.
pub fn monolith_modules() -> Vec<Box<dyn Module>> {
    vec![
        Box::new(config::Config::new()),
        Box::new(characters::Characters::new()),
        Box::new(inventory::Inventory::new()),
        Box::new(accounts::Accounts::new()),
        Box::new(admin::Admin::new()),
        Box::new(audit::Audit::new()),
        Box::new(scheduler::Scheduler::new()),
        Box::new(rating::Rating::new()),
        Box::new(match_module::MatchModule::new()),
        Box::new(leaderboard::LeaderboardModule::new()),
        Box::new(apikeys::ApiKeys::new()),
        Box::new(gateway::Gateway::new()),
    ]
}
