# Durable event log v2 — rollout status after Step 5 (PAUSED)

**Date:** 2026-07-10 00:53
**Plan:** [`../plans/2026-07-09-2234-durable-event-log-fresh-plan.md`](../plans/2026-07-09-2234-durable-event-log-fresh-plan.md)
**State:** Steps 1–5 of 12 complete and committed on master. Rollout paused by
user decision after Step 5 ("dokończymy później"). **The system is fully
functional at this point** — pull delivery is live in both topologies, retention
runs, split-proof green — the remaining steps are refinements, not repairs.

## Done

| Step | Commit | Lane | What landed |
|---|---|---|---|
| 1 | `e5194d3` | fable | Versioned `EventContract` + `SubscriptionSpec` in `core/bus`; five `api/*/events` crates at v1/MinRetention(7d); subscription matrix (audit = 6 independent checkpoints); push-plane shim |
| 2 | `3028c87` | fable | Additive V2 storage: `plane_meta`/`events`/`subscriptions`/`history_contracts`, `asyncevents.append_event()` (single writer implementation), xid8-as-text codec, startup guards, XID-protocol tests on live PG |
| 3+4 | `2cfcc42` | fable (Step 4 tag raised opus→fable to keep the declared single cutover commit) | Pull worker + failure state machine (backoff 1s→5m, pause@20, no skip; timeout poisons only its connection); relay/`POST /events`/inbox/`EVENTS_*` deleted; `run.*`/`split-proof.*`/smoke rewritten; **split-proof 43/0 incl. monolith parity** |
| 5 | `d8cd309` | opus | Checkpoint-coupled retention GC (conservative: no `history_contracts` row ⇒ never delete; paused sub blocks GC + age gauge); `history_contracts` seeded by writer + typed-subscription reconcile; `tools/eventctl` (list/lag/retry/pause/resume/skip --reason/retire/bump-generation) |

Verification at pause point: `cargo build/test/clippy --workspace` green
(360 tests / 0 fail), `archcheck` + `topiccheck` green, `split-proof.ps1` 43/0
(as of the cutover commit; Step 5 changed no delivery), rewritten
`scripts/smoke-split-asyncevents.sh` passed live.

## Remaining (Steps 6–12, per the plan — read the plan for the full spec)

6. `core/invalidation` broadcast plane `[opus]` — replaces the two durable cache
   subscriptions (configrpc `config-cache.config-changed.v1`, inventory's
   `inventory.config-changed.v1`) which STILL RUN today as durable subs (fine,
   but wrong tool — under consumer-group semantics only one replica refreshes).
7. config: revision singleton + INSERT/UPDATE/DELETE trigger calling
   `asyncevents.append_event` + callback caches `[opus]`. CachedConfig keeps
   boot-fill-or-fail.
8. inventory: drop `Starter` cache + its config.changed sub `[sonnet]`.
9. rating: persistent projection (`rating.ratings`), delete in-memory MMR `[opus]`.
10. cmd libs (`modules(wiring)`) + checkmodules profiles `[sonnet]`.
11. topiccheck profile validation + archcheck tripwires (`EVENTS_*` string ban,
    `asyncevents.` table-ref ban with `append_event(` allowlist) `[opus]`.
12. delete `core/outbox` (compiles but unused since Step 3), README/CLAUDE.md/
    memory updates, full verify + both topology proofs `[inline]`.

## Notes for resumption

- Effort levels agreed: `[fable]`→think hard, `[opus]`→think, `[sonnet]`→default.
- `public-api` verify stage is an acknowledged one-time red (fresh-world break).
- `core/asyncevents/README.md` still partially describes push — Step 12 owns docs.
- Local DB gotcha: overlapping `cargo test --workspace` runs contend on the
  plane's migrate advisory lock and look like a hang — run one at a time.
- Fresh boot after wipe needs nothing special; `Plane::migrate` drops legacy
  outbox/inbox defensively.
