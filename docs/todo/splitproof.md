# Splitproof

Last updated: 2026-11-22

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
  SPLITPROOF-06. The monolith-local block that asserts `[M3a]` and `[M3b]`
  GETs `/admin/api-keys` for a `_csrf` field and POSTs `_action=create_role` with
  `role_name` and `role_policy`, then checks the `apikeys.roles` row. The admin that served
  that route was razed by b8435c9a and no code outside splitproof references the path, so the
  block can only fail `[M3a]` ("no _csrf field on apikeys page") and never reaches `[M3b]`.
  The other `/admin/api-keys` assertions (`[AD3b]`, `[AD4]` and the later posts,
  main.rs:5131-5416) target the same route. Re-point them with the apikeys step of the
  admin control-plane redesign (docs/plans/2026-10-22-1759-admin-control-plane-redesign-plan.md,
  step 21).

### Known limits (admin-control-plane-redesign step 15)

- [ ] **The wallet, notification and push checks still post to the retired `/admin/wallet` page**
  WL3, WL4, WL5, WL6, WL6m, NT4, NT4m, PH3, PH6, PH11 and PY7/PY7m grant through `/admin/wallet`,
  which now answers 404, so they stay red although G20 gave them real (guest) players.
  `panel_grant` (`tools/splitproof/src/wallet_ledger_checks.rs`) grants through the panel action;
  WL6/WL6m also need a revoke path and a new expected refusal text. Owner 2026-11-21: re-point
  them once the admin panel port is complete, not per step.
- [ ] **`[LG1]` matches the label loosely and reads the first `data-state`**
  `player_cell_ok` (`tools/splitproof/src/wallet_ledger_checks.rs`) accepts any cell text ending
  in the handle, so `XLedger#1812` passes; the panel state is the first `data-state` in the
  panel, not the panel root's. Its unit test copies the user-cell markup by hand instead of
  rendering it through core admin, and no live run proves the uuid fallback with accounts-svc down.
- [ ] **Panel checks import their HTML helpers from the characters checks**
  `wallet_ledger_checks.rs` uses `attr`, `text`, `table` and `cut` from
  `characters_admin_checks.rs`; a neutral helper module would keep each panel check
  independent. The `[PH6]` comment (`tools/splitproof/src/main.rs`) still says the guest is a
  player "nobody ever authenticated as"; it now holds a session, never a socket.
