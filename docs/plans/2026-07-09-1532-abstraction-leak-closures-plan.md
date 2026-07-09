# Abstraction-leak closures — plan

**Date:** 2026-07-09 15:32
**Goal:** Close the residual abstraction leaks found by the 6-angle audit
(2026-07-09): the `audit-prune` string seam, the gateway's in-module env topology,
the un-netted checker invariants, the duplicated `monolith_modules()` lists, the
5× hardcoded `asyncevents.outbox` test SQL, plus a mechanical cruft/docs sweep.

## Context — overlap map (Research-before-planning)

Every item below was surfaced by the audit and cross-checked against what already
exists; nothing here re-invents an existing mechanism:

| Existing system | Why not just rely on it |
|---|---|
| scheduler's DDL comment ("coupling-through-a-string, pushed to data") | honest, but nothing *links* the seed literal to audit's const — drift = silent no-op prune forever. Fix = shared contract const + linking tests, not a new mechanism. |
| `remote::Stub` + `env_addr` in `cmd/*` (the correct topology pattern) | gateway's `RouteTable::remote_caller` and `ProxyTable::from_env` bypass it — the module itself reads `{PROVIDER}_EDGE_ADDR` / `ADMIN_HTTP_ADDR` / `ACCOUNTS_HTTP_ADDR`. Fix = move resolution to the composition roots via the existing contrib-slot idiom, don't invent a new transport. |
| `archcheck` (dep-graph loops + source-walk tripwires) | constrains `Kind::{Module,Api,Rpc,Cmd}` only; `core/*` is `Kind::Other` = unconstrained, and `<name>events` classifies `Other` too. Fix = extend the existing loops/walks — same tool, new rules. |
| `topiccheck`/`requirecheck` runtime harnesses | both hand-duplicate `monolith_modules()`; a 13th module missed in either = false PASS. Fix = one shared list crate under `tools/`, not a new checker. |
| `asyncevents::transport()` free-fn test helper | exists for *wiring*, but there is no *assertion* helper — 5 fortress test files re-hardcode `asyncevents.outbox` SQL. Fix = `pub mod testing` next to the existing helper. |

Baselines: all six checker-relevant surfaces are CLEAN today (audit-verified), so
every new net is a regression guard that passes on commit.

## Key facts pinned by research

1. **archcheck sees DIRECT deps only.** It runs `cargo metadata --no-deps`
   (main.rs:102) and inspects each package's own `dependencies` array; dep objects
   carry `"kind"` (`null`/`"dev"`/`"build"`), dev is skipped via
   `dep["kind"] == Some("dev")` (main.rs:132-134). There is no transitive-closure
   walk. `core/bus/Cargo.toml` today: deps `tokio, tracing, async-trait, thiserror,
   serde, serde_json, futures`; dev-deps `tokio` — **no sqlx anywhere**, so a
   direct-edge assertion (both dep kinds) fully captures the invariant.
2. **`classify()`** (archcheck main.rs:63-90) uses `p.split_once("/api/")` (twice-
   occurring `/api/` in `api/<name>/api/` paths — the comment at 74-78 explains);
   arms exist for `parts[1] == "rpc"` and `"api"`; **no `"events"` arm** — events
   crates fall to `Kind::Other` and escape `FORBIDDEN_API_DEPS`
   (`["tokio","quinn","axum","hyper","sqlx","tonic","reqwest","tower","edge","remote"]`,
   main.rs:44-46). NB: all five `<name>events` crates dep exactly `bus` + `serde`
   (reviewer-verified — NOT `serde_json`); none has `tokio`, so Rule E passes at
   baseline. Fixture tests should model the real `bus`+`serde` dep set.
3. **Source-walk skeleton to copy:** `grep_option_edge_server` (main.rs:504-538) —
   walk `.rs` under `modules/`, skip `//` lines, substring match. The `EVENTS_`
   tripwire is the same shape.
4. **`monolith_modules()` duplication:** identical 12-entry bodies at
   topiccheck main.rs:154-169 and requirecheck main.rs:101-116 (order: config,
   characters, inventory, accounts, admin, audit, scheduler, rating, match_module,
   leaderboard, webui, gateway). `cmd/server/src/main.rs:27-41` lists 13 —
   the same 12 **plus `metrics::Metrics::new()` first** and
   `gateway::Gateway::new().with_player_edge(player)` instead of bare `new()`.
   `metrics` is core-infra (no `requires()`, no topics), so its absence from the
   harness lists is *currently* harmless — but the drift proves the rot vector.
