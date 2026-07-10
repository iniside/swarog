# Fix plan — XID text-ordering bug, QUIC drain, split-proof & public-api trust

Date: 2026-07-10. Source: 4 externally-reported findings, each verified in this
repo (main context read the cited code directly); 3 research subagents filled
the remaining gaps (quinn API + edge topology, retention/worker/eventctl query
inventory + test fixtures, verify/split-proof .ps1 twins + contract-crate
inventory).

## Context — what research established

**Finding 1 is BIGGER than reported.** The alias-shadowing shape
(`col::text AS col` in the SELECT list + bare `ORDER BY col` → Postgres
resolves the bare name to the TEXT output alias, sorting lexicographically:
`'999' > '1000'`) exists in FOUR places, not one:

| Site | Query | Impact |
|---|---|---|
| `core/asyncevents/src/retention.rs:132,135` | GC floor pick | may delete events still needed by the truly-slowest (incl. **paused/poison**) subscription once it lags past `min_retention_days` — silent data loss, breaks the "poison is never auto-skipped" contract |
| `core/asyncevents/src/worker.rs:127,134` | `deliver_one` "next eligible event" pick | **live delivery path**: on any digit-count crossing of `producer_xid` within a topic's eligible window, the worker picks the wrong "next" event — breaks per-subscription XID-order delivery |
| `tools/eventctl/src/lib.rs:233,239` | `skip` verb's failing-event pick | operator skips the wrong event |
| `core/asyncevents/src/retention_tests.rs:51-52` | test helper `append_positions` re-read | harmless today (single-tx = single XID) but same trap shape |

Safe by construction (verified, DO NOT touch): all WHERE/ON composite-row
comparisons (`(generation, producer_xid, tie_breaker) > ($..::xid8,..)` —
aliases are invisible to WHERE/ON), `refresh_blocked_gauge`
(retention.rs:206-207), eventctl `info`/`snapshot` (qualified `s.`-prefixed
ORDER BY/GROUP BY), `store_tests.rs:131-133` (alias `xid` ≠ column name).

Why current tests are blind: every retention/worker fixture appends all events
in ONE transaction (`append_positions`, retention_tests.rs:41-58) — one shared
`producer_xid`, so text-sort == numeric-sort trivially. XIDs cannot be pinned
on a live cluster (`pg_current_xact_id()` is instance-global), BUT:
`insert_sub` (retention_tests.rs:88-114) binds caller-supplied `cursor_xid`
strings through `$..::xid8` with no reality check, and test files are exempt
from the archcheck plane-table rule — so tests can INSERT synthetic positions
directly (events PK columns incl. `tie_breaker` need
`OVERRIDING SYSTEM VALUE` for direct inserts).

**Finding 2 (QUIC drain).** Both planes (`core/edge/src/server.rs:97-109`,
`core/edge/src/player.rs:131-159`) spawn accept-loop → per-connection →
per-stream tasks fully untracked; both return the same
`RunningServer { endpoint, local_addr }` whose `close()` (server.rs:130-132)
is just `endpoint.close()` — immediate abort of every in-flight stream. quinn
0.11.11 facts (read from the locked crate source): `Endpoint::close` aborts
ALL connections AND stops accepting (no separate "stop accepting" API);
`Endpoint::wait_idle()` only waits for already-initiated closes to finish
notifying peers (no timeout arg); the graceful pattern must be app-level:
stop polling `accept()`, track in-flight tasks yourself, then close+wait_idle.
Precedents: `Bus::close()` (core/bus/src/lib.rs:193-202) already awaits a
tracked `Vec<JoinHandle>`; the Go original tracked conns + WaitGroup but
force-closed BEFORE Wait (abort-then-wait — mirror its *tracking*, improve its
*ordering*). `ordered_teardown` (core/app/src/lib.rs:550-574) calls both
`close()`es synchronously and its doc comment (:226-230) falsely says
"players drain first". `RunningServer` users: core/app (production) + 12
direct `.close()` teardowns in core/edge/src/lib.rs tests.

