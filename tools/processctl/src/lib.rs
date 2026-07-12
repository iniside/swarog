mod lock;
mod process;
mod state;

#[cfg(target_os = "linux")]
mod guardian;

#[cfg(any(windows, target_os = "linux"))]
mod platform;
#[cfg(target_os = "linux")]
mod protocol;
#[cfg(all(test, target_os = "linux"))]
mod protocol_tests;

pub use lock::{BorrowedLease, LeaseError, OwnedLease, RolloutLock, ROLLOUT_LOCK_VERSION};
pub use process::{
    observe_process_identity, OutputDestination, OwnedChild, ProcessError, ProcessGroupPolicy,
    ProcessIdentity, ShutdownOutcome, ShutdownPolicy, SpawnSpec, StartMarker,
};
pub use state::{
    FleetState, FleetStatus, ManagedProcess, ManagedStatus, StateCheckpointError, StateError,
    StateStore, STATE_VERSION,
};

#[cfg(test)]
mod lock_tests;

#[cfg(test)]
mod state_tests;

#[cfg(target_os = "linux")]
/// Dispatches the private Linux guardian mode embedded in the current executable.
///
/// A binary that uses [`OwnedChild`] must call this before constructing a Tokio
/// runtime or parsing its own CLI, and immediately return the supplied exit code.
/// The guardian is therefore always the exact consumer binary; no sibling helper
/// executable or PATH lookup is part of the process-ownership contract.
pub fn dispatch_guardian_from_current_exe() -> Option<std::process::ExitCode> {
    if std::env::args_os().nth(1).as_deref() != Some(guardian::DISPATCH_ARG.as_ref()) {
        return None;
    }
    let code = guardian::run();
    Some(std::process::ExitCode::from(
        u8::try_from(code).unwrap_or(1),
    ))
}

#[cfg(not(target_os = "linux"))]
/// Returns `None` on platforms that do not use the embedded Linux guardian.
pub fn dispatch_guardian_from_current_exe() -> Option<std::process::ExitCode> {
    None
}

#[cfg(all(test, windows))]
mod tests;