5. **Gateway remote dispatch:** `RouteTable::remote_caller` (gateway lib.rs:438-460)
   does env lookup `format!("{}_EDGE_ADDR", provider.to_uppercase())` + `parse` +
   `edge::shared_dev_ca()` + `edge::Client::dial`, caching per provider in
   `RouteTable.remotes: tokio::sync::Mutex<HashMap<String, Arc<dyn Caller>>>`
   (lib.rs:337), evicted on failure via pointer-identity (dispatch, lib.rs:415-428).
   Its ONLY caller is `RouteTable::dispatch` line 417 (`BackendKind::Remote` arm).
   Local-vs-Remote = presence of a `LocalInvoker` in `opsapi::LOCAL_SLOT`
   contributions (`select_kind`, lib.rs:474-480).
6. **Gateway passthrough:** `ProxyTable::from_env` (proxy.rs:51-69) reads
   `ADMIN_HTTP_ADDR`/`ACCOUNTS_HTTP_ADDR` with hardcoded prefixes `/admin`,
   `/accounts/epic`; constructed ONLY by `FrontDoor::new` (lib.rs:189). `cmd/*`
   never touches it — the env comes straight from run scripts.
7. **`cmd/gateway-svc`** already resolves five `*_EDGE_ADDR` defaults through its
   local `env_addr` helper (main.rs:24-33) for `remote::Stub::new` (lines 55-79) —
   the composition root already owns exactly the knowledge the module duplicates.
8. **audit-prune sites:** seed `VALUES ('audit-prune', 86400)` inside scheduler's
   raw-string `SCHEMA_DDL` (scheduler lib.rs:63-80, with the honest coupling
   comment); consumer `const PRUNE_SCHEDULE_NAME: &str = "audit-prune"` (audit
   lib.rs:53) matched at lib.rs:162-164. `schedulerevents` exists, is imported by
   BOTH audit and scheduler already; audit has NO api/events crate (only
   `api/audit/rpc`). The anti-drift test template is
   `audit/src/tests.rs:84-108` (`durable_topics_match_events`).
9. **Five outbox-SQL test sites** (all `[dev-dependencies]` on asyncevents —
   sanctioned dep, leaked table name): accounts tests.rs:290,296-303;
   config tests.rs:150,203-211; match tests.rs:77,92-99; characters
   tests.rs:125,132-138; inventory tests.rs:298. Shapes: cleanup `DELETE …
   WHERE payload->>'<key>' = $1` (sometimes + topic) and count `SELECT count(*) …
   WHERE topic = $1 AND payload->>'<key>' = $2`.