**Finding 3 (split-proof).** `pg()` (split-proof.sh:167-170) and `Invoke-Sql`
(split-proof.ps1:148-152) hardcode `-U gamebackend -h localhost -d gamebackend`
+ `PGPASSWORD=gamebackend`, ignoring `DATABASE_URL`; each script also has ONE
inline duplicate of the same hardcode (sh:607-608, ps1:541). Services and the
monolith stage all correctly receive the overridable `DATABASE_URL`
(sh:113+each leg; ps1:102-103+each Start-Svc incl. monolith :1000). 6
assertion groups skip on missing psql via `note` (wipe→weaker-HTTP-fallback
sh:604-619/ps1:536-553; config live-reload sh:633/ps1:566; audit sh:752/ps1:669;
scheduler sh:802/ps1:711; match-audit sh:870/ps1:776; rating sh:906/ps1:812)
while the verdict (sh:1192-1197, ps1:1047-1053) still prints "all assertions
held" — `FAILS` counts only explicit `fail` calls.

**Finding 4 (public-api).** Both scripts: `cargo public-api` exit code
swallowed (`|| true` sh:214-215; unchecked `$LASTEXITCODE` ps1:180-182) — a
tool crash yields empty base+cur → empty diff → "ok". Baseline = detached
worktree of HEAD (sh:203-208, ps1:166-171) — guards ONLY uncommitted changes;
in this repo's commit-straight-to-master flow a committed break is never
caught. Crate list hand-maintained and stale: 9 of the 14 api+events crates;
missing `configapi`, `configevents`, `leaderboardapi`, `matchapi`, `ratingapi`
(full inventory: 9 api + 5 events + 11 rpc; rpc crates deliberately out of
scope). Stage is advisory (`--all`), blocking only under `--strict`.

**Why not new machinery:** every step below repairs an existing seam
(asyncevents queries, `RunningServer`, the two proof scripts, the public-api
stage) — no new tools, no new crates; finding 4's snapshot baseline reuses the
repo-as-source-of-truth convention (committed files, explicit bless) instead
of inventing tag/state machinery.

Banned-phrase check: every step names exact files/symbols; no TBDs.

---

## Step 1 — Fix XID text-ordering in all four sites + regression tests (finding 1) `[opus]`

**(a) What:** `core/asyncevents/src/retention.rs`, `core/asyncevents/src/worker.rs`,
`tools/eventctl/src/lib.rs`, `core/asyncevents/src/retention_tests.rs`,
`core/asyncevents/src/worker_tests.rs` (or the worker's existing test file —
locate `append_committed` at worker_tests.rs:99-104 and extend there).

**(b) Why now / order:** first — live data-loss + delivery-order bug; every
other step is tooling/lifecycle.

**(c) How:**
- Mechanical fix in all four sites: rename the text alias so the bare ORDER BY
  can no longer bind to it — `cursor_xid::text AS cursor_xid_text`
  (retention.rs:132; read via `f.get("cursor_xid_text")` at :147) and
  `producer_xid::text AS producer_xid_text` (worker.rs:127 with row-read at
  :150, eventctl lib.rs:233 with row-read at :255, retention_tests.rs:51 —
  positional `query_as`, no read rename). Keep the ORDER BY column lists
  as-is: with the alias renamed, the bare names resolve to the real
  `xid8`/table columns (numeric XID order).
- Consistency (reviewer item 4): ALSO rename the two currently-safe
  alias-equals-column sites — worker.rs:101 and eventctl lib.rs:219
  (`cursor_xid::text AS cursor_xid`, single-row selects with no ORDER BY
  today) — same `_text` suffix + row-read updates. Half-fixed traps
  contradict the rationale; zero alias-equals-column instances remain after
  this step (enforce with the sweep below).
- Add a comment at each site: `-- alias must NOT equal the column name:
  a bare ORDER BY prefers the output alias (text sort) over the xid8 column`.
