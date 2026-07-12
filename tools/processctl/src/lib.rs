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
pub fn run_guardian() -> i32 {
    guardian::run()
}

#[cfg(test)]
mod tests;