10. **asyncevents public surface:** free fn `pub fn transport(pool, origin)`
    (lib.rs:400, doc'd as the test helper) — the natural neighbor for a new
    `pub mod testing`. Config's tests also use `Plane::new(...).migrate()` behind a
    `OnceCell` + a `DB_SERIAL` mutex (tests.rs:163-181) — the helper must NOT try
    to own migration/serialization, only the SQL strings.
11. **Cruft coordinates:** dead `ACCOUNTS_EDGE_ADDR` exports — run.sh:206,218,229,
    240,253; run.ps1:242,259,274,289; split-proof.sh:297,313,326,351,368;
    split-proof.ps1:275,293,306,333,351 (gateway/admin occurrences are
    legitimate and stay). Stale "four peers": cmd/admin-svc/src/main.rs:8-10,
    split-proof.sh:398, split-proof.ps1:377 (actual dial list = six: A/B/C/D +
    audit + scheduler). `Delivery.event_id` doc reveals the format
    (core/bus/src/lib.rs:313-315). Dead `adminapi` dep: api/accounts/api/Cargo.toml:19
    and api/characters/api/Cargo.toml:19. CLAUDE.md match blurb conflates
    `/match/report` body keys (`Winner`/`Loser`) with the event payload
    (snake_case).
12. **`axum::Router` in `Context` (audit finding #6)** — decision: NOT erased.
    `PgPool` is charter-blessed (constraint #10); the router gets the analogous
    one-line charter blessing in CLAUDE.md instead of a ceremony abstraction
    (4 legitimate HTTP-surface owners; the router never crosses a process).

---

## Step sequence

### Step 1 — shared `audit-prune` schedule-name constant + linking tests  `[sonnet]`
**(a) What:**
- `api/scheduler/events/src/lib.rs`: add
  `pub mod schedule_names { pub const AUDIT_PRUNE: &str = "audit-prune"; }` with a
  doc comment (reviewer-tightened wording): *names of schedules the scheduler
  module SEEDS — not a namespace for names consumers invent. The producer's seed
  DDL already ships this string (coupling-through-data); the const names that
  existing fact where both sides can reference one symbol.*
- `modules/audit/src/lib.rs:53`: replace the local literal —
  `const PRUNE_SCHEDULE_NAME: &str = schedulerevents::schedule_names::AUDIT_PRUNE;`
  (keep the local alias so call sites don't churn).
- `modules/scheduler/src/tests.rs`: new test `seeded_schedule_names_are_contract`
  asserting `SCHEMA_DDL.contains(&format!("('{}',", schedulerevents::schedule_names::AUDIT_PRUNE))`
  — exact match on the seed tuple opener `('audit-prune',` so a rename of either
  side fails the build/test. (scheduler already deps schedulerevents — fact 8.)
- `modules/scheduler/src/lib.rs:63-80`: update the DDL doc comment — the coupling
  is now "pushed to a shared contract constant", cite the const path.

**(b) Why now / order:** independent quick win; first because it is the only REAL
unguarded cross-fortress drift and takes minutes.

**(c) How:** fully specified above; the only judgment call (host crate) is decided:
`schedulerevents` — the one contract crate both sides already import (fact 8);
audit has no events crate of its own to host it.

**(d) Dispatch:** `[sonnet]`.

---

### Step 2 — archcheck: four new nets  `[sonnet]`
**(a) What:** `tools/archcheck/src/main.rs` + `src/tests.rs`.
- **Rule C (bus stays sqlx-free):** in the metadata loop, for package `bus`, FAIL
  if ANY dependency (normal, dev, or build — do NOT skip dev here) is named `sqlx`.
  Violation text: *"core/bus must stay engine-free (AnyTx seam): found dep `sqlx`"*.
- **Rule D (modules never runtime-dep the plane):** for each `Kind::Module`, FAIL
  on a **non-dev** dep named `asyncevents` (dev-deps stay allowed — the 5 test
  wirings are sanctioned). Violation text names the fortress rule.
- **Rule E (events crates transport-free):** add `Kind::Events(String)` — a third
  arm in `classify()` for `parts[1] == "events"` mirroring the `"api"` arm
  (main.rs:63-90). Apply the existing `FORBIDDEN_API_DEPS` list to `Kind::Events`
  packages in the same loop as `Kind::Api` (main.rs:195-208). Baseline-safe:
  events crates dep only `bus`/`serde`/`serde_json` (fact 2).
- **Rule F (`EVENTS_` env tripwire):** new source-walk `grep_events_env` copied
  from `grep_option_edge_server` (main.rs:504-538): FAIL on any non-comment line
  under `modules/` containing a **boundary-checked** `EVENTS_` match — the char
  preceding the match must NOT be `[A-Za-z0-9_]` (reviewer BLOCKER: a bare
  substring match hits `ASYNCEVENTS_READY` at modules/config/src/tests.rs:168,171
  — real code lines, so the naive rule fails at commit). Equivalent acceptable
  shape: match quoted `"EVENTS_` only. Catches `EVENTS_ORIGIN`,
  `EVENTS_SUBSCRIBERS`, and future `EVENTS_*` knobs — modules are topology-blind.
  Zero boundary-checked hits today.
- Tests: synthetic-fixture cases in `tests.rs` for each rule (bus-with-sqlx →
  violation; module with normal asyncevents dep → violation, dev dep → clean;
  events pkg with tokio → violation — model the real `bus`+`serde` dep set; a
  temp-dir file with `EVENTS_ORIGIN` → violation, comment line → clean, and an
  `ASYNCEVENTS_READY` line → **clean** (the boundary-check regression case))
  mirroring the existing test style.

**(b) Why now / order:** before the gateway refactor (Step 5) so the nets are in
place while the riskiest step lands; no dependency on Step 1.

**(c) How:** all four slot into existing structures: C and D are conditionals in
the existing `for pkg` loop reusing the dev-kind check idiom (fact 1); E is a
classify arm + reuse of the existing forbid-list loop; F is a copied walk fn. No
verify wiring needed — the fortress stage's unparameterized `cargo run -q -p
archcheck` picks up new rules automatically (research-confirmed).

**(d) Dispatch:** `[sonnet]` — additive to existing loops, fully specified.

---

### Step 3 — dedupe `monolith_modules()` into `tools/checkmodules`  `[sonnet]`
**(a) What:**
- New crate `tools/checkmodules` (lib, `publish = false`): single
  `pub fn monolith_modules() -> Vec<Box<dyn lifecycle::Module>>` — the exact
  12-entry body from fact 4, with a doc comment: *the module set both checker
  harnesses run; MUST track `cmd/server`'s list (minus core-infra `metrics`,
  which has no requires/topics/schema and would add nothing to either harness).
  When adding a module: cmd/server, the svc main, split-proof, AND this list.*
- Root `Cargo.toml`: add `"tools/checkmodules"` to members + a workspace alias.
- `tools/topiccheck` and `tools/requirecheck`: delete both local
  `monolith_modules()` fns; dep on `checkmodules`; their Cargo.tomls DROP the 12
  direct module deps (checkmodules carries them). **Keep everything else**: the
  five events-crate deps in topiccheck (`defined_topics()` at main.rs:121-148
  references them directly, outside the list fn) and both tools'
  `bus`/`lifecycle`/`sqlx`/`registry`/`tokio`/`async-trait`/`anyhow` deps are
  untouched by the extraction (module symbols appear ONLY in the list fns —
  reviewer-verified).
- `cmd/server/src/main.rs`: one-line comment above the `mods` vec pointing at
  `tools/checkmodules` (the residual manual link, stated instead of hidden).

**(b) Why now / order:** after Step 2 (archcheck edits) to avoid two agents in
`tools/` concurrently; before Step 5 so the harnesses that will re-verify the
gateway refactor are single-sourced.

**(c) How:** decision made — a shared crate, NOT an equality assertion against
`cmd/server` (a bin crate can't be imported; parsing its source would be a fragile
second-order checker). The residual risk (new module added to server but not the
shared list) drops from two silent misses to one, and the doc comment names it.

**(d) Dispatch:** `[sonnet]` — mechanical extraction.

---

### Step 4 — `asyncevents::testing` outbox helpers + migrate 5 test sites  `[sonnet]`
**(a) What:**
- `core/asyncevents/src/lib.rs`: new `pub mod testing` (adjacent to the existing
  free `transport()` helper, ~line 400) with two async fns:
  - `pub async fn outbox_count(pool: &PgPool, topic: &str, payload_key: &str, payload_value: &str) -> sqlx::Result<i64>`
    → `SELECT count(*) FROM asyncevents.outbox WHERE topic = $1 AND payload->>$2 = $3`
    (note: `payload->>$2` with the key as a BIND PARAM — valid in Postgres, keeps
    one prepared shape for all callers).
  - `pub async fn cleanup_outbox(pool: &PgPool, payload_key: &str, payload_value: &str) -> sqlx::Result<u64>`
    → `DELETE FROM asyncevents.outbox WHERE payload->>$1 = $2` returning
    rows-affected. (Config's topic-scoped DELETE also matches this shape — the
    extra topic filter there was belt-and-suspenders on a namespace-unique key;
    dropping it is behavior-safe because the payload key is unique per test run.)
  - Doc: *test-only helpers — the single owner of the plane's physical table name
    outside the plane itself.* NOT behind `#[cfg(test)]` (cross-crate dev-deps
    can't see test-gated items); plain pub fns in a `testing` module, like the
    existing `transport()` helper.
- Migrate the 5 sites (fact 9) to the helpers; delete their local SQL strings.
  Do NOT touch config's `ensure_asyncevents_schema`/`DB_SERIAL` machinery (fact 10).

**(b) Why now / order:** independent; grouped before the gateway step so all
checker/test infrastructure is settled before the risky refactor.

**(c) How:** fully specified; the one subtlety (bind-param JSON key; no
`#[cfg(test)]`) is decided above.

**(d) Dispatch:** `[sonnet]`.

---

### Step 5 — gateway: topology out of the module, into `cmd/*`  `[opus]`
**(a) What:** remove both in-module env reads (facts 5, 6).
- `core/opsapi/src/lib.rs`: new contrib slot + value type —
  `pub const PEER_SLOT: &str = "opsapi.peers";` and
  `#[derive(Clone)] pub struct PeerAddr { pub provider: String, pub addr: String }`
  (transport-free: name + addr only, no client/CA types). **`addr` is a `String`,
  NOT `SocketAddr`** — the stub stores the address as an unparsed string
  (`EdgeDialer.peer`, core/remote/src/lib.rs:208,267-274) and the lazy parse is a
  documented contract there ("a bad `*_EDGE_ADDR` surfaces as an `Unavailable`
  error … not a construction-time panic", lib.rs:205-207). An eager
  `SocketAddr` would turn a bad addr into a startup failure in every
  stub-wiring process (incl. admin-svc's six stubs). The parse stays in
  `remote_caller`, preserving the `Error::unavailable` taxonomy verbatim.
  `derive(Clone)` is required — `contributions<T: Clone>` demands it.
- `core/remote/src/lib.rs`: `remote::Stub::init` additionally contributes
  `PeerAddr { provider, addr }` (the string it already holds) to `PEER_SLOT`.
  Unread contributions in non-gateway processes sit inert in the `Slots` map
  (core/contrib/src/lib.rs:43-47) — harmless.
- `modules/gateway/src/lib.rs`:
  - `RouteTable::build` collects `PEER_SLOT` contributions into
    `peers: HashMap<String, SocketAddr>` (alongside its existing slot reads).
  - `remote_caller` (lib.rs:438-460): replace the `{PROVIDER}_EDGE_ADDR` env
    lookup + parse with a `peers.get(provider)` lookup; error message becomes
    *"no peer contributed for provider X (wire a remote::Stub in this process's
    main)"*. Keep `shared_dev_ca()` + `dial` + the cache/eviction exactly as-is.
  - `Gateway` builder: new `with_passthrough(prefix: &str, origin: &str)`
    (Vec-accumulating); `FrontDoor::new` takes the collected routes and
    `ProxyTable::from_routes(routes)` replaces `ProxyTable::from_env()`.
    `proxy.rs`: keep `normalize_origin` + longest-prefix sort; delete `from_env`.
- `cmd/gateway-svc/src/main.rs`: build the gateway as
  `gateway::Gateway::new().with_player_edge(player)` + passthroughs from env —
  `.with_passthrough("/admin", &env_addr("ADMIN_HTTP_ADDR", ""))` guarded on
  non-empty (mirror `from_env`'s skip-empty semantics), same for
  `("/accounts/epic", ACCOUNTS_HTTP_ADDR)`. The five `remote::Stub`s already
  carry the edge addrs (fact 7) — no new env needed for op dispatch.
- `cmd/server/src/main.rs`: unchanged wiring (no stubs → no peers → all ops Local;
  no passthrough → ProxyTable empty), matching today's monolith behavior where
  `from_env` finds no `*_HTTP_ADDR`.
- Tests: `cmd/gateway-svc/tests/stub_swap.rs` gains/updates a case asserting a
  `PeerAddr` contribution resolves Remote dispatch; gateway unit tests for
  `from_routes` prefix precedence. **Plus two EXISTING gateway tests that assert
  the env-based error text and WILL break** (reviewer): 
  `modules/gateway/src/tests.rs:425-429` (body contains `GHOSTPROV_EDGE_ADDR`)
  and `:538-543` (eviction re-dial asserts `FAKEPROV_EDGE_ADDR` in the error) —
  rewrite both to seed a `PeerAddr` contribution (or assert the new
  "no peer contributed" message) instead of setting env vars.

**(b) Why now / order:** the riskiest diff, so it goes after all nets (Steps 2-4)
are green; it must pass the new archcheck rules and the single-sourced harnesses.

**(c) How (non-mechanical):**
- **The lazy `OnceLock` route-table build is load-bearing — do NOT make it
  eager.** `RouteTable` is built on FIRST REQUEST (gateway lib.rs:39-44,195-198);
  that is the only reason `Stub::init` contributions (init order unspecified
  relative to gateway) are all present when the table reads `PEER_SLOT`. An
  eager build in `Gateway::init` would race module init order.
- The slot idiom is the sanctioned pattern for exactly this (EDGE_SLOT precedent):
  the module consumes contributions, the composition root decides what exists.
  `PeerAddr` lives in `opsapi` because both `remote` (core) and `gateway` (module)
  already import it, and it is the ops vocabulary crate — adding it to `remote`
  would force gateway→remote, a new module→core edge that archcheck permits but
  we don't need.
- Behavior invariants to preserve verbatim: lazy dial on first Remote call,
  per-provider cache, eviction-on-failure pointer-identity dance
  (lib.rs:415-428), `Error::unavailable` taxonomy, empty-addr skip semantics for
  passthrough.
- `EVENTS_`-tripwire note: `*_EDGE_ADDR`/`*_HTTP_ADDR` strings must be GONE from
  `modules/gateway` after this step — grep-verify as part of the step's
  self-check (`grep -rn "_EDGE_ADDR\|_HTTP_ADDR" modules/` → zero hits).
- Scripts: run/split-proof env blocks keep setting `ADMIN_HTTP_ADDR`/
  `ACCOUNTS_HTTP_ADDR`/`*_EDGE_ADDR` for gateway-svc — same vars, now consumed by
  the main instead of the module. No script change in this step.
- **Split-proof is MANDATORY for this step** (cross-process dispatch changed):
  `./split-proof.sh` full run, not just cargo tests.

**(d) Dispatch:** `[opus]` — seam redesign, but every decision above is pinned;
Opus with the full spec is the right cost point. (Bump to `[fable]` only if the
user prefers top tier for the one seam step.)

---

### Step 6 — cruft + docs sweep  `[sonnet]`
**(a) What:** all coordinates in fact 11/12:
- Delete dead `ACCOUNTS_EDGE_ADDR` exports from the five non-front svc blocks in
  run.sh / run.ps1 / split-proof.sh / split-proof.ps1 (keep gateway + admin).
- Fix "four peers"→"six peers (A/B/C/D + audit + scheduler)" in
  cmd/admin-svc/src/main.rs:8-10, split-proof.sh:398, split-proof.ps1:377.
- `core/bus/src/lib.rs:313-315`: reword `event_id` doc to *"stable opaque
  idempotency key — treat as an opaque string; the plane owns its composition"*
  (format string removed from the module-facing doc; it stays documented in
  core/outbox where it is minted).
- Drop `adminapi` from api/accounts/api/Cargo.toml:19 and
  api/characters/api/Cargo.toml:19 (dead post-refactor deps, audit-verified
  comment-only references).
- CLAUDE.md: (i) match blurb — distinguish `/match/report` request body keys
  (`Winner`/`Loser`) from the snake_case `match.finished` payload; (ii) add the
  router blessing to constraint 10's neighborhood: *"one shared HTTP framework
  (axum) — `ctx.mount(Router)` is the sanctioned surface for the four
  HTTP-surface owners (webui, admin, accounts-OAuth, gateway)"*; (iii) update the
  gateway bullet: peer addressing now injected by `cmd/*` (PEER_SLOT/builder),
  drop "env-addressed peers" phrasing for op dispatch.
- Run `./run.sh` smoke or split-proof once after the script edits (they are
  load-bearing test infra).

**(b) Why now / order:** last — the CLAUDE.md gateway wording depends on Step 5
having landed.

**(c) How:** fully enumerated; zero judgment.

**(d) Dispatch:** `[sonnet]`.

---

## Verification

- Per step: `cargo test -p <touched crates>`; `cargo run -p archcheck`,
  `-p requirecheck -- --strict`, `-p topiccheck -- --durability-strict` after
  Steps 2/3; deliberate-violation spot-checks for each new archcheck rule
  (add sqlx to bus locally → FAIL → revert).
- Step 5: full `./split-proof.sh` (cross-process op dispatch + passthrough
  scenarios already asserted there) + monolith parity leg.
- Final: `./verify.sh --fast` green; `./verify.sh --all` for the advisory tiers.

## Commit plan
One commit per step, conventional scopes:
1. `feat(schedulerevents,audit,scheduler): shared audit-prune schedule-name constant + linking test`
2. `feat(archcheck): nets for bus-sqlx-free, module→asyncevents runtime dep, events transport-freedom, EVENTS_ env tripwire`
3. `refactor(topiccheck,requirecheck,checkmodules): single-source the harness module list`
4. `feat(asyncevents): testing outbox helpers; module tests drop hardcoded plane SQL`
5. `refactor(gateway,opsapi,remote,cmd): peer topology via PEER_SLOT contributions — env reads leave the module`
6. `chore(scripts,docs,api): dead env exports, six-peer docs, opaque event_id doc, dead adminapi deps, CLAUDE.md blurbs`

Trailers per executing model: `[sonnet]` → Claude Sonnet 4.6, `[opus]` → Claude
Opus 4.8.

## Open decisions for the user
- Step 5 lane: `[opus]` (planned) vs `[fable]` for the one seam step.
- Step 4 `cleanup_outbox` drops config's extra topic filter (behavior-safe per
  plan; flag if you want the topic-scoped variant kept as a second fn).
