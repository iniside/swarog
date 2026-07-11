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
