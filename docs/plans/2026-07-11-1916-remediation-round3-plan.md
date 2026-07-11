# Remediation Round 3 — plan

**Date:** 2026-07-11 19:16
**Author:** Fable 5 (session model), synthesized from 9 parallel research subagents + 4 review subagents.
**Status:** draft for approval.

## Context

Two independent review passes (my 4-reviewer sweep + a second external list) converged on
one dominant pattern in the 2026-07-11 remediation wave: **fixes applied to one side of a
twin, the symmetric side left untouched** — never a "fix-A-breaks-B" regression, always a
*fix not carried to the more-hostile path*. The three worst instances:

- dev-grant gated in the **monolith** contribution path but **not** the split (gateway-svc
  route bindings + inventory-svc edge both unconditional) → a logged-in player can mint
  inventory with `INVENTORY_DEV_GRANT` off in the split. **split-proof can't catch it because
  it always sets the flag to 1.**
- 30s stream bounds added to the **internal** mTLS edge (752fdfb) but **not** the public
  player-QUIC plane → attacker-chosen keepalive pins stream tasks indefinitely.
- session-auth hardening (spawn_blocking permit, decoy hash, admission caps) landed in
  **admin** but the twin **accounts** dev-auth path still runs 64 MiB Argon2 synchronously on
  the async worker, with a login timing oracle.

Every finding below was **verified in the current source** (not just diffs) by a dedicated
research subagent; each carries the exact fix seam and a test strategy. The unifying test
deliverable is a new static checker (`tools/routecheck`) that makes the monolith/split
front-door route sets **structurally equal**, so this class of bug fails `verify` in every
env configuration going forward — the general net the user asked for.

### Why not extend an existing checker instead of a new `routecheck`?

- `archcheck` = dependency-law (crate edges), no runtime contribution surface. Wrong layer.
- `topiccheck` = subscription graph; it *does* own the `observe`/recording-transport harness
  we reuse, but its domain is events, not ops/routes. Folding route-parity in would blur two
  concerns. `routecheck` is a sibling that **reuses** `topiccheck`'s harness shape and
  `checkmodules::DeploymentProfile` as its data source (so any future `cmd/*-svc` is covered
  with no hand list — satisfies the "didn't-forget self-check" rule).
- The `describe()` runtime route manifest (mini-orchestrator scope) does not exist yet;
  nothing to lean on there.

### Harness note — split-proof is now a Rust crate (revised after HEAD moved)

Since this plan's research was gathered (base `0595e90`), HEAD advanced 8 commits
(`358ab57..f7d3e71`): **`split-proof.sh`, `split-proof.ps1`, and `tools/winctrl` were deleted**
and replaced by a single cross-platform Rust harness, `tools/splitproof/src/main.rs`
(`cargo run -p splitproof`; `verify.{sh,ps1}` invoke it as the `split-proof` stage). Consequences
for this plan, all verified against HEAD:

- **Every code file this plan patches is UNCHANGED since `0595e90`** (`git diff --name-only`
  confirms) — Steps 1-5, the code minors in 6, 6b, and routecheck in 7 are all still accurate.
- **The old round-1 teardown-leak class is GONE at the source.** The harness tears the fleet
  down via a kill-on-drop guard (`Running::drop` → `child.kill()`), so a panic or early `?` can
  never orphan a `-svc` (the exact winctrl failure mode). The former Step 6 `stop_pid` /
  `Start-Svc` / `winctrl` fixes are **obsolete and removed** — the files no longer exist.
- **The fleet-drift "didn't-forget" self-check already exists** in the Rust harness
  (`preflight_fleet`, main.rs:539 — `cmd/*-svc` on disk must equal the harness fleet), so no
  new fleet self-check is needed; only the topiccheck define-site + orphan-baseline self-checks
  (Step 6b) remain novel.
- **`[I-GATE]` is now a Rust assertion** inside `tools/splitproof` (Step 7), not an sh/ps1 pair.

### Dispatch tags (approve with the plan)

`[fable]` = Fable 5 subagent (correctness/security/seam-critical). `[sonnet]` = Sonnet 4.6
subagent (mechanical, fully-specified). Every code-writing Agent call passes an explicit
`model:` and the matching `Co-Authored-By` trailer. Commit after each step
(Conventional Commits, on master). One test rollout at a time — DB-bound tests serialize; the
DB-free tests (edge loopback, parser, checkers, remote fakes) are safe anytime.

---

## Step 1 — Inventory dev-grant split bypass + complete the accounts gate pattern  `[fable]`

**(a) What.** `modules/inventory/src/lib.rs` (`struct Inner`, `register`, `impl Holdings::grant`,
`init` lines 771-787); `modules/accounts/src/ops.rs` (`register_player_ops` filter);
`modules/accounts/src/lib.rs` (already has the guard — no change beyond confirming).

**(b) Why first.** It's the HIGH-severity security bug, and it must land *before* Step 7's
`routecheck` — the checker enforces exactly the invariant this step establishes, so the code
must satisfy it first or the new blocking stage fails.

