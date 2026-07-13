---
name: add-game-module
description: Full rollout of a new domain module (fortress) in this repo — from seam classification through contracts, module impl, cmd/<name>-svc, monolith registration, stubs, admin/ops, to split-proof assertions. Use whenever adding a new domain/feature module, a new capability another module will consume, a new durable event flow, or when the user says "dodaj moduł", "new module", "add <domain> to the backend". Also use when EXTENDING a module with a new capability/event/player operation — the relevant sections apply. Prevents the classic failure: a feature that accidentally ships monolith-only.
---

# Add a Game Module

New features are *new code, not edits* (Open/Closed at the architecture
level). This skill walks the full rollout; skipping steps is how features end
up monolith-only or seam-violating.

## Step 0 — Research overlap FIRST (mandatory)

Before designing anything, map existing systems: a capability you want often
already exists or has a near-twin behind one of the seams. For each overlap
candidate, write down what it does, how it differs, and an explicit **"why not
extend / depend on X"**. A plan lacking that rationale is incomplete
(CLAUDE.md: Research before planning).

## Step 1 — Classify every interaction (the seam decision)

For EACH communication the module needs, pick the seam — this decision, made
wrong, is what later reads as "just import the other module":

| The need | The seam |
|---|---|
| "Ask B now, get an answer" (sync) | Capability trait: `#[rpc]` trait in `<name>api`, `registry::provide`/`require`, declared in consumer's `requires()` |
| "Tell whoever cares that X happened" (async, cross-module) | Durable event: `bus::define` in `<name>events`, `emit_tx` in the store tx, consumer `on_tx(SubscriptionSpec…)` |
| Same-module async reaction | Plain `emit`/`on` (in-process only — NEVER cross-module) |
| Replica-local cache freshness | `ctx.invalidation().register(channel, name, cb)` — never a durable sub |
| Cross-cutting collection (admin page, ops, readiness) | `ctx.contribute(slot, v)` — `adminapi::SLOT`, `opsapi::{SLOT,BINDING_SLOT,LOCAL_SLOT}` |
| Wire exposure of own ops | `edge::EDGE_SLOT` contribution (own generated glue), unconditional in `init` |

Red flags at this stage: an "event" whose result the producer waits for
(that's a capability); a sync call for something eventually-consistent-is-fine
(that's an event); wanting another module's table (that's a plain id column +
capability or projection).

## Step 2 — Contracts under `api/<name>/`

- `<name>events` — payloads + `bus::define(topic, version, HistoryPolicy)`.
  Payload shapes are forever: additive-only; a breaking change is a NEW version.
- `<name>api` — pure `#[rpc]` traits, transport-free. `#[http(...)]` on
  player-facing ops, plain for wire-only.
- `<name>rpc` — the one-liner `<prefix>_<snake>_meta!(rpc_macro::generate_glue);`
  (+ re-export `adminrpc::register_admin` if it has an admin page). Importable
  ONLY by its own module, `cmd/*` roots, and other `api/*/rpc` crates.

## Step 3 — The module (`modules/<name>/`)

`lifecycle::Module`: `name`, `requires` (domain capabilities from modules/
ONLY — never infra/DB/HTTP), `register` (provide capabilities, no I/O), `init`
(wiring only, no I/O), `migrate` (own schema, idempotent DDL —
`CREATE … IF NOT EXISTS`; no data migrations, wipe is the strategy),
`start`/`stop`. In `init`:

- contribute ops to `opsapi` slots, edge face to `edge::EDGE_SLOT`, admin item
  to `adminapi::SLOT`;
- subscribe: `on_tx(SubscriptionSpec { id: "<name>.<topic-kebab>.v1", start: StartPosition::… }, …)`
  — the id is a durable contract; choose `start` deliberately (no default);
- resolve every `require` HERE (a `start`-time require escapes requirecheck).

Emit with `emit_tx(AnyTx::new(&mut *tx), …)` inside the store's transaction.
Tests in `src/tests.rs`, never inline. Any dev convenience (seed, dev auth):
default OFF, explicit env opt-in, loud warn when ON.

If your module reads a route-gating env var in `register`/`init` (one whose
value changes which operations get contributed), add it to `GATES` in
`tools/routecheck/src/main.rs` — routecheck cannot discover it on its own.

## Step 4 — Composition roots

- `cmd/<name>-svc/src/lib.rs`: `modules(wiring: &ProcessWiring) -> Vec<Box<dyn Module>>`
  = `metrics` + your module + one `remote::Stub` per consumed capability (peer
  addresses from `wiring`; checkers pass dummies). `main.rs` builds the real
  `ProcessWiring` from env. NO gateway/FrontDoor here — the svc serves ops only
  over the internal mTLS edge; gateway-svc dispatches Remote.
- Register the module in `cmd/server`'s lib (monolith).
- Add stubs in every OTHER svc that consumes your new capability.

## Step 5 — Enforcement wiring (this is what keeps it not-monolith-only)

1. Add the svc lib to `tools/checkmodules`'s Split profile.
2. Extend the canonical fleet in `tools/processctl/src/fleet.rs` with the typed
   service environment, dependencies, and ports. Extend `tools/splitproof` with
   a **named assertion** exercising the cross-process path, HTTP ops asserted
   THROUGH gateway-svc and state changes DB-verified. Its fleet-drift preflight
   must still agree with `cmd/*-svc` on disk.
3. New event topics: consumers exist or the topic is consciously added to
   topiccheck's `ALLOW_UNSUBSCRIBED`.
4. Add the module to `tools/conformance/src/policy.rs`. Shipping modules expose
   only the smallest `#[doc(hidden)]` factual probes needed by that tool-owned
   policy; they never depend on conformance policy types and never use a Cargo
   feature for conformance. Map every externally or wire-reachable request string
   field in the centralized input policy, with a concrete non-applicability reason
   where a convention genuinely does not apply.

## Step 6 — Verify (via the safe-verification skill)

Static checks and targeted tests come first. Then run the blocking split-proof
through `cargo run -p verifyctl -- --fast`, followed by the selected final
`verifyctl` manifest once. The split run is the at-risk path — a monolith-only
demo is not proof.

## Self-check before declaring done

Grep-diff yourself against reality (hand-maintained lists drift): the module
appears in `cmd/server`, in its own svc, in checkmodules Split profile, in the
canonical processctl fleet, in a named `tools/splitproof` assertion, and in the
centralized conformance policy; every `require` has a stub in every process where
a consumer runs without the provider. The verifyctl fortress stage derives its
build set from `cmd/*-svc`; do not add another manual build or port list. If any
list disagrees with the code, fix the list in the same change.
