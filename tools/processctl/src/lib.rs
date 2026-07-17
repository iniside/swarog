mod fleet;
mod layout;
mod lock;
mod process;
mod state;

#[cfg(unix)]
mod guardian;

#[cfg(any(windows, unix))]
mod platform;
#[cfg(unix)]
mod protocol;
#[cfg(all(test, unix))]
mod protocol_tests;

pub use lock::{
    rollout_lock_path, BorrowedChild, BorrowedLease, LeaseError, OwnedLease, RolloutLock,
    BORROWED_LEASE_ARG, ROLLOUT_LOCK_VERSION,
};
pub use process::{
    observe_process_identity, OutputDestination, OwnedChild, ProcessError, ProcessGroupPolicy,
    ProcessIdentity, ShutdownOutcome, ShutdownPolicy, SpawnSpec, StartMarker,
};
pub use state::{
    validate_private_path, write_private_atomic, FailureRecord, FleetState, FleetStatus,
    ManagedProcess, ManagedStatus, StateCheckpointError, StateError, StateStore, MAX_STATE_BYTES,
    STATE_VERSION,
};

#[cfg(test)]
mod fleet_tests;

#[cfg(test)]
mod layout_tests;

#[cfg(test)]
mod lock_tests;

#[cfg(test)]
mod state_tests;

#[cfg(unix)]
/// Dispatches the private guardian mode embedded in the current executable.
///
/// A binary that uses [`OwnedChild`] must call this before constructing a Tokio
/// runtime or parsing its own CLI, and immediately return the supplied exit code.
/// The guardian is therefore always the exact consumer binary; no sibling helper
/// executable or PATH lookup is part of the process-ownership contract. The
/// guardian implementation is per-OS (Linux: ptrace/pidfd/signalfd; macOS:
/// posix_spawn-suspended/kqueue) behind one dispatch and one wire protocol.
pub fn dispatch_guardian_from_current_exe() -> Option<std::process::ExitCode> {
    if std::env::args_os().nth(1).as_deref() != Some(guardian::DISPATCH_ARG.as_ref()) {
        return None;
    }
    let code = guardian::run();
    Some(std::process::ExitCode::from(
        u8::try_from(code).unwrap_or(1),
    ))
}

#[cfg(not(unix))]
/// Returns `None` on platforms that do not use the embedded guardian.
pub fn dispatch_guardian_from_current_exe() -> Option<std::process::ExitCode> {
    None
}

#[cfg(all(test, windows))]
mod tests;

/// Serializes tests that `fork` a child (the macOS guardian containment tests)
/// against tests that hold an `flock` and assert reacquire-after-drop (the
/// concurrent-owner lock tests). A `fork` copies every open fd — including an
/// unrelated test's lock fd — into the child until it `exec`s (cloexec) it away;
/// if a reacquire lands in that window it wrongly observes the lock as held. The
/// two families never overlap while both take this guard.
#[cfg(test)]
pub(crate) fn fork_flock_serial() -> std::sync::MutexGuard<'static, ()> {
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
    SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}
pub use fleet::{
    build_environment, game_backend_fleet, game_backend_fleet_with_environment,
    game_backend_monolith, runtime_environment, EnvironmentSnapshot, FleetError, FleetFlavor,
    FleetInputs, FleetSpec, PoolBudget, ServiceSpec, BUILD_ENV_ALLOWLIST, PG_SESSION_BUDGET,
    SERVICE_ENV_ALLOWLIST,
};
pub use layout::WorkspaceLayout;
