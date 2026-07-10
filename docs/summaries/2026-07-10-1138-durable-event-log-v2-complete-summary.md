# Durable event log v2 — rollout COMPLETE (Steps 1–12)

**Date:** 2026-07-10 11:38
**Plan:** [`../plans/2026-07-09-2234-durable-event-log-fresh-plan.md`](../plans/2026-07-09-2234-durable-event-log-fresh-plan.md)
**Interim status:** [`2026-07-10-0053-durable-event-log-v2-steps-1-5-status.md`](2026-07-10-0053-durable-event-log-v2-steps-1-5-status.md)

Push fan-out over HTTP → shared durable event log → consumer-owned pull
subscriptions with persistent checkpoints. The plan's outcome is delivered in
full; the DB was wiped at the cutover per the fresh-start decision.

## What exists now

- **`core/asyncevents`** — XID-ordered shared log (`events`, position =
  `(generation, producer_xid, tie_breaker)`, frontier =
  `pg_snapshot_xmin`), consumer-owned `subscriptions` with transactional
  checkpoints, ONE writer implementation (`asyncevents.append_event`), pull
  workers (SKIP LOCKED consumer groups, backoff→pause, no auto-skip),
  checkpoint-coupled conservative retention, generation fencing for restores.
- **`core/invalidation`** — broadcast cache-refresh plane (LISTEN/NOTIFY +
  authoritative callbacks, first-refresh-or-fail, freshness not delivery).
- **`tools/eventctl`** — operator CLI (list/lag/retry/pause/resume/
  skip --reason/retire/bump-generation); no silent checkpoint moves.
- **config** — monotonic revision, one trigger emits NOTIFY + durable event for
  every write path including raw psql; caches are invalidation callbacks.
- **rating** — persistent projection (`rating.ratings`); restarts keep MMR.
- **cmd libs + profiles** — every process's module list is a lib shared with
  checkers; `topiccheck` validates the subscription graph per deployment
  profile; `archcheck` bans push-era tokens and plane-table access.
- **Deleted:** `core/outbox`, relay, `POST /events`, inbox dedup,
  `EVENTS_SUBSCRIBERS`/`EVENTS_ORIGIN`/`EVENTS_RETENTION`, per-process event
  routing in `run.*`/`split-proof.*`.

## Commits (all on master)

| Step | Commit | Lane |
|---|---|---|
| plan | `396de73` | — |
| 1 | `e5194d3` | fable |
| 2 | `3028c87` | fable |
| 3+4 | `2cfcc42` | fable |
| 5 | `d8cd309` | opus |
| status 1–5 | `1ebf8f3` | — |
| 6 | `6e57f86` | opus |
| 7 | `c112622` | opus |
| 8 | `e1750af` | sonnet |
| 9 | `c8f6edb` | opus |
| 10 | `e8692cc` | sonnet |
| 11 | `98e6f03` | opus |
| 12 | (this commit) | inline/fable |

## Final verification (Step 12, after deleting core/outbox + doc updates)

- `cargo build --workspace` — PASS
- `cargo test --workspace` — PASS (156 suites, 0 failures, single run)
- `cargo clippy --workspace --all-targets -- -D warnings` — PASS
- `cargo run -p archcheck` — PASS (incl. both new tripwires)
- `cargo run -p topiccheck -- --strict` — PASS (6 topics, both profiles,
  single-hosted, version-matched)
- `cargo run -p requirecheck -- --strict` — PASS
- `./split-proof.ps1` — **PASS**: twelve-process split + monolith parity, all
  named assertions (A1–A5, K1–K5, split 1–5b, C1–C3, AD0–AD2, AU1–AU3, SC0–SC1,
  MT1–MT5, P1–P5, MX1–MX2, RL1–RL3, M0–M3)

Docs updated: `core/asyncevents/README.md` (pull model), `CLAUDE.md` (seam 3,
constraints 1/6/8, recipe, config/audit/rating blurbs, commands incl. the
one-test-run-at-a-time rule), verify mutants target `outbox`→`asyncevents`,
agent memory (`durable-event-plane-bus-owned`, index lines).

## Known accepted residue

- `public-api` verify stage red vs pre-rollout HEAD — the acknowledged one-time
  fresh-world break (self-heals as HEAD advances).
- Deliberate scale choices documented in the README: no delivery batching, no
  partition parallelism, offline generation bumps.
