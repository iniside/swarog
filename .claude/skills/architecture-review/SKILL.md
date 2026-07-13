---
name: architecture-review
description: Semantic architecture review of a diff for this repo's seam laws — the violations the mechanical tools (archcheck, topiccheck, requirecheck, public-api) CANNOT see. Use whenever reviewing a diff, after implementing any feature that touches modules/, api/, core/, or cmd/, before committing multi-file changes, or when the user says "review", "sprawdź architekturę", "czy to nie łamie architektury". Also use proactively after a subagent rollout lands code. Not for style/bug review — this is seam law only.
---

# Architecture Review

Review the working diff (or a named commit range) for violations of this repo's
architecture that only a reader can catch. The repo already has mechanical
enforcement — do NOT re-derive what it covers; run it or trust it:

| Already enforced by | Covers |
|---|---|
| `cargo run -p archcheck` | module→module edges, foreign `<name>rpc` imports, `Option<edge::Server>` in modules/, svc-per-module, demos/ consumers, plane-table SQL, gateway-crate consumers |
| `cargo run -p topiccheck` | subscription graph, contract versions, globally unique subscription ids, one host per profile, sinkless topics |
| `cargo run -p requirecheck` | `require()` calls vs declared `requires()` manifest — but ONLY requires resolved in `init` (a `start`-time require escapes it — flag those yourself, see below) |
| verify `public-api` stage | contract-crate surface diffs vs committed baseline |
| verifyctl `split-proof` stage | live cross-process behavior through `tools/splitproof` |

Your job is the **semantic layer above those tools**. Read the diff and check:

## 1. Cross-module events must be durable

A plain `emit`/`on` pair is in-process only. If the producer and consumer are in
*different* modules, plain emit works in the monolith and **silently never fires
in the split** — no error, just missing behavior. Cross-module = `emit_tx` inside
the producer's real DB tx + `on_tx(SubscriptionSpec { id, start }, …)` on the
consumer. Plain `emit` is legitimate only for same-module reactions.
Also check: `emit_tx` uses the *store's* transaction (`AnyTx::new(&mut *tx)`),
not a fresh one — a separate tx breaks the append-with-effect atomicity.

## 2. No request/response through the bus

An event whose consumer's effect the producer then waits for / polls for /
reads back is a sync capability wearing an event costume. That's a
`registry::provide/require` trait, not a topic. Look for: producer emitting then
querying the consumer's projection; "reply" topics; event payloads carrying
correlation ids intended for a response.

## 3. Topology blindness in modules/

archcheck bans `Option<edge::Server>`, but not the broader disease. In any
`modules/` crate, flag: reading env vars that describe topology (peer addresses,
ports, "am I split"), any branch on whether a capability is local vs remote,
reading grace/drain env knobs (those belong to `core/app`), passthrough origins
read anywhere but a `cmd/*` main. Modules learn peers only via the registry swap
and `opsapi::PEER_SLOT` contributions made by cmd roots.

## 4. Lifecycle phase discipline

`register` and `init` do NO I/O — wiring only. Flag: DB queries, HTTP calls, or
file reads in `register`/`init`; schema DDL outside `migrate`; a module touching
another module's schema in `migrate`; a `require()` resolved in `start` or
lazily in a request handler (this is exactly the class requirecheck cannot see —
the enforced invariant is *requires resolve in `init`*).

## 5. Persistence isolation

Schema-per-module. Flag: cross-module foreign keys, JOINs across module schemas,
any module SQL touching `asyncevents.*` tables (calling the plane's SQL
*functions* is fine). A relation to another module is a plain id column.

## 6. Not monolith-only

A new module or cross-process flow must land with ALL of: `cmd/<name>-svc`,
registration in `cmd/server`, stubs where consumers live, the svc lib in
`tools/checkmodules`'s Split profile, a typed service entry in the canonical
`tools/processctl` fleet, and a **named assertion** in `tools/splitproof` (HTTP
ops asserted THROUGH gateway-svc, not direct). The verifyctl fortress build list
is derived from `cmd/*-svc`; do not ask for a second manual list. A feature
demonstrated only via the monolith smoke test is incomplete — say so explicitly.

## 7. Contract hygiene beyond the surface diff

public-api diffs symbols; it can't see semantics. Flag: a changed meaning of an
existing event payload field, serde attribute changes that alter the wire shape,
a mutated published payload instead of a NEW `define(topic, 2, …)` version, a
subscription id reused for changed semantics (ids are durable contracts — new
semantics = new versioned id), a `StartPosition` chosen without thought
(Beginning vs End is a product decision; there is no default for a reason).

## 8. Fail-closed dev conveniences

Any new dev-only convenience (seed data, dev auth, open portal) must default
OFF and require an explicit env opt-in with a loud warning — the pattern of
`ACCOUNTS_DEV_AUTH` / `APIKEYS_DEV_SEED` / `ADMIN_OPEN`. Flag defaults-on dev
behavior.

## 9. House rules

Tests in separate files (`src/tests.rs` / `src/<file>_tests.rs`), never inline.
Replica-local cache freshness = `core/invalidation` callback, never a durable
subscription. New crates land in the right layer (`demos/` for non-shipping).

## Output

Run the mechanical tools first if the diff plausibly trips them (archcheck +
topiccheck + requirecheck are seconds, no DB writes — but respect the
one-test-rollout rule if anything heavier is running). Then report findings
ranked by severity, each as: **what** (file:line), **which law** (section above
or CLAUDE.md constraint number), **why it breaks in the split/at runtime**, and
**the seam-correct fix**. If the diff is clean, say so in one line — do not
manufacture findings.
