> **Status / errata (2026-07-13): HISTORICAL EXECUTION RECORD.** The round remains
> executed as recorded below. Current operator commands have since cut over to
> `devctl` and `verifyctl`; references in the body to shell verification/run wrappers
> describe the implementation at that time and are not current instructions. Later
> authority changes are tracked in
> `2026-07-12-1214-architecture-remediation-rust-tooling-plan.md`.

# Remediation Round 4 — audit findings, authority-point hardening

**Status:** EXECUTED 2026-07-12 (all 16 steps landed; see
`docs/status/2026-07-12-0941-remediation-round4-summary.md`).
**Review:** grumpy-reviewer pass (Fable, think hard, separate context) done
2026-07-11; both MAJORs fixed (Step 4 dedicated-conn source →
`PgConnection::connect_with(pool.connect_options())`; Step 12 `octet_length`
byte semantics) and all MINORs addressed in place (poll-not-assert advisory
test, skip-not-floor tick budget, routecheck-panics doc, 408 comment, Step 14
FAIL-branch test case, exact test/skill paths, Step 9 bless scope).
**Goal:** fix all 14 findings from the 2026-07-11 external audit AND close the
meta-problem it named — checkers guarding a *representation* of an invariant
instead of the *authority* — so that new modules copy healthy patterns instead
of replicating these bugs.

## Context — research synthesis (10 read-only subagents, 2026-07-11)

Every finding was re-verified against HEAD with exact quotes. Verdicts:

