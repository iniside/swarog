# Timing-sensitive tests — repo-wide survey

Date: 2026-07-14. Method: 5 parallel read-only subagents, non-overlapping areas
(asyncevents/bus, edge/httpmw, splitproof+devctl+verifyctl, all 12 modules,
app/invalidation/lifecycle/registry/contrib/remote/metrics/opsapi/rpc-macro), every
test file read in full (grep used only to triage), synthesized in the main model.
Trigger: two live wedges/flakes in one evening — split-proof `[P6]` (rate-limit
burst needs avg <5ms/call to ever pass) and the retention trigger-DDL suite wedge
(fixed separately in `f445346`). Principle per user: **tests verify correctness,
not performance — any assertion machine load can flip is a defect.**

Verdict criteria used uniformly: FRAGILE = a correctness assertion depends on real
elapsed time staying under/over a bound load can break (racing a
backoff/refill/TTL/tick window, "sleep long enough for visibility", upper-bound
latency asserts). OK = paused/injected clocks, explicit persisted-state
manipulation (`UPDATE … now() - interval`), poll-until-eventual with generous
budgets, hang-guards with order-of-magnitude headroom, structural blocking.

## FRAGILE — fix (7)

| # | Test | Mechanism | Fix pattern |
|---|------|-----------|-------------|
| 1 | `tools/splitproof/src/main.rs:2313-2346` `[P6] player_burst` | 22 SEQUENTIAL QUIC calls vs per-conn 10rps/burst-20 — limiter observable only if avg call <5ms; confirmed failing live (2× green on idle machine, 2× red later same code) | Deplete by CONCURRENCY not speed: fire the burst via `tokio::spawn` fan-out on one connection (pattern already used by `burst_429`/[AD2b]/[AD2c]); keep the 2s refill-pause + single sequential call half |
| 2 | `core/asyncevents/src/worker_tests.rs:473` `poison_backs_off_then_pauses_never_skips` | Races the 1s backoff window (`backoff_secs(1)`=1s, worker.rs:139): after first Faulted sets `next_attempt_at=now()+1s`, one `sub_row` query then re-`deliver_one` expecting Skipped — gap >1s under load ⇒ retries instead | Pin the persisted deadline explicitly: `UPDATE asyncevents.subscriptions SET next_attempt_at = now() + interval '1 hour'` before asserting Skipped (mirrors the file's own `clear_backoff` NULL pattern in reverse) |
| 3 | `core/edge/src/player_tests.rs:409-425` `request_denial_is_exact_and_handler_is_not_called` | per_conn 1.0 rps/burst 1: two live QUIC round-trips back-to-back, second must still be denied — real gap must stay <1s | Test doesn't exercise refill at all: `per_conn_rps: 0.0` (never refills), like the sibling test at :354 deliberately uses 0.01 |
| 4 | `core/edge/src/shutdown_tests.rs` — all 4 tests (`:41`, `:75`, `:123`, `:152`) | Recurring idiom: `sleep(50ms)` "to let the request reach the handler"/"flag propagate" before shutdown — guessed visibility window; three of four can flip a CORRECTNESS branch (drained vs rejected), not just a margin | Happens-before instead of time: handler fires a `Notify`/`watch` on entry; call `shutdown()` only after the signal. Assert ordering/outcome, not wall-clock margins |
| 5 | `modules/scheduler/src/tests.rs:478-513` `fires_again_after_interval` | `sleep(1200ms)` vs 1s interval — 200ms margin on a shared-DB test | No sleep: after first fire, `UPDATE scheduler.schedules SET last_fired = last_fired - interval '2 seconds'`, then fire immediately |
| 6 | `modules/config/src/tests.rs:482-526` `psql_style_write_emits_event_and_notify` | The recv loop `break`s on the FIRST 200ms timeout — effectively one 200ms window for a NOTIFY round-trip on the contended shared Postgres | Overall-deadline poll (e.g. 5s): treat a lone `recv` timeout as keep-waiting, give up only past the deadline |
| 7 | `tools/devctl/src/tests.rs:398` `transient_children_obey_cancellation_and_deadline` | `assert!(elapsed < 2s)` over real OS spawn+signal+reap; `Cancelled` outcome already proves correctness | Drop the bound or widen to a pure hang-guard (15–30s) |

## Borderline — loosen opportunistically (3)

| # | Test | Margin | Suggestion |
|---|------|--------|------------|
| 8 | `modules/scheduler/src/tests.rs:803-902` `stop_aborts_wedged_fire_within_grace_and_releases_the_lock` | asserts < STOP_GRACE(4s)+2s — 50% headroom | assert `< STOP_GRACE * 2` or inject a small STOP_GRACE |
| 9 | `modules/gateway/src/tests.rs:1514-1542` `flight_lock_second_caller_is_bounded_too` | 400ms ceiling vs 100ms budget (4×), siblings use flat 2s | raise toward the file's own `BOUNDED` 2s while keeping the "not 2× serial" intent, or virtual clock |
| 10 | `core/remote/src/tests.rs:515-526` `probe_unreachable_peer_errs_fast` | `elapsed < 2s` vs 1s inner bound (2×) — thinnest margin in core | widen to 5s |

## Cleared as OK — notable good patterns (the repo mostly does this right)

- **Explicit persisted state instead of waiting**: admin lockout/session expiry via
  `UPDATE … now() - interval` (modules/admin tests:1345,1415), accounts session
  expiry (:797), scheduler already-due seeding in split-proof `[SC1]`.
- **Deterministic limiter tests**: httpmw token bucket with `rate == 0.0` or
  injected `Instant` (`limiter_tests.rs`, `middleware_tests.rs`); edge's own
  `request_limiter_shares_ip…` calls `.allow()` directly.
- **Concurrency-driven bursts** (load changes when, not whether): split-proof
  `[RL1]/[RL2]`, `[AD2b]/[AD2c]`, `burst_429` — the model for fixing `[P6]`.
- **Poll-until-eventual with generous budgets**: invalidation NOTIFY tests
  (retry-resend ×50@100ms), worker poison/timeout tests, split-proof `poll_*`
  helpers (30×500ms), `[I-GATE]` 60s reasoned budget.
- **Structural blocking, not timing**: store_tests bump-vs-shared-writer (can't
  finish until the tx commits), inventory advisory-lock wipe tests.
- Full cleared-OK inventories per area live in the survey agents' outputs; every
  named split-proof assertion `[A1]…[W2]` was individually classified (only `[P6]`
  fragile; `[ADX1-3b]` deterministic).

Zero timing vocabulary at all: core/registry, core/contrib, core/metrics,
core/opsapi, tools/rpc-macro, modules/apikeys, audit, rating, leaderboard
(beyond the standard 3s connect hang-guard).

## Related, already fixed

- Retention trigger-DDL wedge (ACCESS EXCLUSIVE vs open-tx choreography) — fixed
  in `f445346`: guard extended + `SET LOCAL lock_timeout='5s'` + retry.
- `healthy_sweeps_keep_retention_unstalled` — re-judged post-hardening as
  genuinely load-proof (streak-based, 4s seed floor, no upper bound).

## Fix status (addendum, same day)

All 10 items fixed in `eb288ae` (splitproof P6 → concurrent depletion), `08ca7c3`
(poison backoff → pinned future deadline), `8a77406` (edge: limiter denial via
0.01 rps — NOTE: `per_conn_rps: 0.0` DISABLES the limiter per player.rs:372, the
survey's suggested 0.0 was wrong; + Notify-gated shutdown tests), `c1e86a6`
(scheduler/config/gateway), `44e5623` (devctl/remote). Verified: per-crate tests
green, clippy clean, split-proof **92/92 incl. [P6]**.

Newly observed during the fix run (survey misses — both were classified OK):
`asyncevents::stop_terminates_active_backend…` and
`asyncevents::two_workers_make_progress_past_two_continuously_hot_subscriptions`
flaked with `Elapsed` under full-crate parallel load (pass in isolation) — their
hang-guard budgets were too tight for shared-DB contention. **Fixed in `47489a8`**:
budgets widened (5s→30s, 2s→10s) with hang-guard-not-latency comments; suite
green 50/50.

Two more surfaced by successive full `verifyctl --all --strict` runs (each run
flushed out the next-tightest bound; both were OUTSIDE the survey's file set —
devctl's was judged OK, processctl wasn't covered at all):
- `devctl::down_waits_for_stopped_checkpoint…` — 1s budget on a 75ms
  writer-thread checkpoint starved under full-workspace load. Fixed `15e3b91`
  (hang-guards 15s/5s).
- `processctl::startup_handle_list_excludes_unrelated_inheritable_handle` — NOT
  a timing defect: the child checked slot VALIDITY at the sentinel's numeric
  value, and its own handle table can reuse that slot under a different layout
  (false "leaked"); the panic also poisoned `PROCESS_TEST_LOCK`, cascading into
  a second failure. Fixed `fd0a3cc`: child resolves the handle via
  `GetFinalPathNameByHandleW` and reports leaked only on sentinel-path identity;
  the test lock ignores poisoning.

Final state: `cargo run -p verifyctl -- --all --strict` fully green in one pass
(all blocking + advisory stages PASS, fuzz SKIP platform-legit) — 2026-07-14.

## Also observed (out of scope here)

- `config.settings` accumulates `test_*` junk rows (tests don't clean up on some
  paths) — harmless to the limiter question, but a cleanup-on-seed or test
  teardown sweep would keep the table readable.