- Fixture rules for ALL synthetic-position tests (reviewer items 1-2):
  synthetic events INSERT into `asyncevents.events` with
  `OVERRIDING SYSTEM VALUE` (tie_breaker is `GENERATED ALWAYS`); pin
  `generation = 0` in EVERY test (plane_meta seeds generation 1, store.rs:158,
  so `0 < plane_meta.generation` makes synthetic rows frontier-eligible
  deterministically — leaving current generation would make eligibility
  depend on the cluster's live XID counter); derive tie_breaker values from
  the existing `unique()` nanos helper, NOT hardcoded 1/2 — the events PK is
  `(generation, producer_xid, tie_breaker)` on a shared live DB and three
  tests seeding identical synthetic PKs collide concurrently and poison
  reruns after a panic. Topics come from `unique()` as usual so no synthetic
  row leaks into another test's queries (all reads are topic-filtered —
  verified). The raw INSERT fires the `events_notify` trigger; harmless
  (wakes pollers that poll anyway).
- Regression test A (floor pick) in retention_tests.rs,
  `floor_uses_numeric_xid_order_not_text`: two synthetic events
  (producer_xid `'999'::xid8` and `'1000'::xid8`, generation 0, unique ties,
  backdated `created_at`); two subscriptions via `insert_sub`: active at
  `(0,"1000",tie_hi)`, paused at `(0,"999",tie_lo)`. Correct numeric floor =
  the paused (0,999,tie_lo) → GC deletes nothing (the 999 event is AT the
  cursor, not below) → 2 events survive. Buggy text floor picks the active
  row ('1000' < '999' as text) → the composite DELETE (numeric, safe,
  retention.rs:154) kills the backdated 999 event → 1 survives. Assert
  count == 2. (Reviewer verified this discriminates.)
- Regression test B (worker next-pick) in worker_tests.rs (reuse
  `append_committed`-style harness): two synthetic events per the fixture
  rules (generation 0 — REQUIRED for deterministic eligibility, see above),
  both > the subscription cursor; run one delivery step; assert the '999'
  event delivers FIRST (buggy: '1000' text-sorts first).
- Regression test C (eventctl skip) in `tools/eventctl/src/lib_tests.rs` —
  the harness EXISTS (`#[path = "lib_tests.rs"]` at lib.rs:298-299): same
  two-event seed; `skip` must advance past '999', not '1000'.
- Sweep: `grep -rn "::text AS" core/ tools/` and confirm ZERO remaining
  alias-equals-column instances (after the consistency renames above there
  should be none, including the no-ORDER-BY ones).

**(d) Verify:** `cargo test --workspace` (ONE invocation),
`cargo clippy --workspace --all-targets -- -D warnings`,
`cargo run -p archcheck`, `pwsh -File .\split-proof.ps1` (scheduler/audit
assertions exercise the fixed worker query against live XIDs).

## Step 2 — split-proof: DSN-honest SQL helper + psql required (finding 3) `[sonnet]`

**(a) What:** `split-proof.sh`, `split-proof.ps1`, CLAUDE.md/AGENTS.md
(split-proof description if it mentions psql fallbacks).

**(b) Why now / order:** before Steps 3-4 land more verify surface; split-proof
is the blocking net every later step leans on.

**(c) How:**
- NO DSN parser (reviewer item 6 — anti-magic): psql accepts a connection URI
  natively. `pg()` becomes `"$PSQL" "$DATABASE_URL" -t -A -c "$1"` and
  `Invoke-Sql` becomes `& $Psql $env:DATABASE_URL -t -A -c $Sql` — zero
  parsing; percent-encoded passwords and `sslmode` query params ride along
  for free. Drop `PGPASSWORD` exports (the URI carries credentials).
- Replace the two inline hardcoded psql invocations (sh:607-608, ps1:541)
  with calls through the fixed helper.
- psql becomes REQUIRED: if `find_psql`/`Find-Psql` comes back empty, die
  immediately at script start with
  `"split-proof: psql not found — local Postgres is the test DB and the DB
  assertions are mandatory; install psql or put it on PATH"` (exit 1).
  Delete ALL psql-conditional branches: the six skip sites + the
  weaker-HTTP-fallback wipe branch (sh:351-356,604-619,633,752,802,870,906;
  ps1:333-338,536-553,566,669,711,776,812) AND the guarded K3/K4 leaderboard
  cleanup missed in the first sweep (sh:540-542, ps1:491 — reviewer item 7).
  With psql guaranteed, all are dead code; the verdict lines then honestly
  mean what they say, no skip counter needed.
