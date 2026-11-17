# Splitproof

Last updated: 2026-11-17

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
- [ ] **`[M3b]` posts `create_role` to the razed `/admin/api-keys`**
  SPLITPROOF-03. The monolith-local `[M3b]` block (`tools/splitproof/src/main.rs:324-385`)
  GETs `/admin/api-keys` for a `_csrf` field and POSTs `_action=create_role` with
  `role_name` and `role_policy`, then checks the `apikeys.roles` row. The admin that served
  that route was razed by b8435c9a and no code outside splitproof references the path, so the
  block can only report "no _csrf field on apikeys page". The other `/admin/api-keys`
  assertions (`[AD3b]`, `[AD4]` and the later posts, main.rs:5131-5416) target the same
  route. Re-point them with the apikeys step of the admin control-plane redesign
  (docs/plans/2026-10-22-1759-admin-control-plane-redesign-plan.md, step 21).
