# Plan: Core review fixes (findings #1–#5 + stale comments)

> **OUTCOME (all steps landed + independently reviewed CLEAN):**
> Step 0 `f01d084` (docs, Sonnet) · Step 1 `41ba8f6` + follow-up `534d86a` (edge abort) ·
> Step 2 `c1f1fb9` (app boot reorder) · Step 3 `e45bf8a` (lifecycle lock) ·
> Step 4 `956d58a` (invalidation) · Step 5 `f65ae78` (bus). Each behavioral diff got one
> adversarial `core-reviewer` pass (Opus); the Step-1 review found the no-leak test didn't
> exercise its own failing branch (fixed in `534d86a`). Sibling sweep: the asyncevents plane
> migrate lock uses `pg_advisory_xact_lock` (tx-scoped, auto-released) — no twin of the
> Step-3 defect.

## Context

An external code review (Codex, in Polish) raised 5 findings across the `core/*`
seams plus two stale doc comments. I verified every claim against the actual code
(three Explore agents + my own targeted reads). Conclusion: **none of #1–#5 is a
live production bug on a correct build** — but each is a real latent gap or a
claim-stronger-than-code mismatch, which is exactly the failure class our taxonomy
warns about. The user asked to fix all of them.

Honest framing of what each fix actually closes (so we don't gold-plate):

| # | Defect | Live today? | This fix closes |
|---|--------|-------------|-----------------|
| 1 | QUIC handler futures not aborted past grace | Doc claims drain that code doesn't deliver | Makes `RunningServer::shutdown` truly abort straggler handler futures; proves it |
| 2 | Late-boot panic (dup edge method / dup route) sits after `app.start()` → skips unwind | No (deterministic wiring error, caught in dev) | Reorders panic-capable finalization before `start` so nothing is started when it fires |
| 3 | Migrate session advisory lock leaked on panic/cancel of the locked section | Near-zero (no cancel point; `lock_timeout` bounds next attempt) | RAII guard that ends the PG session on drop → auto-releases the lock |
| 4 | Invalidation callbacks can self-overlap + reorder; `stop()` doesn't await post-abort | No (both consumers have monotonic guards) | Serialize per-callback + document the contract; await after abort |
| 5 | Local bus unbounded channel + unbounded `close()` drain | No (zero production consumers) | Bound the channel + deadline the drain |

Each finding lives in a **different core crate**, so they are independent and each
ships as its own reviewed commit. No cross-step dependency forces an order; we
sequence by value/risk and by the one-test-rollout-at-a-time constraint.

**Verification discipline (MANDATORY):** at most ONE rollout-bearing command at a
time (shared local Postgres). `core/edge` and `core/bus` tests are in-process (no
DB) and safe to run freely; `core/lifecycle`, `core/invalidation`, `core/app` tests
hit Postgres and MUST be serialized — never two at once. Use the `safe-verification`
skill's pre-flight (`cargo`/`rustc` clear + `devctl status`) before any DB-touching
test run.

**Dispatch:** every substantive step is `core/*`/cross-seam → the `core-implementer`
agent (`model:"opus"`, session tier), and every landed diff gets ONE `core-reviewer`
adversarial pass (`model:"opus"`) attacking the fix's own new seams. None of these
steps touch a verify stage (verifyctl/archcheck/topiccheck/golden), so `proof-auditor`
is not needed. Stale-comment edits are mechanical → `[sonnet]`.

---

## Step 0 — Fix the two stale comments  `[sonnet]`

**What.** Doc-only, no behavior change. Do first (zero risk, unblocks a clean base).
- `core/asyncevents/src/store.rs:1-14` (and the companion line ~373 "Not wired into
  `Transport::enqueue_tx` until the plan-Step-3 cutover"): the pull cutover is DONE.
  `transport.rs:95-110` (`LogTransport::enqueue_tx`) calls `store::append(...)`
  directly, and `lib.rs:97-104` (`LEGACY_DROP_DDL`) drops `asyncevents.outbox/inbox`
  + `schema messaging`; there is no `crate::producer` module. Rewrite the header to
  describe current reality (store IS the live append path; outbox/inbox removed).
- `core/app/src/lib.rs:624-627` (the `run()` doc block, step 6): it says
  durable-plane-then-invalidation; the code (lib.rs:849-854) does
  invalidation-then-durable — and the *inline* comment at 843-848 already states the
  correct order + rationale (cold-cache invariant). Flip the doc block to match the
  code, and have it **explicitly cross-reference the start≠teardown asymmetry** (start
  = invalidation-then-durable for the cold-cache invariant; teardown = durable-then-
  invalidation) so a future reader does not "fix" the (correct) teardown doc at step 10
  (lib.rs:638-645) to match. Keep the teardown doc unchanged.

**Why now / order.** Independent of everything; smallest, lowest-risk; keeps the
later behavioral commits from mixing in doc churn.

**Verify.** `cargo build -p asyncevents -p app` (doc comments compile-check via
`cargo doc` not required). No test.

**Commit.** `docs(asyncevents,app): correct stale store.rs cutover + boot-order doc comments`

---

## Step 1 — Finding #1: truly abort QUIC straggler handlers past grace  `[opus]` (core-implementer)

**What is touched.**
- `core/edge/src/server.rs`: `ShutdownState` (246-250), `enter()` (271-274),
  `InFlightGuard` + its `Drop` (303-311), the stream spawn site (434) and the
  conn-level spawn site (~218), `RunningServer::shutdown` (343-355).
- `core/edge/src/player.rs`: the conn spawn (294-295) and stream spawn (503) sites —
  **no player-specific type change**, because `player.rs:33` imports `ShutdownState`,
  `InFlightGuard`, `RunningServer` from `server.rs` (shared). Fixing the shared type +
  wiring every `enter()` call site covers both planes.
- `core/edge/src/shutdown_tests.rs`: add the cancellation-proof test.

**Why now / order.** Highest-value real fix (closes a documented-drain guarantee the
code doesn't honor). Independent of the other steps.

**How (non-mechanical).** Fold an abort registry into the SAME lifecycle that already
drives the `in_flight` counter, so the set we abort is exactly the set `idle()` waits
to drain (invariant: registry membership == in_flight membership). There are exactly
**two** `enter()` sites — the connection task (server.rs:217) and the stream task
(server.rs:433); the accept-*loop* task (server.rs:204) takes no guard and exits on
the closing flag, so it is not tracked (correct). Aborting the connection-level entry
does NOT reach its already-spawned stream tasks — so we MUST track and abort the
stream-level entries too; tracking every `enter()` site does exactly that.
- Add to `ShutdownState`: `handles: Mutex<HashMap<u64, Option<AbortHandle>>>` +
  `next_id: AtomicU64`.
- **Close the track()-after-spawn race (reviewer 1.1 — was a blocker).** `enter()`
  allocates `id`, increments `in_flight`, AND inserts `id → None` into `handles` under
  the lock, returning an `InFlightGuard` carrying `id`. After `tokio::spawn`,
  `track(id, abort_handle)` does `if let Some(slot) = map.get_mut(&id) { *slot =
  Some(abort_handle) }` — **fills only if the entry is still present**, never
  re-inserts. `InFlightGuard::drop` removes `id` (in addition to decrement+notify). So
  a fast task that finishes before `track()` runs: `drop` removed `id` → `track`'s
  `get_mut` is `None` → skip → **no stale handle leaks**. The reverse order fills then
  removes. This is the only ordering that keeps membership == in_flight.
- `RunningServer::shutdown`: after the `timeout(grace, idle())` straggler branch,
  **snapshot** the live `AbortHandle`s into a `Vec` under the lock, **drop the guard**,
  then `abort()` each (reviewer 1.2 — aborted tasks' guard-drops contend on the same
  `std::Mutex`, so never abort while holding it). THEN `endpoint.close(...)`, THEN the
  bounded `wait_idle()`. Aborting drops the guards → `in_flight` → 0.
- **Lock discipline (reviewer 1.2):** the `std::Mutex<HashMap>` is a leaf lock — never
  held across an `.await`. State and uphold this invariant in the code.
- Precedent to mirror for shape/wording: `serve_http` in `core/app/src/lib.rs` +
  `serve_http_cancels_hung_handler_at_grace_and_never_wakes_it` in
  `core/app/src/tests.rs` (HTTP plane already does exactly this).

**Prove the failing branch (tests — two).**
- *Cancellation:* port the app-plane precedent into `shutdown_tests.rs`: extend
  `signaling_slow_echo`'s handler with an `Arc<AtomicBool> resumed` set ONLY after its
  `sleep(delay)` resolves (BEFORE the echo write, so drop-vs-disconnect is a real
  distinction — reviewer 1.4 confirmed `endpoint.close()` alone does not stop the
  spawned task, so without the abort `resumed` flips at t=delay). Test: `delay` = 5s,
  `shutdown(200ms)`; after `shutdown` returns, sleep past 5s, assert `!resumed`. Add
  the player-plane analog (shared struct, but prove the player stream spawn site
  server-side, since CLAUDE.md names both planes).
- *No leak (pins reviewer 1.1):* fire thousands of immediately-returning requests,
  drain, then assert the `handles` map length returns to 0 — proves a normally-
  completing task leaves no `AbortHandle` behind (the blocker, made a permanent test).

**Verify.** `cargo test -p edge shutdown` (in-process, no DB — safe).

**Review.** `core-reviewer` (`model:"opus"`): confirm the enter-time-slot +
fill-if-present ordering actually holds membership==in_flight under a stress loop;
confirm the `std::Mutex` is never held across an `.await`; confirm both `enter()` sites
(conn + stream) are tracked so no stream task is orphaned.

**Commit.** `fix(edge): abort straggler QUIC handler futures at shutdown grace, both planes`

---

## Step 2 — Finding #2: fail dup-registration panics before `app.start()`  `[opus]` (core-implementer)

**What is touched.** `core/app/src/lib.rs` — the `outcome` block (831-1039):
`apply_edge_registrations` / `mem::take` server (862-878), the player finalize
(886-905), and `ctx.take_router().route("/healthz"|"/readyz")` (919-931).

**Why now / order.** Independent. Pure sequencing hardening; do after #1 so the two
core diffs stay separately reviewable.

**How (non-mechanical).** Split the panic-capable, non-I/O finalization from the I/O
bind. Today the order is `migrate → start → apply_edge_registrations → listen →
take_router+route → serve`. The panics are in `apply_edge_registrations`
(`server.handle` panics on a dup wire method — `server.rs:139`) and `.route` (axum
panics on a dup path) — NOT in `.listen()` (the actual I/O). Move the fully-registered
`edge::Server` construction (`mem::take` + `apply_edge_registrations`) and the router
construction (`take_router().route(...)`) to run right AFTER `App::build` /
`validate_requires` and BEFORE `app.migrate()`/`app.start()`. Keep the actual
`.listen()` binds and HTTP `serve_http` where they are (post-start, first I/O per
lifecycle law). Hold the built `Server`/`Router` in `let mut` bindings threaded into
the later listen/serve. **Keep the panics** — dup registration is a documented "loud
boot failure" (CLAUDE.md: `edge::Server::handle` PANICs by design); do NOT convert to
`Err`. The fix is purely *when* it fires: with the started-set empty, a panic has
nothing to unwind.

**Prove.** Unit test in `core/app/src/tests.rs`: build an app whose module set
registers the same edge wire method twice (or mounts a route colliding with
`/readyz`), run `app::run` far enough to hit finalization, assert it panics BEFORE any
module `start` ran (e.g. a probe module records `started=true` in `start`; assert the
flag is still false at panic — `catch_unwind` around the boot future). This proves the
reorder, not just that a panic still happens.

**Verify.** `cargo test -p app` — **DB-touching, serialize** (safe-verification
pre-flight first).

**Invariant being bought (reviewer 2.2).** State it in the code comment: a
dup-registration *panic* (server.rs:139, axum `.route`) UNWINDS the stack — it never
reaches `ordered_teardown` (that runs only on the `outcome` `Err` path). The reorder
does not make the panic "unwind through teardown cleanly"; it makes the panic fire when
`modules_started == false` and nothing is started, so there is nothing to tear down.
That is the whole value.

**Review.** `core-reviewer` (`model:"opus"`): reviewer 2.1 already verified every
module mounts routes/edge/readyz in `init` (gateway/scheduler/config/audit/rating/
apikeys), none in `migrate`/`start`, and the `/readyz` closure's captures exist at
build — re-confirm on the diff that nothing router/edge-related is added post-build.

**Commit.** `fix(app): finalize edge/router registrations before module start so dup-registration panics unwind cleanly`

---

## Step 3 — Finding #3: cancel/panic-safe module-migrate advisory lock  `[opus]` (core-implementer)

**What is touched.** `core/lifecycle/src/app.rs` — `migrate_with_lock_timeout`
(122-195), `MODULE_MIGRATE_LOCK_KEY` (21). New test in `core/lifecycle/src/tests.rs`.

**Why now / order.** Independent. Defensive hardening; low live exposure but real
footgun (sqlx does not release a session `pg_advisory_lock` when a pooled connection
returns to the pool).

**How (non-mechanical).** The unlock/reset today sit as bare statements after
`run_migrations().await` (177) — skipped by a panic in a module's `migrate` or a drop
of the future. Async `pg_advisory_unlock` cannot run from a sync `Drop`, and no house
pattern does async-cleanup-on-Drop. So use session termination as the release
mechanism: `.detach()` the lock connection from the pool into an owned `PgConnection`,
and wrap it in a small RAII guard that **owns the detached connection**. Happy path:
explicit `pg_advisory_unlock` (for prompt, deterministic release before the next
migrate), then let the guard drop. Panic/cancel path: the guard's `Drop` drops the
owned `PgConnection` WITHOUT returning it to the pool → the socket closes → the PG
backend session ends → the session-scoped advisory lock is released automatically.
This is the authority fix: release becomes structural (tied to connection ownership)
instead of a fall-through statement.
- **Drop the dead `RESET lock_timeout` (reviewer 3.1).** `RESET` exists today ONLY
  because the pooled connection carries the GUC back (app.rs:182-185 comment). A
  detached-then-dropped connection never re-enters the pool, so the GUC cannot ride
  back — `RESET` is dead code after the switch. Remove it (keeping it is the exact
  "hack beside the earlier hack" the authority discipline bans).
- **No `defuse()` / double-unlock (reviewer 3.2).** A sync `Drop` can only close the
  socket, never run an async `pg_advisory_unlock` — there is no second unlock to guard
  against. The guard is simply "owns the detached connection; Drop closes it." The
  happy-path explicit unlock is a promptness optimization, not a correctness
  requirement (Drop would release it anyway via session end). Do NOT model a `defuse`.
- **Pool invariant (reviewer 3.4).** The existing "pool max ≥ 2 during migrate"
  invariant (app.rs:128-133) must still hold AFTER `.detach()` — the detached conn no
  longer counts toward the pool's live set while subsequent module migrations acquire
  fresh pool connections. Confirm the default pool size covers it (it does; comfortably
  above 2).

**Prove the failing branch (test — reviewer 3.3 flake pin).** In
`core/lifecycle/src/tests.rs`: a module whose `migrate` panics while the lock is held
(`catch_unwind` the first migrate), then a SUBSEQUENT `migrate` on the SAME pool must
acquire the lock. **The release is via socket-close→backend-EOF (usually ms, but not
instant), so the ASSERTING migrate must use a GENEROUS `lock_timeout`** (or poll
`pg_locks` for release), NOT a millisecond timeout — otherwise the test measures
"released within N ms of socket close" and flakes with 55P03 instead of "released".
Without the guard this deadlocks; with it, the panicked session's lock is released. Run
`--test-threads=1` per the asyncevents memory note.

**Verify.** `cargo test -p lifecycle -- --test-threads=1` — **DB-touching,
serialize.**

**Review.** `core-reviewer` (`model:"opus"`): confirm `.detach()`+drop sends a socket
close PG acts on; confirm `RESET` is gone and the happy-path unlock still runs; confirm
the guard has no phantom defuse and no double-unlock model.

**Commit.** `fix(lifecycle): release module-migrate advisory lock via session teardown on panic/cancel`

---

## Step 4 — Finding #4: serialize invalidation callbacks + document contract + await post-abort  `[opus]` (core-implementer)

**What is touched.** `core/invalidation/src/lib.rs` — `register` (122-146) doc +
`Registration` (82-87) + `run_one` (210-218) + `stop()` (360-370). Doc cross-refs:
`CLAUDE.md:178`, `AGENTS.md:177`, `.claude/skills/add-game-module/SKILL.md:30`.

**Why now / order.** Independent. No live bug (both registrants —
`modules/config/src/lib.rs:739` and `api/config/rpc` `CachedConfig` — already gate on
a monotonic `revision <= guard.revision` check). The gap is the *contract*: the plane
provides zero enforcement, and `listen` + `poll_loop` are separate tasks that can
invoke the same callback concurrently.

**How (non-mechanical).**
- (a) **Serialize per callback.** Add `Arc<tokio::sync::Mutex<()>>` to `Registration`,
  **created once at `register` (lib.rs:141-145) so the `Arc` is SHARED across the
  `Registration::Clone` (lib.rs:82-87) into both `all` and `by_channel` (reviewer 4.3
  — if the mutex were created per-snapshot it would serialize nothing)**. Hold it for
  the duration of `run_one`'s `reg.refresh().await`. A given callback never overlaps
  ITSELF across the listener/poll tasks, so the second invocation reads a fresh
  snapshot after the first completes — combined with each callback's own monotonic
  guard, the reorder window closes. Different callbacks still run concurrently.
- **Accept the HOL cost, don't defer it (reviewer 4.2).** `refresh_all`/`run_channel`
  already iterate callbacks SEQUENTIALLY within one task, so a slow callback already
  blocks its siblings within a pass. The new mutex adds cross-task coupling: a poll
  pass reaching callback A while the listener holds A's mutex waits up to A's remaining
  `callback_timeout` (10s, lib.rs:203) before proceeding to B/C. This is BOUNDED and
  acceptable for a freshness floor — state it in the doc. Do NOT use `try_lock`-skip:
  skipping could drop the newest snapshot (an in-flight run may have queried before the
  latest commit), so queueing is the correct choice.
- (b) **Document the contract** on `register`'s doc comment: callbacks MUST be
  idempotent and apply-only-newer (monotonic); the plane serializes a callback against
  itself but does not order it against a newer external write — the callback owns
  freshness (mirrors `lib.rs:19-22` "atomicity is the callback's job").
- (c) **BOUNDED await after abort in `stop()` (reviewer 4.1 — was a blocker).** The
  current `stop()` is deliberately fire-and-forget after abort (lib.rs:355-359) so a
  callback CPU-spinning without yielding cannot stall teardown — a Tokio `abort()`
  can't preempt a non-cooperative future. An UNBOUNDED `let _ = (&mut t).await` after
  abort would re-introduce exactly that stall. Use a BOUNDED join instead: after
  `t.abort()`, `let _ = tokio::time::timeout(small_grace, &mut t).await;` — confirms
  the cooperative case unwound, still bounded for the non-cooperative case.

**Prove.** Tests in `core/invalidation/src/tests.rs`: (1) a callback that increments an
`AtomicUsize` on entry and asserts it never exceeds 1 while sleeping, fired via
`run_channel` + `refresh_all` concurrently — assert no overlap. (2) a `stop()` test
that a cooperative mid-flight aborted callback is joined before `stop()` returns, AND
that a deliberately non-cooperative (spin) callback does NOT stall `stop()` past
`small_grace` (pins the bounded-await branch, not just its existence).

**Verify.** `cargo test -p invalidation` — **DB-touching, serialize.**

**Review.** `core-reviewer` (`model:"opus"`): confirm the post-abort await is BOUNDED;
confirm holding the per-callback tokio::Mutex across `stop()`'s abort can't deadlock
(abort drops the future → releases the Mutex); confirm the HOL bound is stated.

**Commit.** `fix(invalidation): serialize per-callback refreshes, document monotonic contract, join aborted tasks`

---

## Step 5 — Finding #5: bound the local bus channel + deadline its drain  `[opus]` (core-implementer)

**What is touched.** `core/bus/src/lib.rs` — `subscribe` (115-136, the
`mpsc::unbounded_channel` at 119), `publish`/`emit` (140-157), `close` (221-230).

**Why now / order.** Independent, fully latent. Do last (lowest value).

**Pre-flight — confirm zero consumers, don't rely on one grep (reviewer 5.3).** Per
research-mode, one grep is a lower bound. Before shipping a lossy change, confirm via
callers/LSP that NOTHING reaches the local `subscribe`/`publish` path — including
through the typed `Bus::on`/`Bus::emit` wrappers (lib.rs:152-181) and any re-exports.
The three agents + grep found only `core/bus`'s own tests; the implementer re-confirms
with an LSP callers check on `subscribe`/`publish`/`Bus::on`/`Bus::emit`. If ANY
same-module reaction relies on delivery, drop-on-full is a regression — escalate.

**How (non-mechanical).**
- Replace `mpsc::unbounded_channel` with bounded `mpsc::channel(LOCAL_BUS_CAP)` —
  **choose a CONCRETE value (reviewer 5.2), e.g. `const LOCAL_BUS_CAP: usize = 1024`,
  and confirm no existing `core/bus` test emits a burst > CAP before its subscriber
  drains** (else the drop path fails those tests). `publish` is sync fire-and-forget →
  `try_send`; on `Full` drop with `tracing::warn!(topic, "bus: local subscriber
  lagging; dropping event")`.
- **Semantic change — record it, don't smuggle it (reviewer 5.4).** unbounded-buffer →
  bounded-with-drop-on-overflow. This CONTRADICTS the module doc header (lib.rs:6-11
  and the "shutdown is lossless and ordered" claim at lib.rs:121-122). **Rewrite those
  two doc blocks in the SAME diff** (delivery is now lossy under overload; shutdown
  drain is bounded), plus the commit body. Justification: a local same-module reaction
  bus must not be an unbounded memory sink; drop-with-warn is correct fire-and-forget
  backpressure.
- `close`: bound the drain — mirror the `EDGE_DRAIN_GRACE_MS` idiom
  (`core/app/src/lib.rs:640`): `tokio::time::timeout(grace, &mut task)` per task, then
  `task.abort()` on stragglers, then **return immediately (reviewer 5.1 — do NOT add an
  await after abort, same non-cooperative-stall trap as Step 4.1)**. Bus handlers are
  sync `Fn(&Event)` run inline (lib.rs:53,123-130), reaching an await only at
  `rx.recv()`, so a wedged handler is a sync spin `abort()` cannot preempt — the
  abort-then-return is the only bounded choice. Local grace const (5000ms shape).

**Prove.** Tests in `core/bus/src/tests.rs`: (a) a slow subscriber that never drains —
`publish` past `CAP` drops (warn path), map/channel does not grow unbounded / does not
block; (b) `close` with a wedged (spin) handler returns within the grace rather than
hanging.

**Verify.** `cargo test -p bus` (in-process, no DB — safe).

**Review.** `core-reviewer` (`model:"opus"`): confirm zero consumers was verified by
callers/LSP not just grep; confirm `close` returns immediately after abort (no stall);
confirm the doc header blocks were rewritten to match the lossy semantics; confirm CAP
doesn't break existing tests.

**Commit.** `fix(bus): bound local subscriber channel and deadline close() drain`

---

## Global verification (after all steps)

1. Per-step targeted `cargo test -p <crate>` as noted, **serialized** for the
   DB-touching crates (lifecycle/invalidation/app), free-running for edge/bus.
2. Trailer audit: `git log -6 --format="%h %B" | grep -i "Co-Authored"` — confirm each
   behavioral commit carries `Claude Opus 4.8` (core-implementer lane) and the
   stale-comment commit carries `Claude Sonnet 4.6`.
3. Final gate: `cargo run -p verifyctl -- --fast` (one rollout, nothing else running).
   This runs build/clippy/test/fortress/split-proof — proves both topologies still
   compile and the split still passes (none of these changes is topology-specific, but
   the edge shutdown + boot reorder touch the split's front planes, so split-proof is
   the at-risk check).
4. Persist this plan to `docs/plans/2026-07-14-2211-core-review-fixes-plan.md` and
   commit (`docs(plans): …`).

## Notes / non-goals

- We do NOT convert dup-registration panics to errors (#2) — panics are the documented
  loud-boot-failure convention.
- We do NOT restructure the migrate loop into a spawned task (#3) — the
  detach-and-close-on-Drop guard is the minimal sufficient closure.
- We do NOT add per-callback revision awareness to the invalidation plane (#4) — the
  plane can't know about revisions; serialization + documented contract is the closure.
- #5 is fixed despite zero consumers per the user's "all findings" scope; the drop-on-
  overflow semantic change is explicitly recorded.
