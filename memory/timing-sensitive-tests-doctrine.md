---
name: timing-sensitive-tests-doctrine
description: "Tests verify correctness, not performance — never let an assertion race a real clock; the repo's approved patterns per failure class"
metadata: 
  node_type: memory
  type: feedback
  originSessionId: 2d1f848f-2c99-4b3e-a3bb-5c792c0ff30b
---

User rule (2026-07-14, after a night of whack-a-mole): **"Te testy nie mają testować
wydajności, tylko czy działa"** — any assertion machine load can flip is a defect.
12 tests fixed that day (survey + fixes: docs/research/2026-07-14-2058-timing-
sensitive-tests-survey.md; commits f445346..9034c00).

**Why:** split-proof [P6] needed avg <5ms/call to EVER pass; a retention DDL test
wedged the whole suite (Rust-await ↔ DB-lock cycle Postgres can't detect); each
full `verifyctl --all --strict` run flushed out the next-tightest bound.

**How to apply — pick the pattern for the class (all exist in-repo as models):**
- Racing a backoff/TTL/interval window → SET the persisted state explicitly
  (`UPDATE ... next_attempt_at = now()+'1h'` / `last_fired - interval '2s'` /
  `expires_at = now()-'1m'`), never sleep toward it.
- Rate-limiter observability → deplete by CONCURRENCY (tokio::spawn fan-out, wide
  margin: 60 vs burst 20), never by call speed; or rate 0.01/injected Instant for
  unit tests (NOTE: edge player limiter treats rps 0.0 as DISABLED, not
  never-refill — player.rs:372).
- Parallel-vs-serial latency claims in-process → `#[tokio::test(start_paused=true)]`
  + tokio::time::Instant (needs tokio `test-util` feature); exact virtual-time
  proof, 0.00s wall.
- "Sleep so the other task becomes visible" → explicit happens-before signal
  (Notify on handler entry) + timeout-bounded every call inside poll loops.
- Upper-bound latency asserts → delete or convert to hang-guards with
  order-of-magnitude headroom + a "hang-guard, not a latency bound" comment.
- Test-DDL on the shared `asyncevents.events` table (ACCESS EXCLUSIVE) → take
  `store_tests::WRITER_LOCK_CHOREOGRAPHY` + route through `events_trigger_ddl`
  (SET LOCAL lock_timeout + retry).
- One panicking serialized test must not poison siblings → test locks ignore
  PoisonError (processctl `process_test_lock()`).
- Identity, not slot: a Windows handle-leak check must resolve the handle to the
  sentinel object (GetFinalPathNameByHandleW), not test slot validity.

Full-workspace `cargo test` on the shared Postgres is the best fragility detector —
every red was a real test defect, never bad luck. See
[[admin-extension-points-shipped]].
