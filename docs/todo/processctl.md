# Processctl

Last updated: 2026-11-01

## In progress

_None._

## Open

- [ ] **`BUILD_ENV_ALLOWLIST` lacks SYSTEMDRIVE and ProgramData**
  PROCESSCTL-01. tools/processctl/src/fleet.rs:9-15 lacks both (re-checked: Grep of tools/processctl/src
  finds neither); weles/src/prep.rs:16-19 still calls it a latent gap.
  Source: docs/status/2026-07-15-1120-weles-m0-pre-m1-backlog-status.md:92 (2026-07-15).
- [ ] **Linux `/proc` pgrp parse untested**
  PROCESSCTL-03. `list_process_group` (tools/processctl/src/guardian.rs:495) has no test (Grep finds
  only the two definitions and two call sites); needs a comm with spaces/parens and a
  Linux box.
  Source: docs/status/2026-07-17-2348-macos-port-complete-status.md:74 (2026-07-17).
- [ ] **Linux-only processctl tests never compile**
  PROCESSCTL-04. `mod tests` is gated `cfg(all(test, windows))` (tools/processctl/src/lib.rs:67-68), so
  its Linux-only tests never compile; guardian.rs:955-959 includes only the macOS test
  module.
  Source: docs/plans/2026-07-14-1721-guardian-argv0-shim-fix-plan.md:198 (2026-07-14).
