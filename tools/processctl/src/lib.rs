mod process;

#[cfg(target_os = "linux")]
mod guardian;

#[cfg(any(windows, target_os = "linux"))]
mod platform;

pub use process::{
    OutputDestination, OwnedChild, ProcessError, ProcessGroupPolicy, ProcessIdentity,
    ShutdownOutcome, ShutdownPolicy, SpawnSpec, StartMarker,
};

#[doc(hidden)]
#[cfg(target_os = "linux")]
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
pub fn dispatch_guardian_from_current_exe() -> Option<std::process::ExitCode> {
    None
}

#[cfg(all(test, windows))]
mod tests;
