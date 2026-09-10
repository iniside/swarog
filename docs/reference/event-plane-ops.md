# Event-plane ops: xmin pinning and delivery staleness

Operational notes for the durable `asyncevents` plane (see CLAUDE.md seam #3 and
lifecycle rule #8) that don't belong in the terse top-level guidance.

## The idle-in-tx belt only covers this plane's OWN sessions

Each worker's delivery session (`core/asyncevents/src/worker.rs::connect`) sets
`idle_in_transaction_session_timeout` to 2x `ASYNCEVENTS_HANDLER_TIMEOUT` on
connect. This is a belt against a worker leaking its OWN open transaction — a
dropped future between statements would otherwise leave the session
idle-in-transaction, holding a row lock and pinning `xmin` indefinitely. The
handler-timeout arm's `pg_terminate_backend` only reaches a backend wedged
INSIDE a statement; the per-session `SET` is what bounds the "silently never
resumed" case.

**It does not, and cannot, cover a rogue idle-in-transaction session anywhere
ELSE in the cluster** — a stuck migration, a forgotten `psql` session, another
service's leaked transaction, an ad-hoc admin query left open in a `BEGIN`.
Any such session still pins `xmin` cluster-wide and can stall the plane's
safe-delete frontier and, transitively, delivery, even though every
`asyncevents` worker session is individually well-behaved.

## Mitigation

- Set a **global** `idle_in_transaction_session_timeout` in `postgresql.conf`
  (or `ALTER SYSTEM SET idle_in_transaction_session_timeout = '...'`) so no
  session anywhere in the cluster — plane-owned or not — can idle-in-tx
  indefinitely. This is a cluster-wide operator decision, not something the
  plane can enforce from inside its own connections.
- Alert on the existing `asyncevents_safe_frontier_age_seconds` gauge: a
  growing frontier age is the first externally visible symptom of an xmin
  pin, whether the cause is a plane worker or an unrelated session.
- `/readyz` independently flags DELIVERY STALENESS (no worker completed a
  healthy pass in 30s) as well as a dead worker task — see CLAUDE.md lifecycle
  rule #8 — but that check is process-local and only catches this process's
  own workers stalling, not the upstream xmin pin causing the stall.

## Test isolation: `deliver_all()`'s tally is global, not per-test

`asyncevents::testing::TestTransport::deliver_all()` (`core/asyncevents/src/lib.rs`,
`pub mod testing`) drains every eligible delivery over the SAME shared
`asyncevents.events` log and `asyncevents.subscriptions` checkpoints every other
crate's tests use. `f5e30c8`/`4e093a7` made an unreachable Postgres fail the test
run instead of skipping it green, so for the first time every crate's DB tests
actually execute in the same `cargo test --workspace` pass — which immediately
turned two `modules/wallet` starter-grant tests red (fixed in `cddfc3b`). The
failures were revealed by the skip fix, not caused by it.

A raw `deliver_all()` tally can move in two directions once other crates' tests
are genuinely running alongside it:

- **Over-count**: a foreign crate's test appends to a topic this crate also
  subscribes to, and that foreign event gets drained and counted inside the
  window (observed: `modules/accounts` emitting `player.registered` while
  wallet's `AfterRegistration` subscription was draining).
- **Under-count**: eligibility is frontier-bounded — the worker's query gates
  current-generation rows on `producer_xid < pg_snapshot_xmin(pg_current_snapshot())`
  (`core/asyncevents/src/worker.rs:236`), so ANY concurrently open transaction
  anywhere in the cluster can defer a just-committed event past a single
  `deliver_all()` call. `deliver_all`'s own doc comment already warns
  round-trip tests to poll rather than call it once.

The fix, landed as `deliver_until_consumed` in `modules/wallet/src/tests.rs`:
drain in a loop until THIS test's own subscription checkpoint has passed THIS
test's own event rows (matched by a per-test key), bounded by a deadline that
is a hang guard, not a timing assertion.

**Known-latent, not yet fixed** (both suites green as of `cddfc3b`):
`modules/notifications/src/tests.rs` has 16 `deliver_all()` call sites and is
exposed in BOTH directions (its subscriptions consume `wallet.changed` and
`player.promoted`, both of which the wallet tests emit); `modules/mail/src/
projection_tests.rs` has 13 sites exposed to the under-count direction only
(`mail.send_requested` has no foreign emitter). `core/asyncevents/src/tests.rs`
(:164, :174) is NOT at risk — it uses a unique per-test topic and subscription
id and already polls. Left unfixed because it's 29 call sites across two
modules each needing its own payload-key scoping, and hoisting
`deliver_until_consumed` into `asyncevents::testing` (the clean shared home)
changes a `core/*` public surface and requires `--bless-public-api` — it wants
its own rollout, not a drive-by inside this one.

See also the broader (2026-07-14, pre-dates this class) survey of
timing-sensitive tests: `docs/research/2026-07-14-2058-timing-sensitive-tests-survey.md`
— that document is a dated historical snapshot and is not updated for this
class; this section is the living reference for it going forward.

## A retention sweep can block on a roster op's row lock

`groups`' retention sweep (`modules/groups/src/projection.rs`,
`groups.prune-on-scheduler.v1`) deletes stale `invited`/`requested` rows via
`SELECT … FOR UPDATE SKIP LOCKED` batches, unlike every roster-deciding op
(`join`/`invite`/`respond`/`decide`), which takes a per-group
`pg_advisory_xact_lock` before touching `groups.memberships`. The sweep takes no
such lock. `SKIP LOCKED` means the sweep never blocks waiting for a row a
concurrent `decide`/`respond` already holds — it just skips that row this pass
and picks it up next fire, so no deadlock is possible. The other direction is
real, though: a row the sweep DID lock (not skipped) blocks a concurrent
`decide`/`respond` targeting that same row until the sweep's transaction
commits, which can take up to `PRUNE_BUDGET` (5s) if the sweep is mid-batch.
The batch query's `WHERE state IN ('invited','requested')` also re-evaluates
against current state on every batch, so a row a concurrent `decide`/`respond`
already promoted to `member` before a given batch's `SELECT` simply no longer
matches — the sweep never deletes a row out from under an accept that landed
first.

## Related

- `cargo run -p eventctl -- list` — lag/retry/pause/resume/skip/retire per
  subscription.
- CLAUDE.md, "The point of this codebase" seam #3 (durable event bus) and
  lifecycle rule #8 (readyz/idle-in-tx summary).