- Promote `[5b]` (sh:622-624, ps1 twin) from an echo-only evidence line to a
  real `pass`/`fail` assertion on the post-delete HTTP 404 — the deleted
  else-branch was the only place that 404 was ASSERTED (reviewer item 8);
  without this the gateway-path check silently disappears.
- Keep the `.sh`/`.ps1` twins semantically identical.

**(d) Verify:** `pwsh -File .\split-proof.ps1` (normal run); a negative check:
run once with `DATABASE_URL` pointing at a bogus db name and confirm the DB
assertions FAIL (proving the helper now follows the DSN) — do this manually,
don't commit a failing-mode script change.

## Step 3 — public-api: committed snapshot baseline, tool errors fail, derived crate list (finding 4) `[opus]`

**(a) What:** `verify.sh` (public_api_stage :194-234, PUBLIC_API_CRATES
:82-94), `verify.ps1` (Invoke-PublicApiStage :156-208, $publicApiCrates
:40-50), new committed snapshot dir `docs/reference/public-api-baseline/`,
CLAUDE.md verify-tier description.

**(b) Why now / order:** independent of Steps 1-2; before Step 4 so the edge
API changes in Step 4 (new `RunningServer::shutdown`) land AFTER the gate is
trustworthy (core/edge is not a contract crate, but the bless flow gets its
first real exercise if any api/events surface moves later).

**(c) How:**
- Derive the crate list in both scripts from the filesystem instead of a hand
  list: glob `api/*/api/Cargo.toml` + `api/*/events/Cargo.toml`, read each
  `name = "…"` line (bash: `sed -n 's/^name = "\(.*\)"/\1/p'`; ps1:
  `Select-String`). Today that yields all 14 (9 api + 5 events); a new domain
  joins the gate automatically. rpc crates stay out by construction.
- Replace the HEAD-worktree baseline with COMMITTED snapshots:
  `docs/reference/public-api-baseline/<crate>.txt`, generated by
  `cargo +nightly public-api -p <crate> -s --color=never`. The stage compares
  current output against the committed file; ANY diff (removed OR added
  lines) = FAIL with the message
  `"public-api: <crate> differs from committed baseline — review the diff;
  if intentional (additive or a versioned new contract), regenerate via
  ./verify.sh --bless-public-api (or -BlessPublicApi)"`. Add that bless flag
  to both scripts: regenerates all snapshots and exits (the operator commits
  them). Rationale: additive-only-vs-HEAD guarded nothing after commit; a
  committed snapshot makes the gate catch committed breaks, and bless is an
  explicit reviewed act. The additive-vs-breaking distinction moves to the
  human at bless time (the diff is printed) — mechanical additive-detection
  vs the snapshot stays available in the diff output (removed lines flagged
  as BREAKING in the stage output, added as ADDITIVE) so the operator sees
  which kind they're blessing.
- Tool failures fail the stage: bash drops `|| true` on both cargo public-api
  invocations and checks the exit code (nonzero → stage FAIL naming the
  crate); ps1 checks `$LASTEXITCODE` after each invocation. Missing snapshot
  file for a derived crate = FAIL (message: run bless), EXCEPT when the whole
  baseline dir is absent AND `--bless-public-api` is running for the first
  time. `git worktree` machinery is deleted (reviewer verified: used only by
  this stage, nothing else breaks).
- Toolchain pinning (reviewer item 9 — without it snapshots WILL diff
  spuriously across nightlies): pin `cargo-public-api` to an explicit version
  in both scripts' install lines (the cargo-audit precedent —
  `--version 0.22.2 --locked` at verify.sh:156; pick the currently-installed
  cargo-public-api version at implementation time and hardcode it the same
  way) AND record that version in a header comment inside each snapshot file
  at bless time. The FAIL message additionally says: `"if only formatting
  changed (toolchain drift), re-bless after confirming no symbol changes"`.
  rustdoc-JSON churn across the *nightly* itself is accepted residual risk on
  an advisory stage — noted in the stage's doc comment, not pinned (pinning a
  nightly date bit-rots; the tool pin removes the dominant churn source).
