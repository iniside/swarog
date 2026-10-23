# Splitproof

Last updated: 2026-10-23

## In progress

_None._

## Owner checks

- [ ] **`[W2]` unrun on Linux**
  SPLITPROOF-01. Check that splitproof `[W2]` (tools/splitproof/src/main.rs:586-599) passes as a Linux
  runtime assertion since the enumerate-before-reap change to the `forced_remainder`
  derivation (`verifyctl --fast` or splitproof). The runtime drain is Linux-only; the
  change was reviewed and compiles on Linux but was not run there.
  Source: docs/status/2026-07-17-2348-macos-port-complete-status.md:71,
  docs/plans/2026-07-17-1601-macos-rollout-port-plan.md:879 (2026-07-17).

## Open

- [ ] **Error-string sniffing in `status_or_err` and `player_burst`**
  SPLITPROOF-02. The `Err` arm does `e.to_string().contains(want)` (tools/splitproof/src/main.rs:7109,
  2026-07-11). Typed edge codes (audit-remediation step 16) allow re-pointing it.
  Source: docs/plans/2026-07-13-1415-audit-remediation-plan.md:204.
