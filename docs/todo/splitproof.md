# Splitproof

Last updated: 2026-11-23

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

### Known limits (admin-control-plane-redesign step 20)

- [ ] **NA3 never posts through the row action or the Inbox tool**
  `tools/splitproof/src/notifications_admin_checks.rs` sends through the Notify header
  action only; the `send` row action and the `inbox.send` panel tool are covered by unit
  tests alone. One post per action closes it.
- [ ] **No check for notification purges after an account delete**
  NA4 (the purge of a deleted player's inbox, both topologies) was not written.
- [ ] **NA2 releases the form claim by SQL**
  The idempotent re-POST check deletes the `admin.forms` claim instead of waiting out a
  real claim timeout.
- [ ] **A mid-pass error hides the later NA checks**
  An error inside the NA pass reports `[NA0]` red and the later NA ids do not appear at
  all.

### Known limits (admin-control-plane-redesign step 19)

- [ ] **The harness can overflow the main thread's stack**
  `runtime.block_on(run(root, run_dir))` (`tools/splitproof/src/main.rs:701`) polls the
  whole check future on the main thread (1 MB on Windows); in one Step 19 run it failed
  with "thread 'main' has overflowed its stack". 360cb180 boxed the storage admin call
  sites as a workaround; the next group of inline checks can hit it again. A fix runs
  `run` on a thread with a larger stack or boxes the check groups. Owner 2026-11-22:
  deferred.

### Known limits (admin-control-plane-redesign step 16)

- [ ] **A check id shared by two names is refused only when both run**
  `Proof::check` records the name of every id it sees and the expected-red `sort` refuses
  an id used under two names, but only for checks that ran in that run: twin checks on an
  Ok and an Err branch, or a monolith-only and split-only pair, never meet, so a reused id
  passes. A static scan of the `.check(` call sites would catch it; owner 2026-11-22:
  deferred.

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
  The friends tab checks `[FA1]`/`[FA2]` (`tools/splitproof/src/friends_admin_checks.rs`) share
  this gap: their recorded break removed admin-svc's `friends` peer, never its `accounts` peer
  (owner 2026-11-22: deferred, one extra full run).
- [ ] **Panel checks import their HTML helpers from the characters checks**
  `wallet_ledger_checks.rs` uses `attr`, `text`, `table` and `cut` from
  `characters_admin_checks.rs`; a neutral helper module would keep each panel check
  independent. The `[PH6]` comment (`tools/splitproof/src/main.rs`) still says the guest is a
  player "nobody ever authenticated as"; it now holds a session, never a socket.
