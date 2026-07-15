//! Platform-specific process/job-control primitives.

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;
