# Security-review hardening plan — findings 5/7/8/9/11/12 (+#10 closure note)

Date: 2026-07-10. Source: `docs/reviews/2026-07-09-architecture-security-review.md`.
Research: 6 parallel subagents (lifecycle/app, gateway, archcheck, dev-env flags,
module catalogs, rustls-pemfile) — all reported grep+full-file-read as method (no
rust-analyzer available to subagents); key files were read end-to-end, so coverage
is high despite the grep lower bound.

## Context — what already exists, why not extend X

- **Finding 10 (module-catalog drift) is ALREADY CLOSED** — research falsified the
  review's claim: `tools/checkmodules/src/lib.rs:24-29` calls `server::modules()`
  from `cmd/server/src/lib.rs:37-51` directly (no hand-mirrored list); the Split
  profile calls every real `cmd/<name>-svc` lib. Nothing to build. This plan only
  records the closure in the review addendum (Step 7).
- **Gateway duplicate detection (Step 2)** extends the existing `RouteTable::build`
  seam — no new checker tool, because the collision is only observable where all
  slot contributions meet, which is exactly `RouteTable::build`. `contrib::Slots`
  itself stays append-only (a multi-value slot legitimately holds duplicates for
  other slot types, e.g. `EDGE_SLOT` regs — uniqueness is an *ops-table* invariant,
  not a slots invariant).
- **Archcheck foreign-schema rule (Step 5)** extends `tools/archcheck` — the
  near-twin is the existing `grep_asyncevents_sql` rule (`main.rs:950-978`); we
  generalize its shape rather than adding a new tool.
- **Dev-defaults flip (Step 4)** replicates the codebase's own best pattern —
  gateway `resolve_verifier`/`resolve_key_verifier` (explicit-only + bail) and
  `APIKEYS_DEV_SEED`'s `dev_seed_explicitly_on()` — onto the two remaining
  default-ON module flags; no new config system (per the config-as-code agreement,
  env stays typed reads in modules, conveniences move into startup scripts).
- **Startup unwind (Step 3)** extends `lifecycle::App` + `core/app::run` — every
  component already exposes an idempotent stop (`Plane::stop`,
  `InvalidationPlane::stop`, `RunningServer::close`, `Bus::close`, `App::stop`);
  the work is sequencing them onto the error paths, not building new teardown.

Research facts the steps rely on:

- `App` keeps NO started-state (`core/lifecycle/src/app.rs:12-16`); `App::start`
  fails fast with no rollback (`:82-92`); `App::stop` is best-effort reverse-order
  (`:97-108`), returns `()`.
- `core/app/src/lib.rs run()` has these fallible points after `app.start()`
  (line 361), none of which reach the happy-path teardown block (`:495-516`):
  asyncevents `p.start()` :363, invalidation `p.start()` :369, edge CA :384,
  edge addr parse :387, edge listen :388, player CA :403, player addr parse :406,
  player listen :407, CIDR parse :457, HTTP bind :480, serve :493. Earlier:
  `app.build()` :306, asyncevents migrate :358, `app.migrate()` :360.
- Caps↔impl consistency is currently perfect across all 12 modules + metrics +
  webui + `remote::Stub` (`core/remote/src/lib.rs:298-300`, caps
  REGISTER|START|STOP). One extra `Caps` consumer outside lifecycle:
  `tools/requirecheck/src/main.rs:187`.
- `RouteTable::build` (`modules/gateway/src/lib.rs:433-462`) collects
  `BINDING_SLOT`/`LOCAL_SLOT`/`PEER_SLOT` into `HashMap`s via `.collect()` —
  **last write wins** — while the `SLOT` operations `Vec` is scanned
  first-match-wins in `find`/`find_by_method`. A duplicate method id therefore
  produces a *hybrid*: Operation from the first contributor, decoder/invoker from
  the last. The table is built **lazily on first request**
  (`FrontDoor::table`, `:266-269`, `OnceLock`) because `Gateway::init` (phase 2)
  runs before later modules contribute — eager validation must live in a `start`
  phase (runs after ALL inits), which gateway currently doesn't have.
- Pattern equality must be wildcard-name-blind: `parse_pattern` (`:775-783`)
  yields `Seg::Lit`/`Seg::Wild(name)` and `match_pattern` (`:787-805`) never reads
  the wild name — `/char/{id}` and `/char/{name}` match identical request sets.
  `Seg` currently derives nothing.
