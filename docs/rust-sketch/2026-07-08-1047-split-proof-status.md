# Rust-sketch Step 12 — split-proof status

**Date:** 2026-07-08 10:47 · **Status:** PASS (verified live on this machine)

The rust-sketch (`experiments/rust-sketch/`) port of the Go modular-monolith is
proven end-to-end on the **SPLIT microservices topology** — two processes, real
HTTP + QUIC/mTLS, against the local Postgres. This is NOT the monolith path.

- **A = `characters-svc`** — gateway + characters + messaging, HTTP `:8080`, QUIC edge `:9000`, `MESSAGING_ORIGIN=characters-svc`.
- **B = `inventory-svc`** — gateway + config + inventory + messaging + `remote::Stub`, HTTP `:8081`, dials A's edge at `127.0.0.1:9000`, `MESSAGING_ORIGIN=inventory-svc`.

## How to reproduce

```
cd experiments/rust-sketch
./verify.ps1      # or: ./verify.sh   (build + clippy -D warnings + test + split proof)
# or just the split proof, self-contained (mints CA, starts A+B, asserts, tears down):
./split-proof.ps1 # or: ./split-proof.sh
```

The split proof mints the shared dev CA via `edgeca`, starts A then B (gating each on
`/healthz`), runs the assertions with a fresh `dev-<uuid>` player bearer (so reruns
are idempotent), and tears both processes down on exit (even on failure).

## Bug found and fixed during bring-up (real defect in the ported code)

`rpc-macro`'s response envelope carried the return value as a typed `Option<T>`
field. For a method whose return type is *itself* an `Option` — the real
`characters::Ownership::owner_of` returns `Result<Option<String>, Error>` — a
legitimate `Ok(None)` became `Some(None)`, which serde collapses `null` on the wire;
the client then deserialized `null` back to `None` and its
`resp.value.ok_or_else(...)` mistook it for a **missing** value, returning
`Error::internal`. Inventory maps any `owner_of` error to `503`, so **every lookup of
a non-existent character 503'd** — which surfaced immediately after a delete (the
character is gone, `owner_of` returns `Ok(None)`).

Observed before the fix (a fresh, existing character returned 200; after `DELETE` the
same GET, and any never-existed uuid, persistently 503):

```
--- DELETE on A -> 204, then poll B ---
attempt 1: characters service unavailable HTTP=503
...
attempt 20: characters service unavailable HTTP=503
--- never-existed uuid also 503 ---
characters service unavailable HTTP=503
```

**Fix** (`crates/rpc-macro/src/lib.rs`): the envelope now carries the value as a raw
`serde_json::Value` (defaulting to `null`); the server serializes the return value
into it, and the client deserializes it into the method's real return type — so
`null` faithfully becomes `None` for an `Option` return. Wire bytes are byte-identical
for every non-`None` case. Added a regression test (`crates/rpc-macro-tests/tests/roundtrip.rs`):
a wire-only `find_owner(id) -> Result<Option<String>, Error>` asserting BOTH
`Ok(Some(..))` and `Ok(None)` round-trip over a real edge QUIC connection.

## The proof — observed live output (verify.ps1 split-proof stage)

Player `PID=6d3edb8e-3b19-4a0e-a036-80adc76aa8cc`, other player `1db08eba-06c4-41e5-92e0-8fcb8017d073`.

### 1. Async event, cross-process A -> B (create -> starter grant)
`POST http://127.0.0.1:8080/characters` on A with `Authorization: Bearer dev-<PID>` and `{"name":"Aria","class":"mage"}`:
```
-> HTTP 201  {"class":"mage","created_at":"2026-07-08 10:47:07.496057+00","id":"c1d1d890-2248-4c64-9ec5-51b9768704c8","name":"Aria","player_id":"6d3edb8e-..."}
```
A emitted `character.created`; its relay (origin `characters-svc`) POSTed to
`http://127.0.0.1:8081/events`; inventory's durable `on_tx` granted the starter item.
Poll `GET http://127.0.0.1:8081/inventory/character/<charId>` on B:
```
attempt 1 -> HTTP 200 [{"item_id":"starter_sword","item_name":"Starter Sword","owner_id":"c1d1d890-...","owner_type":"character","quantity":1}]
```
**PASS** — `starter_sword` x1 materialized in B (async event across processes).

### 2. Sync call over QUIC/mTLS, B -> A (the authz check)
The same `GET /inventory/character/<charId>` forces inventory's `list_character` to
call `owner_of` via the `remote::Stub` over QUIC to A (`:9000`). The **200 above**
proves the sync cross-process path AND mutual TLS worked. Negative authz — the same
GET as a DIFFERENT player:
```
-> HTTP 403  forbidden
```
**PASS** — a non-owner is forbidden, proving `owner_of` actually gates over QUIC.

### 3. Integrity via event, not FK, A -> B (delete -> wipe)
`DELETE http://127.0.0.1:8080/characters/<charId>` on A:
```
-> HTTP 204
```
A emitted `character.deleted`; inventory's `on_tx` wiped the holdings. Asserted
against the DB (the definitive integrity proof — the HTTP 404 alone only proves the
character is gone via `owner_of`, which would mask an un-wiped row):
```
attempt 1 -> inventory.holdings rows for c1d1d890-... = 0
```
Post-delete GET (character gone via `owner_of` over QUIC):
```
-> HTTP 404  not found
```
**PASS** — holdings wiped by the `character.deleted` event, no FK cascade.

## Umbrella gate result (verify.ps1, this machine)

```
==================== VERIFY SUMMARY ====================
  PASS   build
  PASS   clippy       (--all-targets -D warnings)
  PASS   test         (all workspace tests, incl. the Option round-trip regression)
  PASS   split-proof
=======================================================
VERIFY: PASS
```

## Files

- `experiments/rust-sketch/verify.ps1` / `verify.sh` — umbrella gate (build + clippy + test + split proof).
- `experiments/rust-sketch/split-proof.ps1` / `split-proof.sh` — self-contained two-process split proof.
- `experiments/rust-sketch/crates/rpc-macro/src/lib.rs` — the `Option<T>` envelope fix.
- `experiments/rust-sketch/crates/rpc-macro-tests/tests/roundtrip.rs` — the regression test.
