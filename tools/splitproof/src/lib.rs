//! Library surface for the split-proof harness.
//!
//! Exposes exactly the two items the `harness = false` `fleet_liveness`
//! integration target needs (`Running`, `fleet_liveness`). Those tests drive
//! processctl's PRODUCTION spawn path (`OwnedChild::spawn`), which on unix
//! re-execs `current_exe --__processctl-guardian-v1`; a libtest unit binary has
//! no early guardian hook, so that re-exec lands on the harness and exits 101.
//! Living in a `harness = false` target whose `main` calls
//! `processctl::dispatch_guardian_from_current_exe()` first — exactly as the
//! production `main.rs` does — is the fix. The binary (`main.rs`) consumes these
//! same items, so there is exactly one definition of each.

use processctl::OwnedChild;

pub struct Running {
    pub name: &'static str,
    pub child: OwnedChild,
}

/// Returns one description per fleet child that is no longer alive (`try_wait`
/// returned `Some`, or the liveness probe itself errored). Shared by the `[LV1]`
/// (post-boot) and `[LV2]` (pre-teardown) liveness assertions so a service that dies
/// AFTER clearing its health gate can't silently drop out of later assertions.
pub fn fleet_liveness(fleet: &mut [Running]) -> Vec<String> {
    let mut dead = Vec::new();
    for running in fleet.iter_mut() {
        match running.child.try_wait() {
            Ok(Some(status)) => dead.push(format!("{} exited with {status}", running.name)),
            Ok(None) => {}
            Err(error) => dead.push(format!("{} liveness check failed: {error}", running.name)),
        }
    }
    dead
}
