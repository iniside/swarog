//! devctl's supervisor internals, exposed as a library so the binary and the
//! guardian-dispatching integration harness (`tests/supervised.rs`) share one
//! implementation. The `supervised` harness cannot live in the libtest crate:
//! it drives processctl's production spawn path, which on unix re-execs
//! `current_exe --__processctl-guardian-v1`; a libtest binary has no early
//! guardian hook, so the re-exec lands on the test harness and exits 101.
//! A `harness = false` integration target owns its `main` and dispatches the
//! guardian first — see `tests/supervised.rs`.

pub mod cli;
pub mod control;
pub mod supervisor;

#[cfg(test)]
mod tests;