- Schema inventory: 10 modules own exactly one schema named after their dir;
  `admin` and `gateway` own none; core owns `asyncevents` only. False-positive
  minefield for a naive dotted-token scan: topic strings (`config.changed`,
  `match.finished`), method ids (`characters.create`, `accounts.login` — several
  prefixes ARE real schema names), and own-schema SQL (`characters.characters`).
- Dev flags: `ACCOUNTS_DEV_AUTH` default ON (`modules/accounts/src/lib.rs:381`),
  `INVENTORY_DEV_GRANT` default ON (`modules/inventory/src/lib.rs:643`), admin
  open when `ADMIN_USER` empty (`modules/admin/src/lib.rs:77-83`). Almost no
  Rust test reads these env vars directly — EXCEPT accounts' `wired()` fixture
  (`modules/accounts/src/tests.rs:266-276`), which calls `Accounts::register`
  and therefore rides the `env_bool("ACCOUNTS_DEV_AUTH", true)` default; six
  live-DB tests depend on it (`tests.rs:306,362,389,415,444`,
  `tests/dev_auth_gate.rs:77`). Also: **topiccheck and requirecheck run real
  module `init`** (topiccheck via `app.build()` at
  `tools/topiccheck/src/main.rs:287`; requirecheck's manual phase loop at
  `tools/requirecheck/src/main.rs:193-196`) — any new init-time bail must not
  break them. Scripts: `split-proof.sh:117-118,416` already sets
  ADMIN_USER/PASS explicitly (the target pattern); `APIKEYS_DEV_SEED=1` is set
  explicitly in `run.sh:120-126,176-182`, `split-proof.sh:268,1121`,
  `split-proof.ps1:240,1001`; `ACCOUNTS_DEV_AUTH`/`INVENTORY_DEV_GRANT` appear in
  NO script (they ride the module default today). **Discovered bug:** `run.ps1`
  is stale — it has NO apikeys-svc leg at all and never sets `APIKEYS_DEV_SEED`.
- rustls-pemfile: direct dep in root `Cargo.toml:156` + `core/edge/Cargo.toml:19`;
  exactly 2 call sites, both `rustls_pemfile::certs(...)` iterator-first-item, in
  `core/edge/src/tls.rs:91` (`DevCA::load`) and `:263` (`load_cert_only`).
  `rustls-pki-types 1.15.0` already in tree; its `pem` feature
  (`CertificateDer::pem_slice_iter` / `pem_file_iter`) is the replacement.
  cargo-audit ignore arrays: `verify.sh:151`, `verify.ps1:56`.

Banned-phrase check: every step below names exact files/symbols; no TBDs.

---

## Step 1 — Delete `Caps`; call all phases unconditionally (finding 9) `[sonnet]`

**(a) What:**
- `core/lifecycle/src/module.rs` — delete `Caps` struct (lines 3-30) and
  `Module::caps()` (:64-67); keep all default no-op phase impls.
- `core/lifecycle/src/app.rs` — `build()` :53, `migrate()` :70, `start()` :84,
  `stop()` :99: drop the `m.caps().contains(...)` guards, call phases
  unconditionally.
- `core/lifecycle/src/lib.rs` — remove `Caps` re-export.
- Remove `caps()` overrides + `Caps` imports from: accounts (:362-364),
  apikeys (:129-131), audit (:303-305), characters (:361-363), config (:571-576),
  inventory (:537-539), leaderboard (:124-126), match (:163-165),
  rating (:147-149), scheduler (:327-329), `core/remote/src/lib.rs:298-300`.
  (admin/gateway/webui/metrics have no override — untouched.)
- `tools/requirecheck/src/main.rs:187` — call `m.register(&ctx)` unconditionally
  (drop the `Caps::REGISTER` branch), keep the rest.
- `core/lifecycle/src/tests.rs` — rewrite `plain_module_only_inits` (:119-143):
  its intent ("phases never invoked without caps") is obsolete; replace with a
  test asserting a default-impl module passes the full
  build/migrate/start/stop cycle on a DB-less `Context` without error (guards the
  "defaults are true no-ops" property this step depends on). `RecMod` drops its
  `caps()`.