| # | Finding | Verdict | Key nuance from research |
|---|---------|---------|--------------------------|
| 1 | Gateway dial under global mutex, no dial deadline | VERIFIED | `RouteTable.remotes` is ONE `tokio::sync::Mutex<HashMap<provider, Arc<dyn Caller>>>` held across `edge::Client::dial().await` (`modules/gateway/src/lib.rs:652-673`). Client transport sets `keep_alive_interval` but NO `max_idle_timeout` (`core/edge/src/client.rs:55-58`) — server.rs:158 and player.rs:239 both set it. The correct per-key singleflight pattern already exists in `modules/gateway/src/keys.rs:244-284`. |
| 2 | Scheduler tick unbounded, stop() hangs, lifecycle detaches | VERIFIED | `pool.begin()/acquire()` un-timed (`modules/scheduler/src/lib.rs:214,273`); each `fire()` gets a fresh 30s budget → tick worst case N×30s; `stop()` is a bare `JoinHandle.await` (`lib.rs:639-655`); `core/lifecycle/src/app.rs:250-264` drops the stop future after 5s leaving the spawned task detached (JoinHandle drop ≠ abort). asyncevents and invalidation both grace-then-`abort()`; scheduler is the sole outlier. `TICK_DEADLINE=30s` vs grace 5s makes detach routine. Advisory-lock poison hazard: `fire()` uses a POOLED conn — abort mid-fire returns the conn to the pool with the session advisory lock held (the documented reason timeout-wrapping fire is forbidden). |
| 3 | Slow-upload DoS on inbound HTTP | VERIFIED | `axum::body::to_bytes(body, MAX_BODY_BYTES)` bounds size only (`modules/gateway/src/lib.rs:772`); proxy `read_timeout` covers origin-response chunks only (`proxy.rs:31-35`); no timeout layer anywhere (`tower-http` present only transitively via axum-server). `core/app::run` has the rate-limit precedent block at `lib.rs:688-711` to mirror. A whole-request `TimeoutLayer` bounds BOTH the typed-op decode await and the proxy `forward()` (both inside the handler future); origin→client response streaming happens after the future resolves and stays covered by the existing per-chunk `read_timeout`. |
| 4 | JWKS no revocation | VERIFIED | Hit path returns cached kid with no freshness check (`modules/accounts/src/epic.rs:98-102`); the only `Instant` guards fetch-attempt cooldown, not cache age. Full-set swap on refresh already drops rotated keys — the missing piece is a TTL check on the HIT path. |
| 5 | Edge silent method overwrite; routecheck erases evidence | VERIFIED | `Server::handle/handle_identity` = bare `HashMap::insert` (`core/edge/src/server.rs:106,112`). Sibling hunt: registry `provide` panics, bus duplicate sub id panics, lifecycle duplicate module panics, gateway `RouteTable::build` bails (`modules/gateway/src/lib.rs:499-549`) — **edge is the ONE silent-overwrite seam in the codebase**. routecheck's BTreeSet (`tools/routecheck/src/main.rs:171`) is not the loss site; `Server::methods()` already returns deduped keys. |
| 6 | apikeys key-length contract split | VERIFIED | `KEY_MAX_BYTES=256` private to gateway (`modules/gateway/src/keys.rs:42,270`, introduced by af26dc5 as a hash-flood guard); apikeys schema/store/admin have NO length bound. `apikeysapi` is the natural home for a shared const. |
| 7 | audit check-then-ALTER | VERIFIED | Sole `ALTER TABLE` in all of `modules/` (`modules/audit/src/lib.rs:85-106`); folding `event_id` into `CREATE TABLE` removes the DO-block entirely, per the repo's wipe-over-migrations policy. |
| 8 | verify green without cargo-audit | VERIFIED (narrowed) | `ensure_tool` failure → SKIP, and `blocking=true, SKIP` never trips `fail` (`verify.sh:204-207,641-645`; verify.ps1:168-171 byte-parallel). **Generalization refuted:** all other blocking stages go through `simple_stage` which has no SKIP branch — cargo-audit is the only offender. |
| 9 | rpc-macro unvalidated HTTP mappings | VERIFIED | `path_args`/`body_names` consumed only via `.get(param)` (`tools/rpc-macro/src/lib.rs:322-330`) — bogus keys inert; `__path.get(w).unwrap_or_default()` (`lib.rs:862-867`) silently decodes ""; the macro never parses `{placeholder}`s out of the path template. All data for `syn::Error` checks is in scope in `build_method`; the crate has NO test harness (trybuild needed from scratch). Operation carries no path_args → NO golden re-bless needed. |
| 10 | retention supervision blind to ineffectiveness | VERIFIED | Sweep errors log-only (`core/asyncevents/src/retention.rs:122-124,145-147`); only task DEATH flips `retention_dead`. The pattern to mirror exists: `Liveness.last_ok_secs` / `mark_pass_ok` / `delivery_stalled` (`core/asyncevents/src/lib.rs:109-155`, wired at `core/app/src/lib.rs:513-517`). |
| m1 | Argon2 LazyLock on async worker | VERIFIED — in BOTH accounts and admin (`modules/accounts/src/lib.rs:337` + `modules/admin/src/lib.rs:504-508`); identical structural bug, not drift. |
| m2 | remote 2nd-attempt reset ignores definitive answer | VERIFIED — guard exists only on the `Err(first)` arm (`core/remote/src/lib.rs:209`); `Err(second)` resets unconditionally (`lib.rs:218-223`); introduced by 4ac75cd itself; the exact combination (transport fault → definitive answer) is untested. Only `RetryMode::OnceAfterReconnect` paths affected. |
| m3 | routecheck env-flag list hand-maintained | VERIFIED — `GATES` = literal 4-entry array (`tools/routecheck/src/main.rs:61-73`); the doc-header "every env config" claim overstates it. |
| m4 | C# gen DTO short-name collision | VERIFIED — flat `BTreeMap<String, StructFields>` across all api crates (`tools/csharp-client-gen/src/scrape.rs:249-270`); last-sorted file wins silently; the tool's own gates (`check_completeness`/`check_drift`) are fail-loud precedents. |

**Meta-survey conclusions (checker-authority):** the repo already has three
authority-point exemplars — gateway `RouteTable::build` collision bail,
invalidation first-refresh fail-loud, splitproof/checkmodules single-sourcing.
The highest-value moves replicate those onto un-guarded twins rather than
adding checker N+1. Adopted into steps below: edge duplicate rejection
(Step 1), wire-only `retry_safe` surfaced as a golden value (Step 9), argon2
param-parity test (Step 11). **Accepted gaps (explicit, with rationale):**

- *Delivery progress-vs-backlog probe*: NOT adopted. A live-but-faulted
  subscription is poison backoff — per-subscription by design (operator surface
  `eventctl`), and must NOT flip process readiness. Connection-error loops are
  already caught by the existing unhealthy-pass STALE clock. Nothing real slips
  through that a probe would catch without falsely flagging poison.
- *start-phase `require()` drift*: requirecheck observes init only; a
  registry-side caller-attribution rework is unjustified by the current tree.
  Documented as "requires must resolve in init".
- *sh/ps1 script twins*: no low-magic single-sourcing exists (owner rejects
  config layers); kept as hand-maintained byte-parallel with the existing style
  rule. Step 14 touches both sides of verify in one diff.
