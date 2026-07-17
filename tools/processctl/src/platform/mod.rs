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

use std::sync::{Mutex, MutexGuard};

/// Process-wide spawn serialization. On macOS the guardian liveness/status pipes,
/// the private-stdin pipe, and the one-shot credential pipe all get `FD_CLOEXEC`
/// in a SECOND `fcntl` after `pipe(2)` (macOS has no `pipe2(O_CLOEXEC)`), so
/// between the `pipe()` and the `fcntl` a concurrent `fork`/`exec` on another
/// thread would inherit a non-close-on-exec copy of a would-be-inherited end —
/// e.g. a second spawn's guardian pinning the first guardian's `live_write` open
/// so that first guardian never sees liveness EOF and never force-kills its target.
/// Holding this lock across the WHOLE create-pipes→fork window of every spawn
/// makes that impossible; the atomic-`pipe2` Linux and handle-list-free Windows
/// arms take it too for symmetry. Crate-wide invariant: every in-process spawn
/// funnels through [`spawn`], which requires a [`SpawnGuard`].
static SPAWN_LOCK: Mutex<()> = Mutex::new(());

/// Proof that [`SPAWN_LOCK`] is held for the fd-create→fork critical section.
/// [`spawn`] requires one by reference; acquire it with [`spawn_guard`] at the
/// OUTERMOST point that first creates a would-be-inherited fd (the private-stdin
/// or credential pipe made by a caller BEFORE `spawn`) so that fd creation and
/// the fork share one critical section, never two racing ones.
pub(crate) struct SpawnGuard(#[allow(dead_code)] MutexGuard<'static, ()>);

pub(crate) fn spawn_guard() -> SpawnGuard {
    SpawnGuard(SPAWN_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner()))
}
