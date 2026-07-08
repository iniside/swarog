# Rust Sketch — Foundations + characters/inventory (split-from-day-one)

**Date:** 2026-07-08 09:37 · Reviewed (think-hard), then **user-corrected: gateway
and RPC codegen stay in M1** (reviewer's descope rejected — see below).
**Goal:** A new for-fun experiment at `experiments/rust-sketch/` porting the Go
modular-monolith's *essentials* + the two reference modules (`characters`,
`inventory`) to Rust. Architecture matches the Go one seam-for-seam and **compiles
& runs as split microservices from milestone 1 — no monolith-only shortcut, no
topology hacks, no hand-written glue debt** ([[never-monolith-only-features]],
[[dont-descope-transport-for-simplicity]]).

**Confirmed decisions (user):** (1) idiomatic Rust interface-crates (nominal
traits), (2) full durable messaging + sync operation transport from the start,
(3) **gateway front-door + op-routing IN M1**, (4) **proc-gen `#[rpc]` codegen IN
M1** (no hand-written glue), (5) research via 2 subagents.

## Review dispositions (what was kept vs. rejected)
- **[BLOCKER — KEPT] `on_tx` handler shape.** Not `Fn(&Ctx,&mut Transaction,T)->Result<()>`
  (un-inferrable async-borrow HRTB). Use a named `trait TxHandler` returning a
  `BoxFuture` that borrows `&mut PgConnection` (via `Transaction: DerefMut`, so no
  second `'c` lifetime drags through the stored handler). Step 2.
- **[BLOCKER — KEPT] Durable transport installs in phase-1 `Register`**, handler map
  pre-allocated there; `on_tx` with no transport **panics** (not silent no-op),
  else the split proof builds clean and silently never delivers. Step 6.
- **[BLOCKER — KEPT but re-fixed] proc-macro ordering.** The reviewer's fix (hand-
  write the glue) is **rejected** — hand-written glue is exactly the debt the user
  forbade. Real fix: the generic `#[rpc]` **macro crate** is authored first (Step 5,
  no domain content — it only emits against `opsapi`+`edge` types, which exist by
  Step 4); the api-trait crates then **apply** the macro (Steps 8–9). No
  circularity, codegen retained.
- **[DESCOPE — REJECTED] gateway + full opsapi HTTP-op layer.** Reviewer wanted these
  in M2. **Rejected by user:** without the gateway, player-facing ops degrade to
  per-module HTTP shims — the precise hack pattern that bit us before. Gateway,
  full `opsapi` (`Operation`/`HTTPBind`/`OpSet` + 3 slots), auth-once, and
  backend-selection are **M1**.
- **[TIGHTEN — KEPT] mTLS 5-point spec** (Step 4), **per-process `MESSAGING_ORIGIN`
  lynchpin** (Step 11), **registry capability-keys are technically necessary** (not
  stylistic), **rule-4 relaxation is a conscious cost** + a lightweight import-lint
  since cargo alone can't replace `go-arch-lint`.

---

## Context — the Go systems being ported (all on the M1 critical path)

A *port*: every piece matches a Go original and is required for **two processes
(characters=A, inventory=B) exchanging an async event AND a sync call, with the
full front-door in place, no shortcuts**. Exact signatures/DDL captured by two
research subagents.

**Foundations (leaf crates — import no module):** `lifecycle` (Module trait +
two-phase Build + Context), `registry` (provide/require/try_require — the one real
semantic gap, below), `contrib` (Slots — feeds admin items + the 3 opsapi slots),
`bus` (async in-process + durable `Transport`/`emit_tx`/`on_tx` seam), `opsapi`
(**full**: `Caller`, `Status`/`Error`, `PlayerID`, `Operation`/`HTTPBind`/`OpSet`/
`LocalOp`/`OpBinding` + `Slot`/`BindingSlot`/`LocalSlot`), `edge` (QUIC + mutual TLS
+ framing + JSON codec), `outbox` (single-owner drain-by-origin relay, HTTP
`POST /events`).

**Codegen:** `rpc-macro` crate — a `#[rpc]` proc-macro applied to a capability
trait, emitting per method: wire request/response envelopes, `Method* =
"<prefix>.<m>"` consts, a `Client` over `opsapi::Caller`, `register_server` (edge
identity adapters), and for `HTTPBindings`-annotated methods the decode/encode +
`operations()`/`route_bindings()` the gateway consumes. Replaces Go's
`tools/rpcgen`; no committed generated files, no `-check` drift gate.

