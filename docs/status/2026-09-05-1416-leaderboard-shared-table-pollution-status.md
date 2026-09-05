# TODO — `leaderboard.scores` pollution makes a unit test permanently red

**Opened 2026-09-05-1416.** Diagnosed during the push-hub rollout's acceptance step; the
defect predates that work and is not push-related. Nothing has been fixed and nothing has
been deleted — this is the record so it is not rediscovered from scratch.

## Symptom

`cargo run -p verifyctl -- --fast` fails its blocking `test` stage:

```
modules/leaderboard/src/tests.rs:99  tests::top_scores_orders_by_wins_desc  FAILED
assertion `left == right` failed
  left: 0
 right: 2
```

## It is not flaky — it is deterministically impossible

`Service::top_scores` is `SELECT player, wins FROM leaderboard.scores ORDER BY wins DESC,
player ASC LIMIT 100` (`modules/leaderboard/src/lib.rs:74`).

The test creates two run-unique players, gives one **2** wins and the other **1**, calls
`top_scores()`, filters the result to its own two players and asserts both are present.

Observed table state on this machine at the time of writing: **110 rows with `wins >= 2`**
(112 rows total; distribution 58×1, 30×49, 20×1, 3×45, 2×14, 1×2). The 1-win player sorts
strictly below all 110, so it lands at position ≥ 111 and can never enter a 100-row window.
`mine.len()` is therefore at most 1 and the assertion cannot pass, on any run, until the
table is pruned.

## Root cause — a shared table with two authorities and no owner

`grep -rn "DELETE FROM leaderboard" --include=*.rs .` returns exactly one hit: the
leaderboard test's own `cleanup` (`modules/leaderboard/src/tests.rs:57`). Nothing else in
the tree ever removes a row.

Meanwhile `tools/splitproof` creates new, permanently-retained players on **every** run,
keyed by process id so each run is distinct:

- `replicas-{pid}` / `replicas-loser-{pid}` / `replicas-fo-{pid}` — the replica/failover
  assertion (`tools/splitproof/src/main.rs:4086-4116`), 30 wins each.
- `champ-{suffix}` — the match/leaderboard accumulation assertion
  (`tools/splitproof/src/main.rs:2357`), 2–3 wins each.

So the harness grows the table monotonically while a unit test asserts a global top-N over
it. The failure was inevitable; repeated fleet runs (including this rollout's) merely
crossed the threshold.

## It amplifies itself

`cleanup` runs at the END of the test, so the assertion panic skips it. Two `lb.*` rows
(2 wins and 1 win) are in the table right now — the leftovers of the failed run. Every
subsequent failure adds two more rows and pushes the threshold further out.

## Fixing it — two halves, both outside the push hub

1. **`tools/splitproof` should remove the players it created.** It already knows their
   names, and a harness leaving permanent rows in a shared table is the actual defect. This
   is the half that stops the table growing.
2. **The leaderboard test should survive its own failure** (clean up on the failing path,
   not only on success) and should stop depending on a global 100-row window to observe
   rows it created — a unique player id protects it from *collisions*, not from being
   sorted out of the result.

Doing only (2) leaves the table growing; doing only (1) leaves the current backlog and the
self-amplification.

## Unblocking now

The rows are derived state — `leaderboard.scores` is a projection built by upserting
`match.finished` deliveries — so pruning loses nothing that the schema itself defines.
Deleting the harness rows (`champ-%`, `replicas-%`) and the failed-run leftovers (`lb.%`)
restores the test. **Not done here:** deleting rows from the developer's database is the
operator's call, not an agent's.

## Related pre-existing items found in the same acceptance pass

Recorded together because they share a cause — earlier rollouts leaving authorities stale —
not because they are related to each other:

- `public-api` (advisory) is red on three items from the mail rollout: `adminapi::SubmitOutcome::notice`,
  `adminapi::slug`, `schedulerevents::schedule_names::MAIL_PRUNE`, plus an untracked
  `docs/reference/public-api-baseline/mailevents.txt`. They need a deliberate
  `cargo run -p verifyctl -- --bless-public-api`.
- `CLAUDE.md` and `.agents/shared/gamebackend.md` still omit `mail-svc` (`:8094`/`:9012`)
  from the split port list and from the "13 fortresses + gateway" count.
- `README.md`'s `core/` enumeration omits `invalidation/`.
- `apikeys::conformance::conformance_key_rejected` is pure arithmetic over a length — it
  executes no production code, so `modules/gateway/src/keys.rs:286`'s cap was covered by no
  conformance case until the push rollout added gateway-side ones. Surfaced by a mutation
  probe: deleting the production guard left `apikeys` green.
