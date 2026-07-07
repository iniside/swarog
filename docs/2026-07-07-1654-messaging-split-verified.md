# Durable event plane — split topology verified (2026-07-07 16:54)

Evidence that the **bus-owned durable transport** (the `modules/messaging` refactor,
plan `docs/plans/2026-07-07-1527-bus-owned-transport-plan.md`) works in the **microservices
split**, not just the monolith — and that the single-owner relay redesign closes the
BLOCKER the plan review caught. This is the committed, repeatable proof (memory
`verify-the-at-risk-path`): exercise the topology the change affects, don't pass off the
easy path.

## What was verified

Repeatable artifact: **`scripts/smoke-split-messaging.sh`** (exits non-zero on any failed
assertion). It boots the full split via `run.sh microservices` — **all four processes**:

- **A** `characters-svc` (accounts + characters), `MESSAGING_ORIGIN=characters-svc`, produces `character.created`.
- **B** `inventory-svc` (inventory + audit + admin), `MESSAGING_ORIGIN=inventory-svc`, durable consumers.
- **D** `scheduler-svc` (scheduler), `MESSAGING_ORIGIN=scheduler-svc`, its own relay over the *shared* `messaging.outbox`.
- **C** `gateway-svc` (front door).

The critical part: **D runs its own relay against the same `messaging.outbox` while A
produces events** — the exact scenario where the first design cut silently lost events (a
foreign-origin relay marking another origin's row "sent to nobody").

## Assertions (all PASS)

1. `inventory-svc` and `scheduler-svc` both boot reporting `module ready module=messaging`
   — they `Requires("messaging")`, so `validateRequires` would refuse boot if it were absent.
2. **Durable cross-process delivery:** a character created on A lands a starter grant in
   inventory on B — `character.created` flowed A's domain tx → `messaging.outbox`
   (origin `characters-svc`) → A's relay → `POST /events` (topic in `X-Event-Topic`) →
   B's inbound sink → `inventory.grantStarter`, with no module aware of the topology.
3. The outbox row is stamped `origin=characters-svc` and is marked **sent by its own-origin
   relay** (polled — the relay commits `markSent` just after B makes the grant visible, so a
   single read races the commit; the async property is polled, not point-checked).
4. **BLOCKER-1 regression:** `messaging.inbox` carries **both** `(messaging:<id>, inventory)`
   **and** `(messaging:<id>, audit)` — both durable subscribers on B consumed the event
   exactly once. With `scheduler-svc` (origin `scheduler-svc`) running its own relay over the
   same table, it did **not** swallow A's event. This is the live counterpart to the internal
   unit regression `outbox.TestRelayDrainsOnlyOwnOrigin`.

## Sample run

```
PASS: inventory-svc booted WITH messaging hosted (durable consumers satisfied)
PASS: scheduler-svc booted WITH messaging hosted (durable producer satisfied)
created character a2496ac0-70f8-43d8-81a2-ee1e6e3680b0 on A (characters-svc)
PASS: durable cross-process delivery: character.created (A) -> starter grant 'starter_sword' in inventory (B)
PASS: outbox row 11 stamped origin=characters-svc (produced by characters-svc, not a foreign origin)
PASS: outbox row 11 marked sent by its own-origin (characters-svc) relay
PASS: BLOCKER-1 regression: event messaging:11 consumed by BOTH 'audit' and 'inventory' on B — scheduler-svc's relay did NOT swallow characters-svc's event
=== SMOKE PASSED: durable event plane works in the microservices split; single-owner relay holds ===
```

## How to re-run

```
./scripts/smoke-split-messaging.sh
```

Needs a reachable local Postgres (same assumption as `run.sh`) and `psql` on PATH (or
`PSQL=<path>`). Complements the internal tests (`outbox/relay_test.go`,
`modules/messaging/messaging_test.go`) with a whole-system split proof.
