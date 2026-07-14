---
name: module-reference-pair
description: characters + inventory are the blessed reference pair for building a fortress module; docs/reference/module-reference.md is the guide
metadata: 
  node_type: memory
  type: reference
  originSessionId: 7008759d-f9b0-4533-8f5c-4cdddce2f63e
---

`docs/reference/module-reference.md` is the canonical guide for adding/extending a
fortress module. It blesses two modules as the reference PAIR:

- **`characters`** = BASIC reference — a provider (sync capability over the registry) +
  durable-event emitter. Split across `lib.rs` (wiring) / `store.rs` (SQL) /
  `service.rs` (Service + Ownership/Player impls, the atomic INSERT+emit_tx pattern) /
  `admin.rs`, mirroring inventory's layout (2026-07-14).
- **`inventory`** = ADVANCED reference — CONSUMES a capability (`charactersapi::Ownership`)
  + reacts to durable events (grant/wipe), with the 503/404/403 seam and the
  tombstone+advisory-lock reordering guard.

The doc is structured copy-first: "copy characters when…", "copy inventory when…", then
the load-bearing **"Do NOT copy"** section (dev-grant non-pattern, unbounded tombstone
table, no-pagination lists, no create-idempotency → copy `match::report`'s `ReportId`
instead). Anchors are `file:line`, no pasted code. Point new-module work here first.
See [[never-monolith-only-features]] and [[dont-descope-transport-for-simplicity]].
