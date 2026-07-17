#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod darwin;
#[cfg(unix)]
mod posix;
#[cfg(all(test, unix))]
mod posix_tests;
#[cfg(windows)]
mod windows;
#[cfg(all(test, windows))]
mod windows_tests;

#[cfg(target_os = "linux")]
pub(crate) use linux::observe_process_identity;
#[cfg(target_os = "linux")]
pub(crate) use linux::{spawn, PlatformChild};
#[cfg(target_os = "macos")]
pub(crate) use darwin::observe_process_identity;
#[cfg(target_os = "macos")]
pub(crate) use darwin::{spawn, PlatformChild};
#[cfg(windows)]
pub(crate) use windows::observe_process_identity;
#[cfg(windows)]
pub(crate) use windows::{spawn, PlatformChild};

pub(crate) struct InheritedInput(pub(crate) std::fs::File);
