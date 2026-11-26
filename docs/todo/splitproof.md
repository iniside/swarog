# Splitproof

Last updated: 2026-11-26

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

### Known limits (accounts-browser-login step 7)

- [ ] **Sign-in bucket checks (BR) run on the split only**
  The monolith's shared sign-in bucket is proved only by the accounts unit test
  `the_sign_in_bucket_runs_the_published_burst_and_refill_rate`.
  Source: docs/plans/2026-11-24-2128-accounts-browser-login-plan.md, Step 7 (2026-11-26).

### Known limits (admin-second-pass step 15)

- [ ] **SH2 and SH2m assert the Fire now that step 15 implemented is refused**
  `[SH2]` (`tools/splitproof/src/scheduler_admin_checks.rs:118-145`) expects the Schedules row
  menu's Fire now to be drawn as not implemented and a direct POST to be refused by admission;
  since admin-second-pass step 15 it fires (post=303, the schedule's fire time moves). Both
  are listed in `tools/splitproof/expected-red.txt`. Fix: assert the implemented Fire now
  (fire recorded, Edit/Delete/Schedule tool still absent) and drop the two entries.

### Known limits (admin-control-plane-redesign step 25)

- [ ] **Admin redesign splitproof groups not rewritten (Step 25h)**
  The owner dropped Step 25h (2026-11-24). The groups it was to rewrite against the redesigned
  portal stay as they are: 25h1 portal core (AD0–AD6g, ADX1–ADX3b, `admin_csrf`, login,
  lockout, rate limit, CSRF, reveal, audit trail, killed-provider and viewer-denied additions);
  25h2 monolith parity (M3/M3b, WL6m, NT4m, ML3–ML4b, `mail_form`, `wallet_form`); 25h3
  cross-process money and inbox (WL3–WL6, NT2/NT2b/NT2c, NT4, PH3, PH6, PH11, PY7,
  `inbox_form`); 25h4 FR8, GR5, the config CAS edit and one real invoke per writing provider.
  Scope and per-group notes: the Step 25 **Splitproof** list in
  `docs/plans/2026-10-22-1759-admin-control-plane-redesign-plan.md`.
- [ ] **Splitproof comments name the retired admin faces**
  `tools/splitproof/src/main.rs` (1470, 1486, 2083, 2198, 2525, 5035, 5195, 5417, 5758, 5800)
  still describes hops as `admin.adminData`/`admin.adminSubmit`; the portal now goes through
  the `cp.*` face.

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
