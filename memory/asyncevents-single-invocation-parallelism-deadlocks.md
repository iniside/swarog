---
name: asyncevents-single-invocation-parallelism-deadlocks
description: cargo test -p asyncevents (-p app) with default intra-run parallelism can self-deadlock on the shared plane; run plane tests single-threaded
metadata: 
  node_type: memory
  type: feedback
  originSessionId: 38ae0b55-88ea-4c50-814e-ca71f55c726d
---

A SINGLE `cargo test` invocation can self-deadlock on the durable event plane —
this is a refinement of, not the same as, the documented "one test rollout at a
time" rule (which is about concurrent *separate* runs). `cargo test -p asyncevents -p app`
with default intra-process parallelism runs many plane-constructing tests at once
against the one shared Postgres; if any one leaks an `idle in transaction` session
(a worker/`testing::deliver_all` path that opened `BEGIN`/`pg_current_xact_id` and
never committed), it pins relation locks and every other concurrent test's DDL
(`CREATE SCHEMA IF NOT EXISTS asyncevents`) + `asyncevents.append_event` blocks
behind it — a full deadlock cascade (observed 2026-07-11: 22 sessions all blocked
~13 min on one root `idle in transaction` pid).

**Why:** the plane heavily uses a migrate `pg_advisory_xact_lock` + `CREATE OR
REPLACE`/`CREATE SCHEMA` DDL; concurrent construction contends, and one stuck
idle-in-tx session is enough to wedge the rest.

**How to apply:**
- Run plane tests single-threaded / separated: `cargo test -p asyncevents -- --test-threads=1`,
  and run `-p app` as its OWN invocation, not folded into `-p asyncevents -p app`.
- Recovery when wedged (per [[durable-event-plane-bus-owned]] + CLAUDE.md): diagnose
  `pg_stat_activity` for the root `idle in transaction` blocker (join `pg_locks`
  blocked↔blocking), `pg_terminate_backend` ALL non-psql gamebackend sessions, then
  kill OS `cargo`/`rustc`/`asyncevents-*`/`app-*` processes, THEN re-run serially.
- Tell every test-running subagent this, not just "one run at a time" — the trap is a
  single invocation, so the generic rule doesn't cover it. See [[verify-the-at-risk-path-not-the-safe-one]].

**Also (2026-07-14): a timing-flaky plane test under FULL-workspace `cargo test` load.**
`verifyctl`'s `test` stage runs the whole workspace in parallel; under that CPU
saturation `core/asyncevents` `retention_tests::healthy_sweeps_keep_retention_unstalled`
can FAIL ("a continuously succeeding sweep must keep retention un-stalled") because its
spawned 50ms-interval sweep task gets scheduling-starved past the test's 2s staleness
window — no `mark_retention_ok` lands in time, so `retention_stalled(2s)` reads true. It
PASSES in isolation (`cargo test -p asyncevents -- --test-threads=1 healthy_sweeps…` →
ok, ~3.2s). So a lone `test`-stage FAIL on THIS test in a verify run is a load-timing
artifact, not a regression — confirm by isolation before treating it as real.

**Deflaked 2026-07-14 (`341500f`) — and the deflake TRAP that double-review caught.**
The test seeds `mark_retention_ok()` ONCE before spawning `retention::run` (so the clock
starts fresh), then must prove a healthy sweep keeps `retention_stalled(2s)` false while
STILL failing if `run` stops re-marking. The naive deflakes ALL false-pass a broken
sweep: widening the window, OR polling for N-consecutive un-stalled reads and breaking
early — because the pre-spawn seed alone keeps `retention_stalled(2s)` FALSE for the first
~2s, so any streak that completes inside that window proves nothing (a broken sweep that
never re-marks passes identically to a healthy one). First deflake attempt shipped exactly
this vacuity; core-reviewer + proof-auditor caught it by tracing the coarse-clock. CORRECT
fix: only ACCEPT the un-stalled streak once `start.elapsed() >= 2×window` (4s) past the
seed — past there an un-stalled read can ONLY come from an active re-mark, so a broken
sweep (stalled every tick after ~2s) resets the streak forever and the test fails.
**Lesson: when deflaking a liveness/staleness test, the un-stalled observation must occur
AFTER the initial seed would have aged out, or the test can't fail on the regression it
guards.** Independent of any characters/inventory/cap change.
