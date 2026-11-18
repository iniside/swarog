# 3. `ordering-not-structural` — *decision or sequence that isn't enforced by construction*
Something that must happen in a fixed order, at a fixed time, or as one unit is left to
"happens to run in the right order" — recomputed per-request when it should freeze at boot,
split across two transactions when it must be one, or reshaped to satisfy the wrong caller.
- **Recurred:** the **Send-bound rewrite** (`d2b202a → 32c7c01`) — production phase
  sequencing/teardown was rewritten into detached spawned tasks purely to satisfy a
  `tokio::spawn(run(...))` in 4 tests; reverted wholesale once review located that the Send
  requirement belonged to the *test caller*, not production. `/readyz` membership resolved
  per-request instead of snapshotted at boot (`c6466eb`). accounts register + session
  issuance in two transactions (`cfff987`). bless actions dispatching before the rollout
  lease was acquired (`f257b67`). lock/lease handle closed before/after marker cleanup kept
  flipping (the 8-commit chain).
- **Attack:** for any "the compiler/test needs this" justification, ask **which caller
  actually has the requirement** — production `.await` and a test's `tokio::spawn` are
  different callers; the fix belongs at the one with the need, never at the shared production
  authority. For any two-step operation ask "what is observable *between* step 1's commit and
  step 2". For any lazily-recomputed value ask "what if the inputs drift after boot".
- **Authority:** freeze the decision at one structural point (`snapshot_readiness_checks`,
  one threaded `PgConnection`/tx, lease-before-dispatch).
