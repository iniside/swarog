#[cfg(target_os = "linux")]
mod linux;
#[cfg(all(test, target_os = "linux"))]
mod linux_tests;
#[cfg(windows)]
mod windows;
#[cfg(all(test, windows))]
mod windows_tests;

#[cfg(target_os = "linux")]
pub(crate) use linux::{spawn, PlatformChild};
#[cfg(windows)]
pub(crate) use windows::{spawn, PlatformChild};