**Modules (private impl — reachable only from a binary):** `messaging` (the only
`Transport` impl; schema `messaging`, inbox dedup, `POST /events` sink,
LISTEN/NOTIFY, housekeeping — hosted in every durable process), `remote` (the
registry **swap**: `Stub` provides a generated edge client under the same
capability keys the local impl would), `gateway` (front-door in every process:
`OperationBackend` Local/Remote, route table from the opsapi slots, **auth-once** →
`PlayerID` in ctx), `config` (DB-backed live config, LISTEN/NOTIFY boot-vs-reconnect
replay — inventory hard-requires it), `characters` (provides Ownership/Player/
Admin; emits Created/Deleted via `emit_tx` atomic with the domain write),
`inventory` (polymorphic Owner, no cross-module FK; sync-asks `OwnerOf`; `on_tx`
grant-starter/wipe — the integrity-via-event proof).

**Contract crates (`api/`):** `charactersapi`, `charactersevents`, `charactersrpc`
(macro-generated), `inventoryapi`, `inventoryrpc` (macro-generated), `configevents`,
`adminapi` (so the modules' admin contributions compile).

**Deferred to Milestone 2 (a choice — neither target module `Requires` these):**
`accounts` (real sessions/OIDC; M1 uses a dev `SessionVerifier`: `Bearer dev-<uuid>`
→ player_id, wired into the gateway's `AuthPlayer` path), `admin` **portal**
(modules still contribute `adminapi.Item`; rendering is separable), `audit`.

## Rust stack (decided once, used throughout)
- **tokio**; **axum** (gateway op-mux, `POST /events`, `/healthz`/`/readyz`).
- **sqlx** (Postgres); LISTEN/NOTIFY via **`sqlx::postgres::PgListener`** (own
  dedicated connection — matches Go's raw-`pgx` listener).
- **quinn** + **rustls** (mutual TLS); **rcgen** for the dev CA (`edgeca`).
- **serde**/**serde_json**; **uuid**; **thiserror**; **syn**/**quote**/
  **proc-macro2** for `rpc-macro`.
- One Cargo **workspace**, crate-per-component.

## Registry design — the one non-mechanical seam
Go registers one service under key `"characters"`; consumers downcast to their OWN
local interface (structural). Rust nominal traits **cannot** multiplex one
`Box<dyn Any>` across three trait objects (`Any` downcasts only to one concrete
`Sized+'static` type; `Arc<dyn Ownership+Send+Sync>` is such a type, but you can't
recover three different `Arc<dyn Trait>` from one erased value). Therefore
(*necessary*, not stylistic):
- **Capability-scoped keys**, derived mechanically as `"<module>.<cap>"`:
  `provide::<dyn Ownership>("characters.ownership", arc)`, `…player`, `…admin`;
  `require::<dyn Ownership>("characters.ownership")` downcasts to `Arc<dyn Ownership>`.
- **The swap holds:** local characters provides the real `Arc<dyn Ownership>`;
  `remote::Stub` provides the edge client as `Arc<dyn Ownership>` under the same key.
  With the gateway in M1, the Stub provides **all three** capability keys
  (`ownership`/`player`/`admin`) from the three generated clients, so remote player/
  admin ops route too — not just inventory's `ownership` need.
- **`Requires()` stays module-name-based** (`["characters","config","messaging"]`) —
  a manifest for `validate_requires`, orthogonal to the derived keys. Document the
  name→key mapping so the name-check-passes-but-key-differs trap is visible.
- **Rule-4 cost (conscious):** nominal typing forces `inventory` to import
  `charactersapi` to name `dyn Ownership` (Go's inventory imports nothing from
  characters). Cargo can't distinguish an allowed `charactersapi` import from a
  forbidden `modules/characters` impl import, so it does **not** fully replace
  `go-arch-lint`; add a small import-lint (or strict crate-visibility: impl crates
  expose nothing `pub` a peer could consume).

---

## Build sequence — Milestone 1

Each step: **(a) what · (b) why now/order · (c) how · (d) dispatch**. Session model
**Opus 4.8**: `[opus]` = top-tier subagent (separate context = independent-review
boundary), `[sonnet]` = mechanical/N-similar/fully-specified, `[inline]` = main-
context judgment.

### Step 1 — Workspace + `lifecycle` + `registry` + `contrib`
- **(a)** Workspace `Cargo.toml`; `lifecycle` (Module trait; capability phases as
  default-no-op methods on one base trait + an explicit `caps()` opt-in bitset so
  Build knows which phases to call; `App::add/build/migrate/start/stop`; `Context` =
  {bus, registry, slots, http mux handle, db pool, log}), `registry`, `contrib`.
- **(b)** Everything imports these; two-phase Build is the correctness core.
- **(c)** `build()` walks modules twice: pass-1 `register`, pass-2 `init`. `registry`
  over `HashMap<String, Box<dyn Any+Send+Sync>>`, `require` downcasts to `Arc<dyn T>`
  with two distinct panics. `contrib` = `HashMap<String, Vec<Box<dyn Any+Send+Sync>>>`.
  Port `lifecycle_test.go` + a provide/require property test.
- **(d)** `[opus]`.

### Step 2 — `bus` (async in-process + durable seam)
- **(a)** `define::<T>`, `emit`/`on` (tokio task + mpsc mailbox per subscriber,
  panic-contained, publish-ordered), drain-on-close. Durable seam: `Transport` trait
  (`enqueue_tx`, `subscribe_tx`), `set_transport` (panic on double-set),
  `emit_tx`/`on_tx`/`on_tx_raw`, `ErrNoTransport`.
- **(b)** config uses sync `on`; characters/inventory use `emit_tx`/`on_tx`.
- **(c) — BLOCKER-1 fix:** durable handlers stored as a named trait object:
  ```rust
  trait TxHandler: Send + Sync {
      fn call<'a>(&'a self, cx: &'a Ctx, conn: &'a mut sqlx::PgConnection, payload: Vec<u8>)
          -> futures::future::BoxFuture<'a, Result<(), Error>>;
  }
  ```
  `on_tx::<T>` wraps a user async fn, deserializing `Vec<u8>`→`T` in `call`. Delivery
  hands `&mut *tx` (Transaction derefs to PgConnection). `emit_tx` takes the
  producer's `&mut sqlx::Transaction`, marshals `T`→JSON, calls `enqueue_tx`. Port
  codec/mailbox round-trip property tests.
- **(d)** `[opus]`.

### Step 3 — `app` boot runner + `validate_requires`
- **(a)** `Config` (env), `run(cfg, modules, edge_server: Option<EdgeServer>)`,
  `validate_requires` (module-name presence, separate from `lifecycle`).
- **(b)** Every `*-svc` reuses this order.
- **(c)** `PgPool` → `Context` → `build` → `validate_requires` → `migrate` → `start`
  → if edge `listen(edge_addr, tls)` (after Build) → axum on `listen_addr` → SIGINT →
  reverse `stop` → `bus.close`.
- **(d)** `[opus]`.

### Step 4 — full `opsapi` + `edge` (QUIC + mutual TLS)
- **(a)** `opsapi` **full**: `Caller`, `Status`/`Error`/`status_of`,
  `with_player_id`/`player_id`, `Operation`/`HTTPBind`/`OpSet`/`LocalOp`/`OpBinding`
  + `Slot`/`BindingSlot`/`LocalSlot` + `AuthReq`. `edge`: quinn `Server`/`Client`,
  length-prefixed framing (4-byte BE + 16 MiB cap, single write), JSON `Codec` trait
  (swap point), `request`/`response` envelope, dispatch precedence exact-`handle` >
  exact-`handle_identity` > longest-`handle_prefix`, `edgeca` dev-CA via rcgen.
- **(b)** Gateway (Step 10), rpc-macro (Step 5), remote (in wiring) all sit on this.
- **(c) — mTLS 5-point spec, all mandatory:** (1) server sets `WebPkiClientVerifier`
  from the shared root → client cert required & verified (quinn does NOT by default);
  (2) ALPN `"edge"` on **both** ServerConfig and ClientConfig; (3) leaf SANs include
  `localhost` + loopback IPs, client dials `ServerName="localhost"`; (4) one
  `RootCertStore` = exactly the dev CA, no system fallback; (5) TLS1.3 + correct
  EKU split. `Client::call` = fresh stream per call on one persistent conn. Port
  frame fuzz + wire round-trip tests; **assert an un-certed client is rejected.**
- **(d)** `[opus]` (edge/QUIC/mTLS); `[sonnet]` for the opsapi value/enum types.

### Step 5 — `rpc-macro` (the `#[rpc]` codegen crate)
- **(a)** A proc-macro crate: `#[rpc(prefix="characters")]` on a trait emits, per
  method, the wire envelopes, `Method*` consts, `Client` over `Caller`,
  `register_server`, and (for `#[http(...)]`-annotated methods) decode/encode +
  `operations()`/`route_bindings()`.
- **(b)** Generic — no domain content; needs only `opsapi`+`edge` types (exist after
  Step 4). Authored **before** the api traits apply it (Steps 8–9), resolving the
  ordering blocker without hand-writing glue.
- **(c)** `syn` parses the trait: first param ctx-carrying (stripped), last return
  `Result<_, opsapi::Error>` (folded into `Status`/`Err`), other params/results must
  be `Serialize`/`Deserialize` — **unsupported types surface as ordinary compile
  errors at the generated serde site** (better than Go rpcgen's load-time rejection;
  note we don't claim whole-program type resolution). Golden-file `trybuild` tests
  for the generated output.
- **(d)** `[opus]` — highest-uncertainty step; get the emitted shapes right against
  the Go envelopes.

### Step 6 — `outbox` relay + `messaging` module
- **(a)** `outbox`: `Relay::new(pool, schema, origin, subscribers, local_targets)`,
  drain `WHERE sent_at IS NULL AND origin=$1 … FOR UPDATE SKIP LOCKED`, `mark_sent`
  same tx, per-(topic,target) poison map, remote delivery = HTTP `POST /events`.
  `messaging`: schema DDL verbatim, `Transport` impl, `consume` dedup, axum `POST
  /events` sink, `PgListener` on `messaging_outbox`→relay kick, housekeeping ticker.
- **(b)** The async cross-process plane; both A and B host it.
- **(c) — BLOCKER-2 fix:** `messaging::register` (phase 1) `bus.set_transport(self)`
  **and** pre-allocates `local_handlers`; `on_tx` with no transport **panics**.
  `enqueue_tx` stamps `origin` from `MESSAGING_ORIGIN`. `consume`: `INSERT … inbox …
  ON CONFLICT DO NOTHING`; 0 rows → no-op. Port deliver-ordering property test + a
  split regression (relay drains only its own origin).
- **(d)** `[opus]`.

### Step 7 — `config` module + `configevents`
- **(a)** schema `config` (settings + `notify_changed` trigger verbatim); service
  (`get_string`/`get_bool`/`get_int`/`get`/`set` + RwLock cache); `PgListener` live-
  reload with **boot-vs-reconnect** replay flag; `configevents` (`Changed` +
  `config.changed`, plain `emit`); admin render (contract only).
- **(b)** inventory hard-requires config; exists before inventory `Init`.
- **(c)** `set` UPSERT (autocommit → trigger NOTIFYs); listener splits payload on
  first `:`, updates cache, `emit(Changed)`; reconnect replays changed keys, **boot
  load silent**. Dedicated `PgListener` connection.
- **(d)** `[opus]` for replay; store/cache `[sonnet]` within the step.

### Step 8 — `charactersapi` + `charactersevents` + `#[rpc]` + `characters` module
- **(a)** `charactersapi` (Ownership/Player/Admin traits, `#[rpc(prefix="characters")]`
  applied, `#[http(...)]` on Player's Create/List/Delete), `charactersevents`
  (Created/Deleted + descriptors), `charactersrpc` (macro output). Module: schema
  verbatim, service impl, `create`/`delete` emitting via `emit_tx` **inside the
  domain tx**, Edge-registered RPC servers, player ops contributed through the
  generated `operations()` into the opsapi slots (gateway-routed).
- **(b)** Provides the capability + events + HTTP ops inventory and the gateway
  depend on. Depends on Steps 1–6.
- **(c)** `create`: player_id from ctx, validate, `BEGIN`→`create_tx`→`emit_tx(Created)`
  →`COMMIT` (atomic). `delete`: owned-delete, 404 if none (no event) else
  `emit_tx(Deleted)`. `OwnerOf`: not-found→`(None,false)`.
- **(d)** `[opus]` for the atomic write+emit pattern (everything copies it);
  `[sonnet]` for store CRUD + admin table once the service shape is fixed.

### Step 9 — `inventoryapi` + `#[rpc]` + `inventory` module
- **(a)** `inventoryapi` (Holdings trait, `#[rpc(prefix="inventory")]`, `#[http(...)]`
  on ListMine/ListCharacter/Grant). Module: schema verbatim (seed items, PK
  `owner_type/owner_id/item_id`, in-module FK to items, **no** cross-module FK);
  service; `require::<dyn Ownership>("characters.ownership")` + `require::<dyn
  Config>("config")` (hard); the two `on_tx` subscriptions (grant-starter/wipe on the
  handed conn); config-driven starter + `config.changed` live-reload; player ops via
  generated `operations()`; admin (owners list + drill-down).
- **(b)** Closes the loop — integrity via event + sync authz via OwnerOf. Depends on
  all prior.
- **(c)** `ListCharacter` maps OwnerOf err→503, !found→404, mismatch→403. Starter
  double-checked-lock from config with const fallback.
- **(d)** `[opus]` for wiring/subscriptions/authz; `[sonnet]` for store + drill-down.

### Step 10 — `gateway` module + dev `SessionVerifier`
- **(a)** Front-handler mounted in every process; `OperationBackend` (`LocalBackend`
  map of `LocalInvoker`; `RemoteBackend` over `edge`), route table built lazily from
  `contributions(Slot/BindingSlot/LocalSlot)`, `new_op_handler` doing **auth-once**
  (`AuthPlayer` → verify bearer → `with_player_id`). Dev `SessionVerifier`
  (`Bearer dev-<uuid>`→player_id) stands in for accounts.
- **(b)** Turns the modules' `#[http]` ops into live routed HTTP; the single auth
  point. Depends on opsapi + edge + the generated `operations()`.
- **(c)** `select_backend`: `try_require` the provider capability locally → Local,
  else Remote(peer). Port the in-process routing smoke (two backends + graceful
  degradation).
- **(d)** `[opus]` — the auth boundary + backend selection.

### Step 11 — Process wiring: `server` + `characters-svc` + `inventory-svc` + `edgeca`
- **(a)** `server` (all modules + gateway, one process). `characters-svc` (gateway +
  characters + messaging, own edge server). `inventory-svc` (gateway + config +
  inventory + messaging + `remote::Stub("characters", peer)` providing all three
  capability keys). `edgeca` mints the shared dev CA. `run.sh`/`run.ps1` matrix.
- **(b)** Payoff: same modules compose into 1 or 3 processes; only the module list +
  env differ.
- **(c) — origin lynchpin:** each `*-svc` sets a **distinct** `MESSAGING_ORIGIN`
  (never the `"monolith"` default) or B's relay drains A's outbox and the async proof
  collapses; A sets `EVENTS_SUBSCRIBERS=character.created=http://B/events;
  character.deleted=http://B/events`; both load the same CA via `EDGE_CA_CERT`/
  `EDGE_CA_KEY`; B sets `CHARACTERS_EDGE_ADDR`→A. `validate_requires` fails loud on a
  missing provider per process.
- **(d)** `[sonnet]` (mechanical from the verbatim matrix); `[inline]` composition
  review.

### Step 12 — Verify net + the split proof
- **(a)** `verify.ps1`/`verify.sh` (cargo build/clippy/test + split smoke). Committed
  repeatable **split proof**: bring up A+B, create a character in A **via the gateway
  HTTP op** → assert starter item materializes in B (async event across processes),
  delete → assert holdings wiped in B; sync proof: gateway ListCharacter in B →
  authorizes via `OwnerOf` over QUIC to A.
- **(b)** [[verify-the-at-risk-path-not-the-safe-one]]: exercise the **split**
  topology through the real front-door, not the monolith.
- **(c)** Script two-process bring-up against local Postgres
  ([[local-postgres-is-the-test-db]]); capture evidence into `docs/`.
- **(d)** `[opus]` for the split proof harness; `[sonnet]` for the cargo wrapper.

---

## Milestone 2 (out of scope here)
`accounts` (real sessions/OIDC replacing the dev SessionVerifier); `admin` portal;
`audit`; `#[rpc]` macro refinements as more modules apply it.

## Residual risks to watch during implementation (non-blocking)
- sqlx `Transaction` borrow ergonomics inside `TxHandler::call` under `Send`+`.await`
  — fallback: per-delivery savepoint on an owned connection.
- rustls/quinn client-auth can *look* configured while silently not verifying — the
  Step-4 proof must assert an un-certed client is rejected.
- `#[rpc]` macro against `#[http]` bindings must emit exactly the opsapi `OpSet`
  shape the gateway route-table reads — golden-file test both sides.
- Keep impl crates' `pub` surface empty of anything a peer could consume, so the
  import-lint has teeth.