**(c) How.** Adopt the **accounts precedent completed to structural parity** (the accounts
research nailed this as option D):
1. `Inner` gains `dev_grant: bool`; set it in `register()` from `env_bool("INVENTORY_DEV_GRANT",
   false)` (helper at line 115; env-in-own-register is sanctioned, same as accounts `dev_auth`
   at lib.rs:438).
2. `Holdings::grant` (line 454) first statement:
   `if !self.dev_grant { return Err(Error::not_found("grant is not enabled")); }` — the single
   authority both topologies traverse (gateway HTTP route, player-QUIC allow-list, raw mTLS
   edge all now fail closed with NotFound→404).
3. **Delete** the `if !dev_grant && op.operation.method == METHOD_GRANT { continue; }` filter at
   lines 778-787 so contribution is **unconditional** — this is what makes the monolith SLOT
   set == split `route_bindings()` set *by construction*. Keep the warn log.
4. **Accounts symmetry (three gated ops, not two).** `ops.rs:21-29` gates **register + login on
   `dev_auth`** AND **`loginEpic` on `epic_enabled`** (client-id presence). To make routecheck a
   clean equality check, delete ALL of that filtering so all three ops are contributed
   unconditionally, gated only at the impl:
   - register/login → existing NotFound guards (lib.rs:219,259).
   - `loginEpic` guard (lib.rs:283-285) returns `Error::unavailable` (503), not NotFound —
     accept that: "epic not configured" reads as 503 (feature unavailable), which is the honest
     status. This is a **404→503 shift for a disabled-epic caller** (already an error case).
   - Rewrite the `ops.rs` module doc (lines 5-8) that describes conditional registration.
5. **Behavior delta is REAL — see M1; resolved by Decision A (accepted).** With ops
   unconditionally contributed, the gateway route table now *matches* a gated method, so the
   API-key / bearer checks run **before** the impl guard. For a gated route with the gate off:
   - no/bad API key → **401** (was 404); client key lacking policy → **403** (was 404); missing
     bearer on a `Player`-auth op → **401** (was 404). Only a fully-keyed, fully-authed caller
     reaches the impl-guard **404**.
   - This **leaks method existence** to unauthenticated callers (gateway lib.rs:363-365 today
     deliberately 404s an unknown method *before* the key check precisely to avoid that oracle)
     and adds per-route metric series for gated routes.
   - The trade is deliberate: a method-existence oracle (dev-gated routes only) in exchange for
     making the monolith/split divergence **structurally testable** by routecheck. **DECIDED:
     Option A (accepted)** — see Decisions. Unconditionalize all four ops, gate at the impl.
6. `cmd/*` composition roots: **zero changes.** `register_server`/`remote_factories` untouched.

**Tests (this step):**
- `modules/inventory/src/tests.rs` — mirror `modules/accounts/src/tests/dev_auth_gate.rs`:
  `Inner { dev_grant: false, .. }.grant(...)` → `Status::NotFound` **before any DB touch**;
  `dev_grant: true` + bad qty → `Invalid` (gate open reaches validation). DB-free.
- `modules/accounts` — confirm existing `dev_auth_gate.rs` still green after the ops.rs change;
  add an assertion that the op IS contributed (unconditional) while the guard rejects. Step 3
  rewrites `login`/`register` and MUST preserve "guard → NotFound before identity fetch" so
  these assertions hold — that is why **1 → 3 is a hard sequential edge** (see M3 / sequencing).

**(d) Tag:** `[fable]` — cross-module seam + a behavior-shaped accounts change. **Hard edge: run
Step 3 AFTER this (same module `accounts`, overlapping test files); never dispatch 1 and 3
concurrently.**

---

## Step 2 — Player-QUIC unbounded stream waits (mirror 752fdfb onto the public plane)  `[fable]`

**(a) What.** `core/edge/src/player.rs` (`serve_stream` 495-524, `respond` 547-553, `PlayerServer`
Default 136-145, `listen`→`serve_conn`→`serve_stream` threading); new test in
`core/edge/src/player_tests.rs`.

**(b) Why here.** Independent of Step 1; grouped early because it's the other HIGH/MED-security
twin and shares the "mirror the internal fix" shape.

**(c) How.** Exact mirror of `server.rs`'s 752fdfb change (the edge research extracted it):
1. New plane-local const `PLAYER_STREAM_GRACE: Duration = Duration::from_secs(30)` (twin of
   `EDGE_STREAM_GRACE`; keep per-plane consts separate — don't couple the untrusted plane's
   knob to the trusted one's, matching the `MAX_PLAYER_BIDI_STREAMS`/`MAX_EDGE_BIDI_STREAMS`
   convention).
2. `PlayerServer` gains `stream_grace: Duration` (default in the manual `Default`) +
   `#[cfg(test)] pub(crate) fn set_stream_grace(&mut self, g: Duration)`; thread it as a param
   through `listen`→`serve_conn`→`serve_stream`.
