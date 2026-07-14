# Module reference: `characters` + `inventory`

The blessed reference PAIR for building a fortress module (CLAUDE.md "Adding a
module"). Audience: a contributor about to add or extend a module. Anchors are
`file:line` (clickable), never pasted snippets, so they age against line drift
rather than going stale silently. Spot-check the cited line before trusting it;
if it moved, the surrounding symbol name still finds it.

## The pair and their roles

`characters` is the BASIC reference: a provider (a sync capability over the
registry) plus a durable-event emitter. It is honestly a minimal core wrapped in
advanced extras — a per-player advisory-lock cap gate, a dynamic config read on
every create, admin fan-out over QUIC — in one module, not a toy. It was split
across four files so the reading order maps to files: module wiring
(`lib.rs`), SQL (`store.rs`), domain logic + capability impls (`service.rs`),
admin (`admin.rs`). `inventory` is the ADVANCED reference: it CONSUMES another
module's capability AND reacts to that module's durable events — the case that
usually breaks a modular monolith once you have more than three modules
(cross-module FK temptation, ordering assumptions, shared-helper temptation).

---

## Copy `characters` when… you add a plain provider + emitter

| What | Anchor |
|---|---|
| Capability contract, wire-only sync trait | `api/characters/api/src/lib.rs:49-54` (`#[rpc]` `Ownership`, `owner_of` `#[retry_safe]`, no `Identity`, no `#[http]`) |
| Player-facing capability, HTTP-bound | `api/characters/api/src/lib.rs:61-79` (`Player`: create POST 201, list GET 200 `#[retry_safe]`, delete DELETE 204; each takes a leading `Identity`) |
| Event payloads | `api/characters/events/src/lib.rs:20-26` (`Created`), `:30-34` (`Deleted`) |
| Event descriptors | `api/characters/events/src/lib.rs:42-48` (`define("character.created"/".deleted", 1, HistoryPolicy::MinRetention{days:7})`) |
| `requires()` names capabilities only | `modules/characters/src/lib.rs:111-113` (`config`, a real sync dep — infra is never declared) |
| `register`: provide under EVERY key a consumer uses | `modules/characters/src/lib.rs:119-138` (offers the one `Service` under BOTH `characters.ownership` and `characters.player` in phase 1, no I/O) |
| `migrate`: own schema only | `modules/characters/src/lib.rs:52-61` (SCHEMA_DDL, plain `player_id` id column, no cross-module FK) + `:141-147` |
| `init`: wire only, contribute unconditionally | `modules/characters/src/lib.rs:154-208` — player ops to the opsapi slots `:167-171`, local admin `Item` `:177-190`, edge faces to `EDGE_SLOT` `:197-206` (topology-blind: `app::run` applies them iff this process serves an edge) |

### The atomic pattern every later module copies

The domain write and its durable event append commit in ONE transaction, on the
SAME `&mut *tx` — the event is durable iff the row is:

- `create` = advisory-lock cap gate → INSERT → `emit_tx(CREATED)` → commit:
  `modules/characters/src/service.rs:101-176` (INSERT + emit at `:159-174`, cap
  gate at `:122-157`).
- `delete` = DELETE returning the canonical id → `emit_tx(DELETED)` → commit,
  with an EXPLICIT `tx.rollback()` on not-found (never a deferred drop):
  `modules/characters/src/service.rs:189-227` (rollback arm `:201-209`, DELETE +
  emit `:217-225`).

Note the `emit_tx` uses the DB-canonical `RETURNING` id/player_id, not the
client-echoed argument, so `created` and `deleted` match on both fields.

### The SQL layer worth studying

Write methods take `&mut PgConnection` so they run under the caller's tx; reads
use the pool:

- writes: `create_tx` `modules/characters/src/store.rs:44-61`, `delete_owned_tx`
  `:97-117`, `count_owned_tx` `:130-141` (counts UNDER the create tx's advisory
  lock — the cap gate is only race-safe inside one serialized tx).
- reads: `list_by_player` `:63-72`, `get` `:76-88`.

### Split composition root

`cmd/characters-svc/src/lib.rs:18-31` returns `metrics` + `Characters` + a
`remote::Stub` for the consumed `config` capability — the stub `provide`s an
edge-backed client under the SAME registry key the local impl would, so
`characters` code is unchanged between topologies.

---

## Copy `inventory` when… you consume a capability and/or react to durable events

| What | Anchor |
|---|---|
| Depend on the TRAIT, never the impl/rpc crate | `modules/inventory/src/lib.rs:204` (`require::<dyn Ownership>` under `characters.ownership`), rationale `:201-205` (real service in monolith, generated edge client in split — same call site) |
| 503 / 404 / 403 distinction at the consumer seam | `modules/inventory/src/service.rs:68-77` (transport `Err` → 503 unavailable, `Ok(None)` → 404, owned-by-other → 403) |
| Durable consumer: effect + checkpoint in ONE `on_tx` tx | `modules/inventory/src/lib.rs:218-246` (two INDEPENDENT subs, ids `inventory.character-created.v1` / `.character-deleted.v1`, `StartPosition::Genesis`) |
| Atomic effect bodies on the handed delivery conn | `modules/inventory/src/projection.rs:99-151` (`grant_starter`), `:161-175` (`wipe_character`, tombstone-before-delete) |
| Reordering guard across independent subs | tombstone check/skip `modules/inventory/src/projection.rs:100-113`, plant-before-delete `:163-173`, per-character advisory xact lock `lock_character` `:46-58`, namespaced FNV key `lock_key` `:29-44` |
| Unit-test a consumer against a FAKE of the trait | `modules/inventory/src/tests.rs:18-27` (`FakeOwnership`), the 503/404/403 mapping test drives it (no real producer needed) |

The subscription id is a durable CONTRACT and the `StartPosition` has no default
— both are chosen deliberately. Two subscriptions means `deleted` can arrive
before `created`; the tombstone (not an FK) is what keeps integrity, because
UUIDs never recur so a tombstone is permanent truth.

---

## Do NOT copy

The single most important section. These four are shipped, working, and
DELIBERATELY not reference-grade. Copying them propagates a known defect class.

### 1. The dev-only grant is NOT a mutation pattern

`modules/inventory/src/service.rs:89-98` carries an explicit "NOT a
reference-grade mutation pattern" doc block; the gate is `:103-105`, the boot
warn `modules/inventory/src/lib.rs:267-272`. Why it's wrong to copy: three
separate pool autocommits (`item_exists` → `grant_pool` → `list`), NO
idempotency key, and an accumulating `ON CONFLICT … quantity = quantity +
EXCLUDED.quantity` writer — so a failure after `grant_pool` commits makes a
manual retry double-grant. **The real pattern is `grant_starter`**
(`modules/inventory/src/projection.rs:99-151`): one handed delivery tx,
exactly-once via the durable subscription.

### 2. The tombstone table grows without GC

`modules/inventory/src/projection.rs:85-88` (the growth-cost note) and `:158-160`
(the only writer): one permanent row per deleted character, no retention /
watermark. Acceptable ONLY in the current wipe-is-migration phase; a long-lived
deployment needs a retention policy. Do not copy the unbounded-table shape into a
module that expects to run for years.

### 3. List endpoints have no cursor / pagination

Both list paths silently truncate beyond a hard ceiling:
`modules/inventory/src/store.rs:6-11` (`HOLDINGS_HARD_LIMIT`) and
`modules/characters/src/store.rs:4-12` (`LIST_HARD_LIMIT`, with the KNOWN GAP
comment). There is no `has_more` / cursor — surplus rows vanish from the view. A
new list-bearing module should design pagination in from the start, not copy the
ceiling.

### 4. No create-idempotency key on `characters::create`

`modules/characters/src/service.rs:101-176`: transport correctly never replays a
mutation, but there is NO contract-level idempotency key, so a client retry after
an ambiguous timeout can double-create. The repo's one idempotency exemplar to
copy if/when this matters is `match::report`'s REQUIRED `ReportId` — search
`modules/match/` for `ReportId` (a duplicate key with the same
winner/loser is a 202 no-op, a different one is a 409, and the method is
`#[retry_safe]`). Copy THAT shape, not the un-keyed create.

---

## Reading order

Post-split, numbered, maps to files:

1. `api/characters/api/` (`charactersapi`) — the trait surface.
2. `api/characters/events/` (`charactersevents`) — payloads + descriptors.
3. `modules/characters/src/lib.rs` — Module wiring (register/migrate/init, SCHEMA_DDL, edge/admin/ops contributions).
4. `modules/characters/src/store.rs` — SQL layer (`&mut PgConnection` writes vs pool reads).
5. `modules/characters/src/service.rs` — Service, the atomic `create`/`delete`, the cap gate.
6. `modules/characters/src/admin.rs` — `AdminData` impl + `admin_content`.
7. `cmd/characters-svc/src/lib.rs` — the split composition root (module list + stubs).
8. `api/inventory/` (`inventoryapi`) — the consumer's own contracts.
9. `modules/inventory/src/lib.rs` — `init` / the two durable subscriptions.
10. `modules/inventory/src/service.rs` — the consumed-capability seam + 503/404/403.
11. `modules/inventory/src/projection.rs` — the durable effect bodies + reordering guard.
12. Both composition roots (`cmd/characters-svc`, `cmd/inventory-svc`).
13. The standout tests (below).

---

## Proof it works in the split topology

The pair is exercised end-to-end in the real split fleet, not just the monolith.

Split-proof assertions (`tools/splitproof/src/main.rs`):

| Scenario | Anchor |
|---|---|
| `[2]` starter-grant via cross-process event | `:959-960` |
| `[3]` cross-wire 403 (owner_of over QUIC gates) | `:962-964` |
| `[5]` DB-verified wipe via `character.deleted` | `:975-983` |
| `[5t]` tombstone planted in the delivery tx | `:984-993` |
| `[4c]` canonical-id emitted despite non-canonical URL spelling | `:999-1026` |
| `[P1-P3]` player-QUIC create/list/authz | `:1165-1174` |
| `[RDY-DEAD]` characters-svc kill → `/readyz` flip → recover | `:1511-1634` |
| `[I-GATE]` dev-grant guard survives a restart WITHOUT the env | `:1414-1494` |

Fleet wiring (`tools/processctl/src/fleet.rs:496-506`): characters-svc
:8080/:9000 depends on config-svc; inventory-svc :8081/:9001 depends on
characters-svc + config-svc; peer edge addresses injected as `*_EDGE_ADDR` env.

Standout tests to study:

- characters: `modules/characters/src/tests.rs:283-312`
  (`create_rolls_back_domain_row_when_durable_append_fails` — a failing transport
  proves the `emit_tx` failure rolls back the domain INSERT).
- inventory: `modules/inventory/src/tests.rs:430` (`grant_on_created_via_on_tx`,
  real delivery plane), `:560` (`deleted_before_created_tombstones_and_skips_late_grant`,
  reordering), `:653` (`concurrent_grant_and_wipe_serialize_on_advisory_lock`),
  `:707` (`concurrent_grant_and_wipe_serialize_across_uuid_spelling`).

---

## Maintenance note

Blessing this pair means a substantive edit to `characters` or `inventory` may
require updating this doc. Anchors are `file:line` (not snippets) precisely to age
slower — a body edit that leaves a symbol in place needs no change here. But if
you re-split, move code between the four characters files, or renumber the
split-proof scenarios, recompute the affected anchors in this doc in the same
rollout.