- Stale-doc sweep: `api/config/rpc/src/lib.rs` comment mentioning `Caps::START`;
  `module.rs` phase-discipline doc comment ("Opt in via Caps::…" lines).

**(b) Why now / order:** first because Steps 2 and 3 both touch the phase
machinery: Step 2 adds a gateway `start()` (no cap bit needed once this lands)
and Step 3 rewrites `App::start` error handling — doing Caps removal after them
would churn the same lines twice.

**(c) How (non-mechanical bits):** the ONLY behavior change is that default
no-op phases now execute (verified no default touches `ctx`); config's own
`ctx.db().ok_or_else(...)` guards (:582-586, :606-609) already protect the
DB-less case independently of Caps. `migrate` logging in `App::migrate` should
move below the call and stay per-module (log noise for no-op modules is
acceptable; do NOT add a "did it do anything" heuristic).

**(d) Verify:** `cargo test --workspace` (one invocation), `cargo clippy
--workspace --all-targets -- -D warnings`, `cargo run -p archcheck`,
`cargo run -p requirecheck -- --strict`. No split-proof here — Steps 1+2 land
back-to-back and Step 2's split-proof run covers both (do not pause the
rollout between them).

## Step 2 — Gateway collision detection at startup (finding 8) `[opus]`

**(a) What:** `modules/gateway/src/lib.rs` (`RouteTable`, `FrontDoor`,
`Gateway`), `modules/gateway/src/tests.rs`.

**(b) Why now / order:** after Step 1 so `Gateway::start` needs no `Caps::START`
declaration.

**(c) How:**
- Change `RouteTable::build(slots: &Slots) -> RouteTable` to
  `-> anyhow::Result<RouteTable>`, building the three HashMaps with explicit
  loops + occupancy checks instead of `.collect()`:
  duplicate `OpBinding.method`, duplicate `LocalOp.method`, duplicate
  `PeerAddr.provider` (same provider, different addr — identical re-contribution
  of the same addr may stay an error too: simplest strict rule, no allowlist),
  duplicate `Operation.method`, and duplicate verb+pattern → descriptive
  `bail!` naming both contributions.
- Verb+pattern equality: derive nothing on `Seg`; write
  `fn pattern_shape_eq(a: &[Seg], b: &[Seg]) -> bool` — equal length, `Lit`s
  compare by string, ANY `Wild` equals ANY `Wild` (wildcard-name-blind, matching
  `match_pattern` semantics). Verb compares `eq_ignore_ascii_case`.
- `FrontDoor::table()` (`:266-269`): `OnceLock<Arc<RouteTable>>` stays for the
  request path, but `get_or_init` can no longer swallow an `Err` — give
  `FrontDoor` a `fn build_table(&self) -> anyhow::Result<Arc<RouteTable>>` used
  by both a new eager call and the lazy path (lazy path `expect`s: by then the
  eager validation has already passed in every process that runs `start`).
- New `Gateway::start` (async, after Step 1 runs unconditionally): calls
  `self.front_door.build_table()?` — phase `start` runs after ALL module inits,
  so every contribution is present; a collision becomes a loud startup failure
  in BOTH topologies (monolith `cmd/server` and `cmd/gateway-svc` both host the
  gateway module).
- While in the file: fix the stale module-doc header (`lib.rs:1-2`) claiming the
  gateway is "present in EVERY `app::run` process" — reality is front-door-only
  (`cmd/server` + `cmd/gateway-svc`, archcheck-enforced).
- External `RouteTable::build` callers: exactly two, both in
  `modules/gateway/src/tests.rs` (:349, :795) — update for the `Result` return.
- Tests (extend `modules/gateway/src/tests.rs`, reusing `demo_opset()`/
  `Slots::new()` helpers): duplicate method id → Err naming the method;
  duplicate verb+path with differently-named wildcards (`/char/{id}` vs
  `/char/{name}`) → Err; same-shape different-lit paths → Ok; duplicate peer
  provider → Err; the existing happy-path tests updated for the `Result` return.

**(d) Verify:** cargo test + clippy; `./split-proof.ps1` (proves no false
positive across the real 12-process contribution set + monolith parity leg).

## Step 3 — Startup unwind on partial start (finding 5) `[fable]`

**(a) What:** `core/lifecycle/src/app.rs` (`App::start`),
`core/app/src/lib.rs` (`run`, lines ~306-516), `core/lifecycle/src/tests.rs`,
`core/app/src/tests.rs`.