- *routecheck GATES derivation*: deriving gate env-vars from source is a
  research project; Step 15 instead makes the doc honest and adds a pointer in
  the add-game-module skill checklist.

**Why not extend/depend on X:** every fix below extends an existing seam or
copies an existing in-repo pattern (keys.rs flight locks, invalidation stop,
delivery STALE clock, gateway build bails, golden value rendering). No new
module, no new checker, no new config surface is introduced anywhere in this
plan — that is the point.

## Sequencing logic

Core seams first (Steps 1–5: edge, gateway, app, scheduler, remote) because
they are the templates new modules copy and later steps' tests ride on them;
then plane/observability (6), then macro/tooling authority moves (7–9), then
module-local fixes (10–13), then verify-net + tooling batch (14–15), then the
full verification gate (16). Each step compiles and passes tests on its own;
commit after each (Conventional Commits, per-step).

---

## Step 1 — `core/edge`: duplicate-method rejection + client dial deadline `[fable]`

**(a) What:** `core/edge/src/server.rs` (`handle`, `handle_identity`, ~104-113),
`core/edge/src/client.rs` (`dial_with_config`, ~44-66), `core/edge/src/tests.rs`.

**(b) Why now:** the ONE silent-overwrite seam (finding 5) and the missing dial
bound (finding 1b) both live in core/edge; Step 2's gateway rework calls the
new dial and Step 8's routecheck behavior changes as a free side effect (a
duplicate now panics inside the checker run too).

**(c) How:**
- `handle`/`handle_identity`: reject a duplicate loudly, matching the
  registry/lifecycle convention: check BOTH maps (a name in `handlers` and
  `id_handlers` is also a collision) and
  `panic!("edge: method {method:?} registered twice — two capabilities claim the same wire method")`.
  Registration happens at startup (`app::run` → `apply_edge_registrations`), so
  a panic is a loud boot failure, same class as `registry::provide`.
- `client.rs`: add `transport.max_idle_timeout(Some(IDLE_TIMEOUT.try_into()...))`
  mirroring `server.rs:158`, and wrap `endpoint.connect(addr, "localhost")?.await`
  in `tokio::time::timeout(DIAL_DEADLINE, ...)` with
  `const DIAL_DEADLINE: Duration = Duration::from_secs(5);` mapping elapse to
  `Error::Connect("dial timed out after 5s")`.
- Tests: duplicate `handle_identity` panics (`#[should_panic]`); dial to a
  bound-but-silent UDP socket errors within the deadline (assert elapsed < 10s).
- Doc: update the module doc in server.rs to state the uniqueness contract.

## Step 2 — gateway: per-provider dial, no lock across await `[fable]`

**(a) What:** `modules/gateway/src/lib.rs` (`RouteTable.remotes` field ~478,
`remote_caller` ~651-675, eviction path ~623-635), `modules/gateway/src/tests.rs`.

**(b) Why now:** depends on Step 1's bounded dial; this is finding 1a, the
highest-severity fault-isolation break.

**(c) How:** mirror the flight pattern from `modules/gateway/src/keys.rs:244-284`
(same file family, proven under test):
- Cache becomes `std::sync::Mutex<HashMap<String, Arc<dyn Caller>>>` — locked
  only for synchronous get/insert/remove, never across an await.
- Add `flights: std::sync::Mutex<HashMap<String, Weak<tokio::sync::Mutex<()>>>>`;
  `remote_caller` resolves/creates the per-provider flight mutex synchronously,
  drops the outer lock, `lock_owned().await`s only its provider's flight, then:
  re-check cache → dial (bounded, from Step 1) → insert → return. A dead
  characters-svc now blocks only characters-bound requests.