- Stage stays advisory-by-default / blocking-under-`--strict` (unchanged
  wiring). Seed the initial snapshots in this step (run bless once, commit
  the 14 files).
- Nightly-toolchain SKIP path stays as-is.

**(d) Verify:** `bash verify.sh --all` if bash runs the stage on this box,
else `pwsh -File .\verify.ps1 -All`; confirm public-api PASS with the seeded
snapshots; manually confirm a synthetic removal (temporarily delete a pub fn
in `leaderboardapi`, run stage, expect FAIL, revert) — manual check, not
committed.

## Step 4 — QUIC graceful shutdown: track in-flight work, drain before modules stop (finding 2) `[fable]`

**(a) What:** `core/edge/src/server.rs`, `core/edge/src/player.rs`,
`core/app/src/lib.rs` (`ordered_teardown` + its doc comments + the step-10
comment :226-230), `core/edge/src/lib.rs` (tests), new tests in core/edge's
test module.

**(b) Why now / order:** last of the code steps — the most design-heavy, and
it wants Steps 1-3's trustworthy nets underneath it (split-proof exercises
both planes' shutdown on every teardown).

**(c) How:**
- Add in-flight tracking to BOTH planes (same shape, mirroring Go's tracking
  with corrected ordering, and `Bus::close()`'s await-the-handles precedent):
  a shared `ShutdownState` struct in server.rs (used by player.rs too):
  `Arc<ShutdownState { closing: watch::Sender<bool>, in_flight: AtomicUsize,
  idle_notify: Notify }>`. Primitive choices are MANDATED (reviewer item 13):
  `closing` is a `tokio::sync::watch` channel, NOT `Notify` —
  `notify_waiters()` stores no permit, so a loop not currently parked misses
  the signal; watch receivers see the flipped value whenever they poll. The
  idle wait uses the re-check-after-subscribe loop:
  `loop { let notified = idle_notify.notified(); if in_flight == 0 { break }
  notified.await; }` — registering BEFORE checking closes the
  decrement-between-check-and-await race.
- Guard placement (reviewer item 12): the RAII in-flight guard is created in
  the ACCEPT arm (in `serve_conn` where `accept_bi()` yields, and in the
  accept loop for the connection-handshake task) and MOVED into the spawned
  task — creating it inside `serve_stream` leaves a window where an
  accepted-but-unstarted stream is invisible to the drain and gets aborted
  by `endpoint.close()`.
- Streams, not just connections (reviewer item 11 — the critical one):
  internal clients hold ONE persistent connection with stream-per-call, so
  stopping the endpoint-accept loop alone never drains — the live peer keeps
  opening new streams and `in_flight` never reaches 0. BOTH `serve_conn`
  loops (server.rs:185-196, player.rs:148-159) must also `tokio::select!`
  between `accept_bi()` and the `closing` watch: on closing, stop accepting
  NEW streams (break the loop; in-flight stream tasks hold their own guards
  and finish), letting the per-connection loop end without aborting the
  connection.
- `RunningServer` gains an `Arc<ShutdownState>` field (reviewer item 14) and
  `pub async fn shutdown(&self, grace: Duration)`:
  (1) send `closing = true` (watch) — endpoint-accept AND every per-conn
  stream-accept loop stop admitting;
  (2) await in-flight == 0 via the subscribe-then-check loop, wrapped in
  `tokio::time::timeout(grace, …)` — in-flight==0 must short-circuit
  immediately (idle teardown pays ~0); on timeout, warn naming the count;
  (3) `endpoint.close(0, b"server shutting down")` (aborts stragglers) then
  `endpoint.wait_idle().await` in a short fixed timeout (`min(grace, 3s)`)
  so a dead peer can't hang teardown.
  Keep the existing sync `close()` (12 test call sites; it remains the
  hard-abort path and deliberately bypasses tracking) — `shutdown` is the
  graceful superset. quinn `Endpoint::accept()` is cancel-safe in select!
  (reviewer verified: incoming queue lives in the endpoint).
- `core/app/src/lib.rs ordered_teardown`: replace both `close()` calls with
  `shutdown(grace).await`, player first then internal edge (real drain order
  now matches the previously-aspirational comment — rewrite the :226-230 and
  teardown doc comments to describe the actual semantics). Grace from
  `EDGE_DRAIN_GRACE_MS` env read in `run()`'s config (core/app, NOT in
  modules — topology/process knob), default 5000ms. The teardown-order
  invariant from the Step-3 hardening rollout is preserved: listeners
  (drained) → plane stop → invalidation stop → bus close → app stop.