**(b) Why now / order:** after Steps 1-2 — `App::start` is rewritten here and
Step 1 already removed its caps guard; Step 2 added a gateway `start` that can
now fail, making unwind reachable in practice (a gateway collision must not
strand the scheduler's advisory lock or the asyncevents workers).

**(c) How:**
- `App::start`: track the started prefix; on module N failing, call `stop` on
  modules N-1..0 (reverse), log-and-continue per module (same best-effort policy
  as `App::stop`), then return the original error. Modules whose `start` never
  ran do NOT get `stop` — a documented-contract choice for symmetry (current
  module stops are `Option::take`-guarded and would tolerate it — scheduler's
  stop :432-441 included — but the contract should not RELY on that).
  Implementation: iterate with index, on error `for m in
  self.modules[..i].iter().rev() { … }`. Update the `Module` trait doc comment
  to state this guarantee.
- **Double-stop rule:** when `app.start()` itself returns Err (:361), the run()
  unwind must SKIP `app.stop()` — `App::start` has already stopped its started
  prefix internally, and calling `App::stop` after Step 1 would run `stop` on
  every module including never-started ones. `app.stop()` in the unwind applies
  only to failures AFTER a successful `app.start()`.
- `core/app/src/lib.rs run()`: restructure the post-`app.build()` body so every
  error path performs ordered teardown. Concretely: extract the fallible
  sequence between module start and HTTP serve into an inner async block whose
  `Err` arm runs the unwind, mirroring the existing happy-path teardown order
  EXACTLY as written at `:495-516` (listeners closed, plane `p.stop()`, then
  `invalidation.stop()`, `app.stop()` — subject to the double-stop rule above —
  then `ctx.bus().close()`; read the block and copy its order rather than this
  sentence). Both planes' `stop` are
  documented idempotent (`Option::take` guard), so the unwind may
  over-approximate ("stop whatever exists") for the planes while listeners are
  closed only when `Some`. Failures BETWEEN `app.build()` and `app.start()`
  (asyncevents migrate :358, `app.migrate()` :360) unwind with `app.stop()`
  skipped (no module started) but bus close still performed. The exact shape
  (guard struct with owned `Option`s vs labelled block) is the implementer's
  choice; the invariant that must hold is: **teardown order on every error path
  == the happy-path teardown order, truncated to what was actually
  created/started.**
- Tests: lifecycle test — module B's `start` fails → A (started before B) gets
  `stop`, C (never started) does not; recorded order asserted. app test — reuse
  `core/app/src/tests.rs` style; unit-test the extracted unwind helper if the
  chosen shape allows (no live QUIC needed).

**(d) Verify:** cargo test + clippy; `./split-proof.ps1` (boots 12 processes —
regression-proves the happy path unchanged).

## Step 4 — Dev-defaults fail-closed; conveniences move to scripts (finding 11) `[opus]`

**(a) What:** `modules/accounts/src/lib.rs:381` (+warn copy :418-423),
`modules/accounts/src/tests.rs` (`wired()` fixture),
`modules/inventory/src/lib.rs:643` (+warn copy :645-648),
`modules/admin/src/lib.rs:77-83` (+`Admin::init`, +tests),
`tools/topiccheck/src/main.rs`, `tools/requirecheck/src/main.rs`,
`run.sh`, `run.ps1`, `split-proof.sh`, `split-proof.ps1`, `verify.sh`,
`verify.ps1`, `CLAUDE.md` (accounts/inventory/admin module blurbs + smoke-test
block :251-253), `AGENTS.md` (mirrors CLAUDE.md).

**(b) Why now / order:** independent of Steps 1-3 code-wise, but sequenced after
them so split-proof runs in this step exercise the new unwind/collision code
paths end-to-end while flags churn.

**(c) How:**
- `ACCOUNTS_DEV_AUTH`: flip `env_bool("ACCOUNTS_DEV_AUTH", true)` → `false` at
  `modules/accounts/src/lib.rs:381`; rewrite the warn (now: warn when ON, since
  OFF is silent default). Gateway-side `dev_auth_explicitly_on()` is already
  explicit-only — untouched.
- `INVENTORY_DEV_GRANT`: same flip at `modules/inventory/src/lib.rs:643`; the
  op-filter at :649-658 needs no change.
- Admin: empty `ADMIN_USER` becomes a **startup failure** in `Admin::init`
  (`anyhow::bail!("admin: set ADMIN_USER/ADMIN_PASS or ADMIN_OPEN=1 for a
  deliberately open local portal")`) unless new `ADMIN_OPEN` is explicitly
  truthy — implement `admin_open_explicitly_on()` copying the
  `dev_seed_explicitly_on()` idiom (`modules/apikeys/src/lib.rs:212-217`,
  `"1"/"true"/"on"` case-insensitive). `ADMIN_OPEN=1` keeps the loud warn.
  Admin unit tests keep injecting `auth_user` literals (`tests.rs:24-25`) — add
  one test for the bail and one for the `ADMIN_OPEN` path.
- **Checkers (reviewer BLOCKER):** topiccheck (`app.build()` at
  `tools/topiccheck/src/main.rs:287`) and requirecheck (phase loop at
  `tools/requirecheck/src/main.rs:193-196`) run real `Admin::init` with no env
  set and would die on the new bail — both run in verify (requirecheck under
  `--strict` is blocking). Fix in the checker mains: set `ADMIN_OPEN=1` via
  `std::env::set_var` at the top of each `main()` with a comment ("checkers
  build module graphs, they serve no HTTP — open-admin is meaningless here").
  The accounts/inventory flips need nothing there (feature-off, not bail).
- **Accounts tests (reviewer MAJOR):** `wired()`
  (`modules/accounts/src/tests.rs:266-276`) calls `Accounts::register(&ctx)`
  and rides the env default; after the flip, register/login return `NotFound`
  in six tests. Patch the fixture to force dev-auth ON explicitly (set
  `ACCOUNTS_DEV_AUTH=1` for the test process via a fixture-local
  `std::env::set_var` guard, or restructure `wired` to build `Service` with
  `dev_auth: true` directly like `dev_auth_gate.rs:15-24` does — prefer the
  struct route, no env mutation in tests). Update the stale comment at
  `dev_auth_gate.rs:76`.
- Scripts — set explicitly on every process that hosts the module (env-prefix
  pattern in `.sh`, `Start-Svc` hashtable in `.ps1`), following the existing
  `ADMIN_USER`-in-split-proof precedent:
  - `run.sh` / `run.ps1` monolith leg: `ACCOUNTS_DEV_AUTH=1`,
    `INVENTORY_DEV_GRANT=1`, `ADMIN_USER=admin`, `ADMIN_PASS=admin` (overridable
    `${VAR:-default}` style like the existing `APIKEYS_DEV_SEED` line).
  - `run.sh` split legs: `ACCOUNTS_DEV_AUTH=1` on accounts-svc,
    `INVENTORY_DEV_GRANT=1` on inventory-svc, ADMIN creds on admin-svc.
  - `split-proof.sh` / `split-proof.ps1`: `ACCOUNTS_DEV_AUTH=1` on accounts-svc
    (A-leg) and the monolith stage (`[M0]` calls `/accounts/register` —
    `split-proof.sh:1126-1136`); `INVENTORY_DEV_GRANT=1` on inventory-svc and
    monolith. ADMIN creds exist on the admin-svc leg ONLY (`:416`) — the
    **monolith-parity stage** (`split-proof.sh:1118-1122`, `.ps1` twin) hosts
    the admin module too and must gain `ADMIN_USER`/`ADMIN_PASS` (reviewer
    BLOCKER — without it the parity leg fails to boot).
  - `verify.sh:364-365` / `verify.ps1:344-352` (csharp stage's self-contained
    monolith): add `ADMIN_USER`/`ADMIN_PASS` and `ACCOUNTS_DEV_AUTH=1` (its
    `gbclient flow` does register→create→list) + `INVENTORY_DEV_GRANT=1` for
    symmetry.
  - **Fix `run.ps1` staleness while in the file:** add the missing apikeys-svc
    leg (build + `Start-Svc` + `APIKEYS_DEV_SEED='1'`) mirroring `run.sh:176-182`
    — without it the PS split path boots a gateway whose apikeys capability is
    absent, which after this step's philosophy (and current gateway code,
    `keys.rs:233-238`) is a startup failure, i.e. `run.ps1` is broken today and
    this step would surface it.
- Docs: CLAUDE.md accounts blurb "default ON + loud warn" → "explicit-only
  (`ACCOUNTS_DEV_AUTH=1`), set by run/split-proof scripts"; same for inventory;
  admin "open + warn when unset" → "fails startup unless ADMIN_USER/ADMIN_PASS
  or explicit ADMIN_OPEN=1"; smoke-test block gains
  `ACCOUNTS_DEV_AUTH=1 INVENTORY_DEV_GRANT=1` alongside `APIKEYS_DEV_SEED=1`.

**(d) Verify:** cargo test + clippy; `cargo run -p topiccheck` and
`cargo run -p requirecheck -- --strict` (the regression gate for the admin
bail); `./split-proof.ps1` AND `bash split-proof.sh` if runnable on this box
(both script families changed); manual monolith smoke per updated CLAUDE.md
block; `./run.ps1` boot check for the new apikeys leg.

## Step 5 — archcheck: foreign-schema SQL tripwire (finding 7, scoped) `[opus]`

**(a) What:** `tools/archcheck/src/main.rs` (+`tests.rs`). Explicitly NOT
DB-role isolation — this is the agreed cheap tripwire; runtime enforcement
stays deferred.

**(b) Why now / order:** independent; last of the code steps because it's pure
tooling with no runtime coupling to Steps 1-4 (but after Step 4 so its test
fixtures don't collide with script churn in review).

**(c) How:**
- Schema set: reuse the fortress dir scan (`crate_dirs`, `main.rs:465-474`) —
  schemas = module dir names (research-confirmed 1:1 for all 10 persisting
  modules; `admin`/`gateway` own none and simply never match) + `"asyncevents"`
  handled by the EXISTING rule (unchanged).
- New helper `foreign_schema_sql_refs(line: &str, own: &str, schemas:
  &[String]) -> Vec<String>`: for each schema `s != own`, scan the line for the
  token `s` followed by `.` (via `find_all` + `contains_boundary_checked`-style
  left-boundary), and report ONLY when the match is immediately preceded
  (ignoring whitespace) by one of the SQL context keywords:
  `FROM`, `JOIN`, `INTO`, `UPDATE`, `DELETE FROM`, `TABLE`, `EXISTS`
  (case-insensitive). This kills the method-id/topic false positives
  (`'config.changed'` inside `append_event(...)` has no preceding SQL keyword;
  `"characters.create"` likewise) while catching real query text
  (`FROM inventory.items`, `INSERT INTO rating.ratings`, `UPDATE config.settings`).
  `REFERENCES` stays owned by the existing cross-schema-FK rule (:564-605) —
  do not duplicate it.
- Driver `grep_foreign_schema_sql(root)`: walk `modules/<name>/src` per module
  (own = dir name), skip comment lines and test sources (`is_test_source`,
  same policy as `grep_asyncevents_sql` — tests may build cross-schema fixtures).
- Wire into `main()` as the next numbered rule; violations name file:line,
  offending schema, and the module.
- Tests (existing style — pure-function tests on the helper + one temp-dir walk
  test): positives (`FROM inventory.items` inside a characters module file;
  `INSERT INTO rating.ratings`), negatives (own-schema `FROM characters.characters`;
  topic literal `'config.changed'` as an append_event arg; method id
  `"accounts.login"` in a policy string; `match.finished` in a comment;
  `EXISTS (SELECT 1 FROM apikeys.keys` positive).
- Run against the real tree; if a legitimate hit surfaces (none expected —
  reviewer verified all non-test module SQL is own-schema today; the only
  cross-schema literals live in `modules/inventory/src/tests.rs:322,350,382`,
  excluded by `is_test_source`), it is a real finding to fix, not to allowlist.
- **Declared limitation (document in the rule's doc comment + one test):** the
  heuristic is line-scoped — a multi-line SQL string with the keyword at one
  line's end (`FROM\n  other.items`) escapes it. No such split exists in the
  tree today; the rule is a tripwire for accidental drift, not full coverage
  (that stays with the deferred DB-role isolation).

**(d) Verify:** `cargo test -p archcheck`, `cargo run -p archcheck` clean on the
real tree, clippy.

## Step 6 — rustls-pemfile → rustls-pki-types `pem` (finding 12) `[sonnet]`

**(a) What:** `core/edge/src/tls.rs:91-94` and `:263-266`, root
`Cargo.toml:156`, `core/edge/Cargo.toml:19`.

**(b) Why now / order:** fully independent; ordered last-but-docs because it's
a 15-minute mechanical swap.

**(c) How:** enable the `pem` feature on the existing `rustls-pki-types`
workspace dep (add to root `[workspace.dependencies]` if not yet listed there —
it is currently transitive-only, so it becomes a direct dep with
`features = ["pem"]`); replace both
`rustls_pemfile::certs(&mut reader).next()` call sites with
`CertificateDer::pem_slice_iter(pem_bytes).next()` (both sites parse from
in-memory PEM strings — `pem_slice_iter` fits; map the error into the existing
`anyhow` context). Delete `rustls-pemfile` from both Cargo.tomls; confirm
`cargo tree -i rustls-pemfile` comes back empty; `cargo audit` (with the
existing single ignore) must report no RUSTSEC-2025-0134. **Also:**
`core/edge/fuzz/Cargo.lock:649` pins rustls-pemfile independently (fuzz SKIPs
on this Windows box, so it lingers silently) — regenerate it
(`cargo update -p rustls-pemfile` / `cargo generate-lockfile` inside
`core/edge/fuzz/`, no build needed) or, if regeneration needs the fuzz
toolchain, note the deferral explicitly in the commit message.

**(d) Verify:** cargo test + clippy; `./split-proof.ps1` (the mTLS edge is the
changed surface — the split boot IS the cert-path integration test);
`cargo audit --ignore RUSTSEC-2023-0071` clean.

## Step 7 — Review addendum + memory sync `[inline]`

**(a) What:** prepend an addendum block to
`docs/reviews/2026-07-09-architecture-security-review.md` stating: findings
1/2/6 closed structurally by durable event log v2 (2026-07-10, push plane
deleted); finding 10 closed by Step 11's checkmodules single-sourcing
(falsified during 2026-07-10 research); findings 5/7(scoped)/8/9/11/12
addressed by this plan (link); finding 3 (shared CA) deliberately deferred to
the mini-orchestrator milestone; finding 4 partially addressed (admin
fail-open closed in Step 4; CSRF + hashed key storage remain open, deliberate
trust-model decision). Update the affected CLAUDE.md lines not already touched
in Step 4 (Caps mention in the lifecycle constraint if any). Run
`scripts/memory-sync.ps1 push` if memory files change.

**(b) Why now / order:** last — records what actually landed.

**(c) How:** plain doc edit, inline in the main context.

**(d) Verify:** none beyond reading it.

---

## Dispatch summary

| Step | Lane | Model arg | Trailer |
|------|------|-----------|---------|
| 1 Caps deletion | `[sonnet]` | `model:"sonnet"` | Claude Sonnet 4.6 |
| 2 Gateway collisions | `[opus]` | `model:"opus"` | Claude Opus 4.8 |
| 3 Startup unwind | `[fable]` | `model:"fable"` | Claude Fable 5 |
| 4 Dev-defaults + scripts | `[opus]` | `model:"opus"` | Claude Opus 4.8 |
| 5 archcheck SQL rule | `[opus]` | `model:"opus"` | Claude Opus 4.8 |
| 6 rustls-pemfile | `[sonnet]` | `model:"sonnet"` | Claude Sonnet 4.6 |
| 7 Review addendum | `[inline]` | — | Claude Fable 5 |

Effort embedded in subagent prompts: `[sonnet]` steps default effort; `[opus]`
steps "think"; `[fable]` step "think hard". Each subagent commits its own step
(Conventional Commits, its model's trailer); main context reviews each diff
against this plan before dispatching the next. Full verify (`./verify.ps1`)
after Step 6; split-proof runs are called out per step above.

Commits land directly on master (per the standing work-on-master agreement).

## Review

Grumpy-reviewer pass (Fable, think hard, separate context) 2026-07-10: verdict
SHIP WITH FIXES — 2 blockers (admin bail vs checkers; admin creds missing from
monolith legs of split-proof/verify), 2 majors (accounts `wired()` fixture
rides the env default; double-stop on the `app.start()` Err path), 6 minors.
All 10 items are folded into the step bodies above. Reviewer verified the
plan's line citations, the Caps consumer sweep, the finding-10 closure, and
the run.ps1 staleness claim against the real tree.