3. In `serve_stream`: wrap the request read (line 508) in `tokio::time::timeout(grace,
   read_frame_max(&mut recv, MAX_PLAYER_FRAME))` — elapsed ⇒ `tracing::debug!` + return (drops
   both halves = reset, frees the InFlightGuard + RequestConnGuard clone + stream slot).
4. Bound the output half by giving `respond(send, resp, grace)` a grace param and wrapping its
   whole body (`write_frame` + `finish` + `stopped().await`) in ONE `timeout` — covers all three
   call sites (505 rate-denied, 515 FrameTooLarge, 523 main response) in one move.
5. **Dispatch stays unbounded** — same deliberate intent as the internal plane (a domain call
   may legitimately be long; the remote leg is bounded by its own edge grace). Do NOT bound it.

**Tests (this step):** mirror `server_tests.rs::hung_request_read_is_reaped_after_grace` and
`undrained_response_is_reaped_after_grace` into `player_tests.rs` — the codebase **never uses
paused time** (real quinn timers don't advance under it), so use the **grace-shrinking seam**
(`set_stream_grace(200ms)`), a raw quinn client with `keep_alive_interval(500ms)` +
`stream_receive_window(1024)` that withholds a frame / never drains, and poll `in_flight_count()`
2→1 via the returned `RunningServer.shutdown` (the same reach server_tests uses — `player.rs`
shares `ShutdownState`/`RunningServer`). The undrained-response test doubles as the
keepalive-defeats-idle-reaper proof. Add an explicit assertion for the **rate-denied early-return
path** (respond call site 505) too, since it's one of the three now-bounded `stopped().await`s.
DB-free, no serialization. Optionally extend the `edge_timing_invariants` const-pin test with
`PLAYER_STREAM_GRACE == 30s`.

**(d) Tag:** `[fable]` — transport correctness + a real DoS surface.

---

## Step 3 — Accounts dev-auth + JWKS hardening (carry the admin pattern to its twin)  `[fable]`

**(a) What.** `modules/accounts/src/lib.rs` (`register` 213-254, `login` 256-273, `login_epic`
286-291, `Service` fields + `register()` construction at 432), `modules/accounts/src/password.rs`
(decoy hash + `PasswordVerifier` seam), `modules/accounts/src/epic.rs` (`key_for` 61-78, error
taxonomy); new tests in `src/tests.rs` + `src/epic_tests.rs`.

**(b) Why here.** Depends on nothing above; it's the third security twin. Larger than Steps 1-2,
so it gets its own step.

**(c) How.** Duplicate admin's in-module pattern (no shared helper — fortress rule; admin
deliberately duplicated `password.rs`):
1. Add to `Service`: `argon_permits: Arc<Semaphore>` = `Semaphore::new(2)`, `login_slots:
   Arc<Semaphore>` = `Semaphore::new(32)`, input caps **email ≤ 320B (RFC 5321 local+domain max),
   password ≤ 1024B** (admin's 128 is a username cap — email needs more headroom),
   `verifier: Arc<dyn PasswordVerifier>`
   (the injectable seam admin has; add the trait to `password.rs`). Construct in
   `Module::register` (pure, no I/O).
2. **Timing oracle fix (login):** restructure to fetch identity → pick `(hash, candidate)` =
   real-or-decoy → **one** unconditional `spawn_blocking` verify → decide
   `verified && known_user && valid_input`. Decoy = `static DUMMY_HASH: LazyLock<String>` (mint
   once via `hash_password("accounts-timing-equalizer")`), verified against a fixed decoy
   candidate (never the user's real password against a decoy). Copy admin lib.rs:504-520.
3. **spawn_blocking-owned permit (5844831 shape):** the `OwnedSemaphorePermit` moves *into* the
   blocking closure (`let _permit = argon;`) so a cancelled request can't free it while the
   detached 64 MiB hash runs. `login_slots.try_acquire_owned()` (cap 32, non-blocking → reject)
   stays in the async frame (releases on cancel by design).
4. **`register` gets the same treatment** — it also hashes on the request path (admin's
   precedent only covered verify because adminctl hashes offline).
5. **No per-IP limit in the service** — `Auth::login` is a pre-auth opsapi method; gateway
   injects only `Identity` post-auth, so no client IP reaches it. Per-IP throttling, if wanted,
   lives at the gateway. State this explicitly; don't fake an IpLimiter here.
6. **JWKS singleflight + cooldown (`epic.rs`):** add `refresh: tokio::sync::Mutex<Option<Instant>>`;
   on cache miss take the mutex, re-check cache under it, honor a `MIN_REFRESH_INTERVAL` (~30s)
   cooldown (a flood of bogus kids costs ≤ 1 fetch/interval), else fetch + stamp.
7. **Error taxonomy (503 vs 401):** split `verify`/`key_for` into `Rejected` (bad
   sig/alg/aud/iss/exp/kid-after-fresh-fetch) vs `Infra` (network/HTTP-status/fetch failure);
   `login_epic` maps `Infra → Error::unavailable` (503), `Rejected → unauthorized` (401) —
   mirrors the `verify_session` 503-not-401 precedent in the same file. "Unknown kid during
   cooldown" → Rejected if there was ≥1 successful fetch, Infra if never.

**Tests (this step):** copy admin's fixtures — recording `PasswordVerifier` fake → assert
decoy path taken for unknown email (called once, with the decoy hash; **status-identity, not
timing** — the accounts tests already assert both bad-password and unknown-email return
`Unauthorized`); `GatedVerifier` (mpsc-frozen) → permit survives `login.abort()`
(`available_permits()` stays 0 until release) + 3rd concurrent login queues / 33rd rejected;
JWKS stub on `127.0.0.1:0` counting hits → exactly 1 fetch/cooldown for N concurrent unknown
kids, and a 500 stub yields Infra→503 not 401. DB-free unit tier + the existing `wired()`
DB-tier (clean-skips without Postgres).

**(d) Tag:** `[fable]` — security-critical concurrency + error-taxonomy design.

---

## Step 4a — Event-plane liveness (interval(0), unsupervised tasks, STALE stamp, lock-wait)  `[fable]`

**(a) What.** `core/asyncevents/src/retention.rs` (`interval_from_env` 54-59),
`core/asyncevents/src/lib.rs` (`Plane::start` 238-274, `Liveness` 99-190),
`core/asyncevents/src/worker.rs` (per-pass `mark_pass_ok` ~475; the `pg_terminate_backend`
lock-wait ~318-335), `core/app/src/lib.rs` (readyz check wiring ~504-524); tests in
`core/asyncevents/src/{tests,worker_tests}.rs`.

**(b) Why split from 4b (M5).** Step 4 originally bundled 5 sub-fixes across two planes
(asyncevents + scheduler) and two readyz semantics — too big for one dispatch. 4a is
asyncevents-only. **Note the `core/app/src/lib.rs` overlap with Step 6.2 (mailto helper): do
NOT dispatch 4a and 6 concurrently.**

**(c) How.**
1. **interval(0) → fail-startup** (house style: audit's 7ef7a18 bailed rather than clamped).
   Change `interval_from_env` → `anyhow::Result<Duration>` (twin of `worker::handler_timeout_from_env`),
   reject `Duration::ZERO` naming the env var + value + default; call it with `?` in
   `Plane::start` alongside `handler_timeout_from_env()?`, **before** any durable mutation, not
   inside the spawned task. **Decision (m4c): unparseable stays fallback-to-default** — a garbage
   string is a typo that shouldn't brick startup the way an explicit `0` (a deliberate
   "disable"-looking value that actually panics) should; only reject values that *parse to zero*.
   State this in the bail message's sibling comment.
2. **Supervise the three unguarded tasks** (`retention::run`, `wakeup::listen`,
   `plane_metrics::refresh_loop` — spawned at 256/265/270 without the worker's
   catch_unwind+`dead` wrapper). Per-task disposition (don't blanket-flip `dead`):
   - `retention` death → a **separate named readyz check** (`asyncevents-retention`) fed by its
     own `Arc<AtomicBool>` on `Liveness` (a GC outage is storage growth, not a serving outage —
     taking the process out of rotation would be wrong). catch_unwind wrapper like the workers.
   - `wakeup::listen` death → loud log + a Prometheus counter, **not** `dead` (workers still
     poll; it's a latency degrade).
   - `plane_metrics` death → log-only (observability, never readyz).
3. **STALE per-pass stamp (round-1 MED):** `mark_pass_ok` stamps once per *full pass* (up to
   64×subs×handler_timeout) while `DELIVERY_STALL_MAX` is a fixed 30s → two slow-but-*healthy*
   handlers can 503 an actively-delivering process. Stamp after each `Step::Delivered` (or
   per-sub) so the clock reflects progress, not pass boundaries.
4. **Worker lock-wait bound (round-1 MED):** in the timeout arm, if the control connection for
   `pg_terminate_backend` fails to connect the error is swallowed and the subsequent
   `record_failure` UPDATE can block unbounded on the row lock held by the un-terminated backend.
   Add a `lock_timeout`/`statement_timeout` on the `fresh`/`record_failure` path (same class
   41b1c0f fixed for migrate) so it fails loud instead of hanging.
**Tests (this step, 4a):**
- DB-free: parser test that `EVENTS_HOUSEKEEP_INTERVAL=0` is rejected while `"nonsense"` still
  falls back (model: `worker_tests::handler_timeout_parser_is_strict_and_checked`, a plain
  `#[test]`); the new `retention_dead` flag transition asserted by direct struct manipulation
  (model: `tests::liveness_delivery_stalled_...`).
- DB-bound (serialize), **LAND NOW per Decision 2:** a panicking retention task flips
  `asyncevents-retention` readyz. Add a `#[cfg(test)]` panic-injection seam — e.g. a
  `RETENTION_PANIC_ONCE` test hook (an `AtomicBool`/`OnceCell` the retention loop checks and
  `panic!`s on, gated behind `#[cfg(test)]` so it never ships) — then drive the real
  `Plane::start`/`stop` harness (`test_pool()` clean-skip), trip the panic, and assert the
  `asyncevents-retention` readyz flag flips while the delivery `dead` flag stays green (proving
  per-task isolation). Serialized with the other plane tests.

**(d) Tag:** `[fable]` — lifecycle ordering + readyz semantics.

---

## Step 4b — Scheduler liveness + hang bound  `[fable]`

**(a) What.** `modules/scheduler/src/lib.rs` (`run_loop` 457-471, `start`/`stop`, `Scheduler`
fields); a `"scheduler"` readyz contribution; tests in `modules/scheduler/src/tests.rs`.

**(b) Why separate.** Different plane, different readyz owner than 4a, and it carries the B2
redesign. Independent of every other step.

**(c) How.**
1. **DB-layer bound, NOT a future-dropping timeout (B2).** A `tokio::time::timeout` around
   `tick()` would drop the tick future mid-`fire`, and `fire` holds a **session-scoped
   `pg_try_advisory_lock` with explicit commit-before-unlock** (lib.rs:451-456, Go NOTE #10) —
   dropping it leaks the advisory lock on a pooled connection so that schedule never fires again
   on any process. Instead bound at the DB layer, same class as 41b1c0f: set a
   `statement_timeout` (and/or `lock_timeout`) on the tick's connection so a wedged query errors
   through the existing `if let Err(e) = tick(...)` arm **without dropping the future**. If the
   impl finds `tick` doesn't own a single dedicated connection (it acquires from the pool
   per-statement), fall back to `pg_advisory_xact_lock` inside the fire tx so rollback releases
   the lock — but the `statement_timeout` route is preferred (no lock-semantics migration).
   The subagent verifies which connection model `tick` uses **before** choosing; both branches
   are specified here, so this is a decision-with-known-options, not a research gap.
2. Scheduler-owned liveness flag (catch_unwind on the loop body, mirroring the asyncevents
   worker wrapper) + a `"scheduler"` `httpmw::ReadyCheck` contributed in `init`/wired in `start`
   (scheduler already contributes `adminapi::SLOT`/`edge::EDGE_SLOT`; the flag lives on
   `Scheduler`). A "no healthy tick in N×TICK_INTERVAL" stamp (analogous to
   `DELIVERY_STALL_MAX`) makes a hang visible; `TICK_DEADLINE` = 30s (2× the plane handler
   budget's order of magnitude, comfortably above a healthy sub-second tick).
3. `stop()`'s `let _ = t.await` discards a panic — log the `JoinError`.

**Tests (this step, 4b):** DB-free liveness-flag transition (direct struct manipulation); plus,
**LAND NOW per Decision 2**, a DB-bound hang test: from the test, hold a competing lock / force a
slow statement on the name the tick will touch (or point the tick's connection at a
deliberately-blocked row), assert the tick returns an **error via the `statement_timeout`**
(loop keeps running, future not dropped, advisory lock not leaked) and the `"scheduler"` readyz
flag flips — model on `modules/scheduler/src/tests.rs`'s existing live-DB `fire`/`due_schedules`
drivers. Serialized. If `tick` needs a `#[cfg(test)]` seam to inject the block deterministically,
add one (test-only, never ships).

**(d) Tag:** `[fable]` — lifecycle + a subtle lock-safety redesign (B2).

---

## Step 5 — Remote retry seam: classify domain answers vs transport failures  `[fable]`

**(a) What.** `core/remote/src/lib.rs` (`Reconnecting::call` 183-208), `core/opsapi/src/lib.rs`
(add `Status::is_transport`), `modules/gateway/src/lib.rs` (`RouteTable::dispatch` evict arm
606-631); tests in `core/remote/src/tests.rs`.

**(b) Why here.** Touches the retry seam efaba4c created; independent of Steps 1-4.

**(c) How. Predicate must be "definitive peer answer", NOT "== Unavailable" (M4).** Today the
only non-Unavailable error the Conn seam yields is NotFound (`core/edge/src/lib.rs:96-103`:
`UnknownMethod → NotFound`, everything else → Unavailable), so `reset iff Unavailable` works
*now* — but it fails **closed on the wrong side**: a future mapping that yields `Internal` for a
transport fault would leave the dead connection cached forever (the exact "brick the route" bug
the eviction prevents). Gate on the *answer*, not the *fault*:
1. Add `impl Status { fn is_definitive_answer(&self) -> bool { matches!(self, Status::NotFound) } }`
   (a peer that returned this demonstrably answered; the connection is healthy). Comment it as
   tied to the `From<edge::Error>` mapping so a future non-Unavailable transport status is caught.
2. `Reconnecting::call`: if `first.status.is_definitive_answer()` return `Err(first)` immediately
   **without** `reset()`, regardless of `retry_mode`. **Everything else** (Unavailable, Internal,
   anything new) proceeds to reset + conditional redial — reset stays the default; only a proven
   answer skips it.
3. On the c2 path, if `c2.call` also errors, `self.reset(&c2)` before returning — a dead c2 must
   not stay cached (restores the "a failure always invalidates" docstring truth for the
   transport case). Update the docstring to say "a *transport* failure".
4. Gateway twin (dispatch evict arm): gate *skipping* `cache.remove(provider)` on
   `err.status.is_definitive_answer()` — evict on everything else. A definitive NotFound must not
   evict a healthy cached connection; an unknown status still evicts (safe default).

**Tests (this step):** extend `FakeConn` with a `status` field / `not_found()` ctor (today it
only ever yields Unavailable — which is *why* this went unnoticed). New:
`notfound_does_not_reset_or_redial` (dials==1, closes==0, original NotFound returned, for both
retry modes); `internal_status_still_resets` (a non-Unavailable, non-NotFound status like
`Internal` MUST still reset — pins the "reset is the default" direction against M4);
update `gives_up_after_one_retry`'s assertion `closes==1`→`2` (proves c2 reset); keep
`redials_once_and_succeeds` as the Unavailable regression guard. DB-free.

**(d) Tag:** `[fable]` — seam correctness, subtle classification.

---

## Step 6 — Minor correctness + doc fixes  `[sonnet]`

**(a) What.** `core/app/src/lib.rs` (`mailto:` 899) +
`docs/reference/hetzner-deploy-checklist.md:22`, `modules/apikeys/src/admin.rs` (`_new_*`
collision ~102-233) + `store.rs`, `api/match/api/src/lib.rs` (report doc 31-36) +
`CLAUDE.md:244` + `modules/match/src/lib.rs:137` (garbled comment).

**(b) Why here.** All independent, all mechanical with fully-specified edits; batched.

> **REMOVED (harness rewrite):** the former `stop_pid` (bash) and `Start-Svc`/`winctrl` (ps1)
> teardown-leak fixes are gone — `split-proof.{sh,ps1}` + `tools/winctrl` were deleted at HEAD
> and the Rust harness's kill-on-drop guard makes the leak structurally impossible. Nothing to
> fix there.

**(c) How.**
1. `mailto:`: normalize code-side (`c.strip_prefix("mailto:").unwrap_or(c)` before
   `format!("mailto:{email}")`) **and** fix the doc to `ACME_CONTACT=you@example.com` (bare
   email matches the field's documented contract). Extract a tiny testable helper; unit-test
   both input forms.
2. apikeys `_new_*`: reject `_`-prefixed key names at creation (in `apply_edit`'s insert branch
   alongside `check_policy`, and push into `store::insert`/`insert_tx` so non-admin paths are
   guarded too). Unit test the rejection.
3. match doc: rewrite the `report` doc to state **both** outcomes (same payload → 202 no-op;
   different winner/loser under same ReportId → 409 Conflict); tighten `CLAUDE.md:244`; delete
   the garbled `The`/`An existing` leftover at match/lib.rs:137. Confirm `modules/match/src/tests.rs`
   covers the conflicting-payload 409 (add if missing — that would be a real test gap, comment
   fixes need none).

**(d) Tag:** `[sonnet]` — every edit is spelled out; no design latitude.

---

## Step 6b — Checker self-checks (independent, mechanical)  `[sonnet]`

**(a) What.** `tools/topiccheck/src/tests.rs` (define-site self-check); `verify.sh`/`verify.ps1`
(orphan-baseline check, in lockstep).

**(b) Why separate from Step 7 (m2).** These have **zero** dependency on routecheck or Step 1 —
they close "hand-list silently stops mattering" gaps and can land anytime as their own commits.
Bundling them into Step 7 delayed independent wins and blurred the commit.

**(c) How.**
1. **topiccheck define-site self-check:** a `#[test]` mirroring
   `checkmodules::split_fleet_matches_cmd_dirs` — scan `api/*/events/src/lib.rs` for `define(`
   lines (comment-filtered text scan, like archcheck's text tripwires), diff the topic-literal
   set against `defined_topics()` (currently 7 sites, complete), die per-drift naming the file.
2. **Orphan-baseline check:** after deriving the live crate list, assert every
   `docs/reference/public-api-baseline/*.txt` has a matching live crate; a leftover snapshot
   (deleted domain) fails loudly. Add to both `verify.sh`/`verify.ps1` (kept in lockstep).

**(d) Tag:** `[sonnet]` — mirror existing patterns, no design latitude.

---

## Step 6c — Value-level contract golden (Decision 3, lands now)  `[fable]`

**(a) What.** New golden fixture under `docs/reference/contract-golden/` + a check that produces
and diffs it; wired into `verify.sh`/`verify.ps1` as a blocking stage with a `--bless` flow
mirroring `public-api-baseline`. Reads `topiccheck::defined_topics()` (already the canonical
event-contract list) and each domain's generated `operations()`.

**(b) Why its own step.** It closes the gap cargo-public-api structurally *cannot* see — the
**runtime values** inside `define(...)` and inside each `OpSet` (which are constructed in
function bodies / static initializers, invisible to a signature-level tool). It's design-y
(where the golden lives, the bless flow) so it's `[fable]`, not `[sonnet]`. Independent of every
other step except that it reuses `defined_topics()` (unchanged) and `operations()` (unchanged).

**(c) How.**
1. **Where it lives:** extend `tools/topiccheck` (it already builds every process with the
   recording-transport harness and owns `defined_topics()`), OR a sibling `tools/contractcheck`.
   Recommend extending topiccheck (a new subcommand/stage) to avoid a second harness — decide in
   the step; both are viable, no research gap.
2. **Event-contract golden:** for each `defined_topics()` entry emit a stable line
   `topic=<s> version=<n> history=<policy>` (sorted). This catches a silent topic-string edit,
   a version bump, or a retention change that the type-level public-api baseline misses.
3. **RPC-contract golden:** for each domain, call the generated `operations()` (the same call
   routecheck uses) and emit each `OpSet`'s `(method, http_verb, http_path, auth, success_status,
   retry_mode)` tuple (sorted). This catches a changed HTTP path/verb/auth/status or a flipped
   `#[retry_safe]` — none of which the signature baseline sees.
4. **Bless flow:** `--bless` writes the golden; the default run diffs the live values against the
   committed golden and FAILs on any diff (removed = breaking, added = additive), exactly like
   `verify.sh --bless-public-api`. Wire a blocking `contract-golden` stage into both verify
   scripts (lockstep).
5. **Bless the initial golden** as part of this step's commit (the current values are the
   baseline).

**Tests (this step):** the check IS the test (a `#[test]` that runs it over the workspace and
asserts the committed golden matches, mirroring how archcheck/checkmodules self-test). DB-free —
`operations()` and `defined_topics()` need no pool.

**(d) Tag:** `[fable]` — golden format + bless-flow design (where it lives, diff semantics).

---

## Step 7 — `tools/routecheck` (the general net for topology parity)  `[fable]`

**(a) What.** New `tools/routecheck` (bin, ~250 lines); `core/edge/src/server.rs` (add
`pub fn methods(&self) -> Vec<String>`); `verify.sh`/`verify.ps1` (wire a BLOCKING stage);
mandatory `[I-GATE]` dynamic assertion in `tools/splitproof/src/main.rs`.

**(b) Why LAST.** `routecheck` codifies the invariant Step 1 establishes; wiring it as a
blocking stage before Step 1 lands would red the tree. It comes after its target is green.

**(c) How.**
1. **`edge::Server::methods()`** — additive accessor returning keys of the exact + identity
   handler maps. `Server::new()` binds no socket (only `listen` does), so a checker can build a
   fresh `Server`, apply every `EdgeReg` (one-shot `FnOnce`), and read the served method set with
   zero I/O.
2. **`tools/routecheck`** (clone topiccheck's `observe` harness shape; consume
   `checkmodules::DeploymentProfile`): for each process, `App::build` (register→init, lazy pool,
   no-op transport) then read `ctx.contributions::<opsapi::Operation>(opsapi::SLOT)` etc.
   (`Operation` is `Clone+Eq` by design). Assert, over **both** env configs (all-gates-unset =
   fail-closed default, and all-gates-on):
   - **FRONT-PARITY:** `ops(monolith server) == ops(split gateway-svc)` (symmetric-diff
     reported) — *this is the inventory-bug catcher; the unset run alone catches it.*
   - **PER-PROCESS INTEGRITY:** `methods(ops(p)) == binds(p)` (an op without a binding = a
     skipped route); monolith `methods(ops) ⊆ locals`.
   - **SPLIT SERVE-PARITY:** `methods(ops(gateway-svc)) ⊆ ⋃ edge(domain-svc)` (every fronted op
     is actually served — catches "gate only the front" half-fixes).
   After Step 1+3 make contribution unconditional, front-parity degenerates to pure structural
   equality — a durable tripwire for future modules. **B1 (resolved by Decision A):** Step 1
   unconditionalizes all three accounts ops (register/login/**loginEpic**) plus inventory-grant,
   so the two front sets are equal by construction and no per-op env juggling or declared
   exception is needed in routecheck.
   - **Env mechanics (m3):** routecheck flips gate env vars with `std::env::set_var`, which is
     unsound once a threaded runtime exists, and `App::build` reads env in `register`. Set all
     env for a config **before** creating any runtime/threads; run the two configs sequentially
     in a defined order (unset-first, then on). One `main`, no shared runtime across configs.
3. **`[I-GATE]` dynamic complement — MANDATORY, in the Rust harness (M2).** The harness boots the
   whole fleet with `INVENTORY_DEV_GRANT=1` (main.rs:138), so it structurally can't catch this
   bug; the house rule ("verify the at-risk path, not the safe one") demands a committed live
   proof for a security fix. Add a named `[I-GATE]` assertion in `tools/splitproof/src/main.rs`
   (called from `run()` after `assertions(...)`, before the monolith-parity teardown at
   main.rs:497):
   - Drop *only* the inventory-svc `Running` guard (frees its HTTP + edge ports), respawn it via
     a modified `Svc` whose env **omits** `INVENTORY_DEV_GRANT` (clone the inventory `Svc`, strip
     that one pair), `wait_healthy`. gateway-svc's `remote::Stub` re-resolves the peer on the next
     dial, so the restart is transparent to the front — no gateway restart needed.
   - `POST /inventory/me/grant` **through gateway-svc** (:8082) with a valid `X-Api-Key` +
     bearer (per M1 the unauthed response is now 401, so the probe MUST be fully authed) →
     assert **404**. A parallel positive control (the same call with the flag on, already covered
     by the main assertions) anchors it.
   - No restore needed: the monolith-parity phase tears the whole split down immediately after.
   The static routecheck carries the general class; `[I-GATE]` proves this specific fix live in
   the split. (The harness's existing `preflight_fleet` fleet-drift tripwire already covers the
   "new svc not in the harness" gap — no addition there.)

**(d) Tags:** `routecheck` + `Server::methods` = `[fable]` (new-tool design, harness reuse);
`[I-GATE]` assertion in `tools/splitproof` = `[sonnet]` (mechanical, one restart-and-probe in an
existing Rust harness). The topiccheck/orphan self-checks moved to **Step 6b**.

---

## Sequencing summary

```
Step 1   inventory+accounts gate       [fable]  ← HIGH; precedes 3 and 7
Step 2   player-QUIC bounds            [fable]  ← HIGH; independent
Step 3   accounts argon2/JWKS          [fable]  ← MED-security; AFTER 1 (same module)
Step 4a  asyncevents liveness          [fable]  ← MED; not concurrent with 6 (core/app overlap)
Step 4b  scheduler liveness + B2       [fable]  ← MED; independent
Step 5   remote retry seam             [fable]  ← MED; independent
Step 6   minors + docs                 [sonnet] ← not concurrent with 4a (core/app overlap)
Step 6b  checker self-checks           [sonnet] ← independent
Step 6c  value-level contract golden   [fable]  ← independent (reuses defined_topics/operations)
Step 7   routecheck + [I-GATE]         [fable]/[sonnet] ← LAST (enforces Step 1)
```

**Hard edges:** 1 → 3 (same module `accounts`, overlapping test files — never concurrent);
1 → 7 (routecheck enforces Step 1's invariant). **Soft conflicts (don't run concurrently, order
free):** 4a ↔ 6 (both edit `core/app/src/lib.rs`). Everything else is order-independent. Commit
after each step. Trailer audit before "done": `git log -9 --format="%h %B" | grep Co-Authored`
matches each lane (`[fable]`→Fable 5, `[sonnet]`→Sonnet 4.6).

## Test-catch matrix (what would have caught each finding)

| Finding | Test that now catches it | DB? |
|---|---|---|
| inventory split bypass | `routecheck` front-parity (every env) + `[I-GATE]` live 404 + inventory unit gate test | mixed |
| player-QUIC pin | `player_tests` reap-after-grace (shrunk seam + in_flight poll) | no |
| accounts oracle/argon | decoy-path + permit-survives-cancel + concurrency-cap unit tests | no |
| JWKS amplifier / 503 | JWKS stub hit-count + 500→503 unit test | no |
| interval(0) panic | parser reject `#[test]` (0 rejected, "nonsense" still falls back) | no |
| unsupervised task death | liveness-flag transition unit + (heavier) plane panic test | mixed |
| scheduler hang | tick `statement_timeout` → error-not-wedge + readyz flag (DB, lands now) | yes |
| remote NotFound reset | `notfound_does_not_reset_or_redial` + `internal_status_still_resets` fake-Dialer | no |
| new define not in list | topiccheck define-site self-check | no |
| orphan baseline | verify orphan-baseline check | n/a |
| topic/version/retention/rpc value edit | Step 6c contract golden diff | no |

## Decisions (resolved with the user)

1. **CRUX → Option A (unconditional op contribution + impl-guard).** All four dev-gated ops
   (register / login / **loginEpic** / inventory-grant) are *always* contributed, gated only at
   the impl, making `routecheck` a pure structural-equality check that catches this bug class in
   every env — the strongest general net. **Accepted cost:** a gated route returns **401/403**
   (not 404) to an unauthenticated caller (a method-existence oracle limited to dev-gated routes),
   and disabled-epic shifts 404→503. Steps 1 and 7 are written to this decision; no hand-list of
   gated exceptions.
2. **Heavier DB tests → LAND NOW.** The panicking-task-flips-readyz (4a) and scheduler-hang (4b)
   DB tests are in this rollout, incl. the test-only panic/lock-injection seams they need. They
   run serialized (one-test-rollout-at-a-time) and clean-skip when Postgres is unreachable.
3. **Value-level contract golden → LAND NOW** as **Step 6c** (not a follow-up). Reuse
   `topiccheck::defined_topics()` (topic/version/history) + each domain's generated
   `operations()` (rpc method/path/auth/status/retry_safe) → a committed golden with a `--bless`
   flow, wired as a blocking verify stage.