- Eviction (`dispatch`'s `Arc::ptr_eq` reset) moves to the std-mutex cache;
  semantics unchanged.
- Tests: (i) concurrent calls to two providers where one dial hangs — the
  healthy provider's call completes; (ii) duplicate-dial suppression per
  provider (second caller waits on the flight, reuses the first's client).

## Step 3 — `core/app`: whole-request inbound HTTP timeout `[opus]`

**(a) What:** `core/app/src/lib.rs` (Config + `run`, near the rate-limit block
~688-711), `core/app/Cargo.toml` (add `tower-http = { version = "0.6", features = ["timeout"] }`),
`cmd/gateway-svc/src/main.rs` (~106), `core/app/src/tests.rs`.

**(b) Why now:** finding 3; independent of Steps 1-2 but core-owned, and every
svc inherits it — the "new modules copy healthy defaults" fix.

**(c) How:**
- `Config`: `http_request_timeout: Option<Duration>` + builder
  `with_request_timeout_default(Duration)`; env override
  `HTTP_REQUEST_TIMEOUT_MS` read in `core/app` beside `HTTP_DRAIN_GRACE_MS`
  (same precedent: mechanism in core, knob via env, modules never read it).
  Default ON at 30s for every process (aligned with `PROXY_READ_TIMEOUT`), `0`
  disables.
- Apply `tower_http::timeout::TimeoutLayer` at the same point as the rate
  limiter (under the metrics layer, so timeouts are counted). This bounds the
  typed-op body read AND the proxy inbound-upload leg in one place; origin
  response streaming stays covered by the proxy's per-chunk `read_timeout`.
  The comment at the layer site must name the emitted status (TimeoutLayer →
  **408**) and why it is deliberate, so nobody later "fixes" it to 504
  silently.
- Test: a handler that sleeps past the (test-configured, short) timeout returns
  408/timeout response; a fast request is unaffected.

## Step 4 — scheduler: bounded acquire, aggregate tick budget, non-hanging stop `[fable]`

**(a) What:** `modules/scheduler/src/lib.rs` (`due_schedules` ~214,
`fire` ~271, tick loop ~232-244, `stop` ~639-655), `modules/scheduler/src/tests.rs`.

**(b) Why now:** finding 2. Fable lane because the advisory-lock/abort
interaction is the subtlest correctness spot in this plan.

**(c) How:**
- **Dedicated connection for `fire`:** replace `pool.acquire()` with a
  dedicated `PgConnection::connect_with(pool.connect_options())` — the connect
  options are derived FROM the existing ctx-provided pool (the module has no
  DSN and `lifecycle::Context` must not grow one; sqlx's
  `Pool::connect_options()` gives back the options the pool was built with).
  Dropping a dedicated connection CLOSES the session → the session advisory
  lock releases → aborting mid-fire can no longer poison a pooled session.
  This removes the documented reason timeout/abort was forbidden. (Connection
  churn only on due schedules — fine at 1s tick for this project; asyncevents'
  dedicated delivery backends are the precedent.)
- **Bound acquisition:** wrap `pool.begin()` in `due_schedules` and the
  dedicated connect in `fire` with `tokio::time::timeout(ACQUIRE_DEADLINE, …)`
  (5s const) — dropping a pending acquire/connect carries no session state.
- **Aggregate tick budget:** compute `let tick_deadline = Instant::now() + TICK_DEADLINE;`
  once per tick; pass the remaining budget into each `fire`'s
  `SET statement_timeout`. When the budget is exhausted, SKIP the remaining
  due schedules for this tick (log them, count the tick as errored) instead of
  flooring to 1ms — no point burning an acquire + advisory lock on a
  guaranteed statement-timeout error; the next tick re-reads due schedules
  anyway.
- **stop():** mirror `invalidation::stop` (`core/invalidation/src/lib.rs:335-345`):
  send stop signal, then per-task `tokio::time::timeout(STOP_GRACE, &mut t)`,
  `t.abort()` on elapse (safe now, per the dedicated-connection change);
  `const STOP_GRACE: Duration = Duration::from_secs(4);` — under the app-level
  5s so the module resolves before the lifecycle abandons it. Also check the
  stop signal between fires inside the tick loop so an in-progress tick yields
  at the next schedule boundary.
- Update the module doc-comment (the "FORBIDDEN" rationale changes with the
  dedicated connection). Tests: stop() returns within grace while a fire is
  stuck (mock via a pg_sleep schedule); advisory lock is released after abort —
  POLL `pg_try_advisory_lock` from a fresh conn with a short deadline (the
  server releases the session lock only after noticing the disconnect; an
  immediate assert is flaky).

## Step 5 — `core/remote`: definitive-answer guard on the retry arm `[sonnet]`

**(a) What:** `core/remote/src/lib.rs` (~216-224), `core/remote/src/tests.rs`.

**(b) Why now:** finding m2; one-line fix fully specified by research, no
dependencies.

**(c) How:** add `Err(second) if second.status.is_definitive_answer() => Err(second),`
above the reset arm, mirroring line 209. Test: fake dialer yielding
`Unavailable` (attempt 1) then `NotFound` (attempt 2) under
`RetryMode::OnceAfterReconnect`; assert `closes == 1` (only the first conn
reset) and the error surfaces as NotFound.

## Step 6 — asyncevents retention: staleness clock + error counter `[opus]`

**(a) What:** `core/asyncevents/src/lib.rs` (`Liveness` ~109-155),
`core/asyncevents/src/retention.rs` (~118-147), `core/app/src/lib.rs`
(readyz check ~530-540), `core/asyncevents/src/retention_tests.rs`.

**(b) Why now:** finding 10; copies the in-crate `delivery_stalled` pattern.

**(c) How:**
- `Liveness`: add `retention_ok_secs: Arc<AtomicU64>` + `mark_retention_ok()`
  + `retention_stalled(max_age) -> bool` — exact mirrors of
  `last_ok_secs`/`mark_pass_ok`/`delivery_stalled` (including the
  `stopping`/zero-seed semantics). Seed once when the retention task starts.
- `retention::run`: stamp `mark_retention_ok()` when `sweep` returns `Ok`.
- Metrics: `IntCounter asyncevents_retention_sweep_errors_total`, incremented
  in both error sites (top-level sweep + per-topic `gc_topic`), registered
  beside the existing wakeup-listener-deaths counter.
- readyz: extend the `asyncevents-retention` `ReadyCheck` to also report
  `retention_stalled(RETENTION_STALL_MAX)`; `RETENTION_STALL_MAX` = 3×
  housekeep interval (const beside `DELIVERY_STALL_MAX`).
- Test: a sweep that errors persistently (revoke perms on the retention SQL fn
  or inject a failing pool) flips readyz within the budget; a healthy sweep
  keeps it green.

## Step 7 — rpc-macro: compile-time HTTP-mapping validation + trybuild `[opus]`

**(a) What:** `tools/rpc-macro/src/lib.rs` (`build_method` ~299-346,
`parse_http` ~170-226), new `tools/rpc-macro/tests/compile_fail/*.rs` +
`.stderr`, `tools/rpc-macro/Cargo.toml` (dev-dep `trybuild`).

**(b) Why now:** finding 9 — the authority fix (illegal mapping fails to
compile) instead of another checker. Before Step 9 because both touch the
macro and 9 builds on a validated base.

**(c) How:** in `build_method`, after the arg loop:
1. Diff `b.path_args.keys()` ∪ `b.body_names.keys()` against real param names →
   leftover key = `syn::Error::new(attr_span, "path_args/body_names entry {k:?} names no parameter of {method}")`.
2. Scan `b.path` for `{name}` placeholders (hand-rolled, same style as
   `parse_http`): every placeholder must be some `path_args` VALUE, and every
   `path_args` value must appear as a placeholder — mismatch either way is a
   `syn::Error`.
   Errors propagate through the existing `syn::Result` → `to_compile_error()`
   path (`lib.rs:114`).
3. Add `trybuild` harness: one pass fixture + three compile-fail fixtures
   (bogus path_arg key, bogus body_name key, placeholder/arg mismatch).
4. Workspace check: all existing contracts must still compile (they encode the
   correct mappings; any that fail are real latent bugs — fix them, don't
   loosen the check).

## Step 8 — routecheck: honest claims + duplicate-count note `[sonnet]`

**(a) What:** `tools/routecheck/src/main.rs` (doc header ~1-73).

**(b) Why now:** after Step 1, edge duplicates are impossible (they panic at
build inside routecheck's own profile run), so routecheck needs no BTreeSet
rework — only its documentation must stop overstating.

**(c) How:** rewrite the header: SERVE-PARITY is set-membership over a
uniqueness guarantee now enforced by `edge::Server` itself (cite Step 1),
and state explicitly that a duplicate edge method now PANICS mid-run inside
routecheck's profile build (a loud backtrace from a "static checker" is the
intended behavior, not a checker bug); `GATES` is an explicit, hand-curated
allowlist — remove "every env config", add "adding a route-gating env var
REQUIRES a GATES entry" with a pointer from the add-game-module skill
(Step 15 wires that pointer).

## Step 9 — contract-golden: wire-only retry semantics as a value `[opus]`

**(a) What:** `tools/rpc-macro/src/lib.rs` (glue emission — extend the
generated `route_bindings()`/`operations()` family with a wire-method
descriptor incl. `RetryMode`), `tools/topiccheck/src/golden.rs` (render the
new value), `docs/reference/contract-golden/contracts.txt` (re-bless),
public-api baselines for touched contract crates (re-bless, additive).

**(b) Why now:** meta-survey candidate #2 — closes the golden's self-declared
blind spot ("wire-only `#[retry_safe]` surfaces no data value"); rides on the
macro work from Step 7.

**(c) How:** emit a `pub fn wire_ops() -> Vec<opsapi::WireOp>` (new small
struct in `core/opsapi`: `{ method: &'static str, retry_mode: RetryMode }`)
from `generate_glue` for EVERY trait method (http and wire-only); golden
renders one `wire module=… method=… retry=…` line per entry, sorted, same
diff/bless flow (topiccheck's golden already imports every `<name>rpc` crate
to render `operations()` values — `wire_ops()` rides the same imports).
Re-bless golden + public-api intentionally in the same commit, with the diff
shown in the commit body. Bless scope: the public-api baseline list is
derived from the filesystem over `api/*` contract crates — confirm at
execution whether `core/opsapi` is in that set; if not (expected: it is
core/, not api/), only the `api/*/rpc` crates whose re-export surface grew
need a re-bless.

## Step 10 — accounts: JWKS TTL on the hit path `[opus]`

**(a) What:** `modules/accounts/src/epic.rs` (~84-149), `modules/accounts/src/epic_tests.rs`.

**(b) Why now:** finding 4; module-local, independent.

**(c) How:** cache becomes `RwLock<Option<(JwkSet, Instant)>>`; hit path
accepts a cached kid only if `fetched_at.elapsed() < JWKS_CACHE_TTL`
(`const … = Duration::from_secs(600)`); stale-or-miss falls through to the
EXISTING singleflight + `MIN_REFRESH_INTERVAL` cooldown (unchanged — it
bounds attacker-forced fetch rate); refresh remains a full-set swap (which is
what actually drops rotated kids). Tests: stale hit triggers refetch; a kid
absent from the new set is rejected post-refresh; cooldown still bounds fetch
frequency.

## Step 11 — accounts + admin: dummy-hash prewarm + argon2 param parity `[sonnet]`

**(a) What:** `modules/accounts/src/lib.rs` (`start`), `modules/accounts/src/password.rs`,
`modules/admin/src/lib.rs` (`start`, ~640), plus a parity test in
`cmd/server/src/tests.rs` (the one crate that already imports both modules).

**(b) Why now:** finding m1 (both twins) + meta-survey candidate #4 in one
mechanical sweep.

**(c) How:** in each module's `start()`:
`tokio::task::spawn_blocking(|| { LazyLock::force(&DUMMY_HASH); }).await` —
first I/O/CPU belongs in start, so the first unknown-user login never pays
Argon2 init on an async worker. Parity test: assert accounts' and admin's
argon2 params (m/t/p/output len — expose each module's params via an existing
or tiny `pub(crate)`→`pub` const accessor consumed only by the test) are
equal, so the security-cost twins can't drift silently.

## Step 12 — apikeys ⇄ gateway: one key-length contract `[sonnet]`

**(a) What:** `api/apikeys/apikeysapi/src/lib.rs` (add
`pub const MAX_KEY_BYTES: usize = 256;`), `modules/gateway/src/keys.rs`
(use it), `modules/apikeys/src/store.rs` (`insert_tx` reject over-length),
`modules/apikeys/src/admin.rs` (validation-phase reject with a form error),
`modules/apikeys/src/lib.rs` (DDL: add `CHECK (octet_length(key) <= 256)` —
**octet_length, not length**: the shared const is BYTES and the gateway checks
`key.len()` bytes; `length()` counts characters, so a multibyte key ≤256 chars
but >256 bytes would re-open the exact contract split this step closes —
defense-in-depth; fresh-boot only per wipe policy), public-api baseline
re-bless (additive), tests in apikeys + gateway keys tests.

**(b) Why now:** finding 6; contract crate is the designed meeting point of
the two modules.

**(c) How:** as listed — `insert_tx` and the admin validation both reject on
**byte** length (`key.len() > apikeysapi::MAX_KEY_BYTES`); the store/admin
error message names the limit and the constant's home so the next reader
finds the single source.

## Step 13 — audit: fold `event_id` into CREATE TABLE `[sonnet]`

**(a) What:** `modules/audit/src/lib.rs` (~85-106).

**(b) Why now:** finding 7; deletes the sole ALTER in `modules/`, restoring
the "idempotent CREATE …, nothing more" policy uniformly.

**(c) How:** `event_id text` becomes a column of the `CREATE TABLE IF NOT EXISTS`;
delete the `DO $$ … ALTER … $$` block and rewrite the doc-comment to state the
policy instead of the workaround. Rollout = wipe (`DROP SCHEMA audit CASCADE`
or full DB wipe) — note it in the commit body; no bridge.

## Step 14 — verify.sh/.ps1: blocking stages must not silently SKIP `[sonnet]`

**(a) What:** `verify.sh` (~194-222, summary ~635-646), `verify.ps1`
(~164-190 + its summary) — one diff, both twins.

**(b) Why now:** finding 8 (narrowed to cargo-audit by research).

**(c) How:**
- Install-enabled (`--no-install` NOT passed) + `cargo install` fails → **FAIL**
  (environment defect, not offline).
- `--no-install` + tool absent → SKIP stays (operator opted out), and the
  offline advisory-DB regex branch stays SKIP.
- Summary: when any `blocking=true` stage is SKIP, the final line becomes
  `VERIFY: OK (blocking stage(s) SKIPPED: cargo-audit)` — grep-visibly
  different from plain `VERIFY: OK`.
- Comment beside `add_result`/`simple_stage` codifying the invariant:
  "a blocking stage may SKIP only on an explicit operator opt-out, and any
  blocking SKIP must be named on the final line."

## Step 15 — tooling batch: C# gen collision bail + skill pointer `[sonnet]`

**(a) What:** `tools/csharp-client-gen/src/scrape.rs` (~267-270) + its tests;
`.claude/skills/add-game-module/SKILL.md` — add the routecheck-GATES
checklist line from Step 8.

**(b) Why now:** last code fixes; both are fully specified one-liners in
fail-loud style the tool already uses.

**(c) How:** before `structs.insert(name, fields)`, `bail!` on
`contains_key` naming BOTH source files (track name→file provenance in a side
map). Test: two fixture crates with a same-named struct → error names both
paths. Skill: add "if your module reads a route-gating env var in
register/init, add it to routecheck's GATES" to the module checklist.

## Step 16 — full gate + docs + memory `[inline]`

**(a) What:** CLAUDE.md (edge uniqueness contract sentence in seam 2's
description; scheduler stop semantics if the FORBIDDEN wording is quoted
anywhere), `./verify.sh --all` (single rollout — check for running
cargo/rustc first, per the one-test-run protocol), trailer audit
(`git log --format="%h %B" | grep Co-Authored` vs lanes), memory-sync push if
memories changed.

**(b) Why now:** the gate closes the round; docs and the safety net must
reflect the new invariants before the next module is added.

**(c) How:** run verify (blocking + advisory; expect golden/public-api
re-blessed in Steps 9/12 to PASS), then splitproof is already inside verify's
blocking tier; fix anything red; write
`docs/plans/2026-07-11-2249-remediation-round4-plan.md` status flip to
"executed" + a short summary doc per the docs convention.

---

## Baseline re-blesses (intentional, each in its own step's commit)
- Step 9: `docs/reference/contract-golden/contracts.txt` + public-api baselines
  (additive `wire_ops`/`WireOp`).
- Step 12: public-api baseline for `apikeysapi` (additive const).

## Verification map (which stage proves which step)
- Steps 1,2,4,5,6,10,11,12,13: `cargo test --workspace` (+ targeted `-p` runs
  during development, one at a time).
- Step 3: core/app test + splitproof (RL scenarios unaffected; timeout is 30s).
- Step 7: trybuild fixtures + workspace build.
- Steps 8,15: `cargo run -p routecheck` / csharp-client-gen tests.
- Step 9: contract-golden + public-api stages.
- Step 14: three cases. (i) `./verify.sh --fast` normally → plain `VERIFY: OK`;
  (ii) `PATH` without cargo-audit + `--no-install` → named-SKIP banner;
  (iii) the NEW FAIL branch (install-enabled + failed install): simulate by
  removing cargo-audit from `PATH` and pointing `CARGO_HOME` at a plain FILE
  (not a directory) so `cargo install` fails deterministically → expect
  `VERIFY: FAIL` with cargo-audit FAIL. Run these verify invocations one at a
  time (shared-Postgres protocol), or with the heavy stages already green from
  case (i) reuse `--fast` and accept the rebuild cost.