- Tests (core/edge/src/lib.rs style, live QUIC over localhost):
  (i) `shutdown_waits_for_inflight_handler` — register a handler that sleeps
  200ms then returns a marker; fire a request, call `shutdown(2s)` mid-flight
  (spawn the call after a 50ms delay), assert the client still receives the
  full response (drain waited) and shutdown returns after the handler
  finishes;
  (ii) `shutdown_stops_accepting_new_connections` — after `shutdown` begins,
  a NEW client connect attempt fails;
  (iii) `shutdown_grace_timeout_aborts_stragglers` — handler sleeps 5s,
  `shutdown(200ms)` returns in ~200ms + wait_idle bound, not 5s.
  Same trio need not be duplicated for the player plane — one smoke test that
  `PlayerServer`'s `RunningServer::shutdown` drains a single in-flight player
  call suffices (the tracking struct is shared).
- The failing-startup unwind path (Step-3 rollout's `ordered_teardown` on
  Err) gets the same `shutdown` — no special-casing; grace applies there too
  (a failed startup with an in-flight request is already an edge case; the
  bounded grace keeps unwind prompt).

**(d) Verify:** `cargo test --workspace` (ONE invocation), clippy, archcheck,
`pwsh -File .\split-proof.ps1`. Honest coverage note (reviewer item 16):
split-proof exercises the IDLE teardown path only (processes are killed with
no in-flight QUIC work) — the busy-drain path is covered exclusively by the
in-process test (i). Watch split-proof wall time: idle teardown must add ~0
(in-flight==0 short-circuits); a noticeable regression means the idle
short-circuit is broken.

## Step 5 — Docs + memory + final verify `[inline]`

**(a) What:** CLAUDE.md (asyncevents contract note: delivery order now
provably numeric-XID; split-proof description: psql mandatory; verify tiers:
public-api snapshot+bless), the plan doc committed, memory update if any
standing memory contradicts (check `durable-event-plane-bus-owned` — ordering
claim unaffected, no edit expected), `scripts/memory-sync.ps1 push` only if
memory changed. Full `pwsh -File .\verify.ps1 -All` as the closing gate +
trailer audit (`git log --format="%h %B" | grep Co-Authored`).

**(b-d):** inline, last, verified by the verify run itself.

---

## Dispatch summary

| Step | Lane | Model arg | Effort in prompt | Trailer |
|------|------|-----------|------------------|---------|
| 1 XID ordering fix | `[opus]` | `model:"opus"` | think | Claude Opus 4.8 |
| 2 split-proof DSN/psql | `[sonnet]` | `model:"sonnet"` | default | Claude Sonnet 4.6 |
| 3 public-api snapshots | `[opus]` | `model:"opus"` | think | Claude Opus 4.8 |
| 4 QUIC drain | `[fable]` | `model:"fable"` | think hard | Claude Fable 5 |
| 5 docs + final verify | `[inline]` | — | — | Claude Fable 5 |

Each subagent commits its own step (Conventional Commits, its model's
trailer); main context reviews each diff against this plan before dispatching
the next. Commits land directly on master.

## Review

Grumpy-reviewer pass (Fable, think hard, separate context) 2026-07-10:
verdict SHIP WITH FIXES — 16 items (6 MAJOR, 5 MINOR, 5 verified-OK), all
folded into the step bodies above. Highlights: synthetic-PK uniqueness +
generation=0 pinning for the regression fixtures; DSN parser replaced by
URI-native psql; cargo-public-api version pin (cargo-audit precedent);
drain design closed on three gaps (per-connection stream-accept gating,
guard-at-accept placement, watch-not-Notify + subscribe-then-check idle
wait). Reviewer verified: test A discriminates buggy/correct floors; the
worktree deletion breaks nothing; Step 1 changes nothing split-proof
asserts; no CLAUDE.md constraint violations.
