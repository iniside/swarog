# Processctl

Last updated: 2026-11-10

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
- [ ] **MSVC discovery has no hermetic test**
  PROCESSCTL-05. `append_msvc_linker_path` (tools/processctl/src/fleet.rs:635-706) reads the real
  `ProgramFiles`/`ProgramFiles(x86)` and filesystem, so `sanitized_build_path_contains_the_discovered_msvc_linker`
  (tools/processctl/src/fleet_tests.rs:373) proves the 64-bit Program Files root only on a host with VS there;
  on a Build-Tools-under-(x86) host it passes with or without that scan, and newest-version-wins across roots is
  never pinned. Needs the scan extracted to a fn taking candidate roots, then a tempdir test.
  Source: proof-auditor review of 08be12df (2026-11-01).
- [ ] **MSVC linker test only checks INCLUDE exists**
  PROCESSCTL-06. tools/processctl/src/fleet_tests.rs:413 asserts `contains_key("INCLUDE")`; an INCLUDE with no
  existing dirs passes (fleet.rs inserts `join_paths` of an empty list). Should assert an entry holding
  `vcruntime.h` and one holding `windows.h`/`stdio.h`.
  Source: proof-auditor review of 08be12df (2026-11-01).
- [ ] **MSVC and SDK versions compared as text**
  PROCESSCTL-07. MSVC version dirs (tools/processctl/src/fleet.rs:662) and `newest_directory` sort `OsString`
  names lexicographically, so a hypothetical `14.100.x` loses to `14.99.x`. Does not bite with current
  fixed-width versions.
  Source: core-reviewer + proof-auditor reviews of da737d80 / 08be12df (2026-11-01).
- [ ] **`devctl down` hangs on Windows after the fleet has stopped**
  PROCESSCTL-08. After `devctl up monolith`, `devctl down` exited 1 with "timed out
  waiting 130s for shutdown cleanup" (tools/devctl/src/supervisor.rs:165). The server
  process had already exited and Postgres had no sessions left, but the supervising
  `devctl.exe` (the `up` process) never published a terminal cleanup state; it was
  stopped by hand with Stop-Process. An earlier `down` in the same rollout printed
  "os error 32" (file in use) and still stopped cleanly. Cause not investigated
  (report, don't fix); diagnose the supervisor's Windows shutdown path before fixing.
  Source: mockup-implementer smoke for 01b28403, admin-control-plane-redesign step 8a (2026-11-10).
