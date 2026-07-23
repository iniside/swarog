# Rollout review remediation (describe-routing + replicas rollout, 5b8358f..2a44cc6)

**Date:** 2026-07-22 15:51
**Author:** Opus 4.8 (session)
**Status:** plan — grumpy-reviewer pass (opus/ultrathink) done + addressed; pending user approval

## Review disposition (grumpy pass, opus/ultrathink)

Verdict was needs-rework on old Steps 7 & 9. Addressed:
- **Finding 1/3 (Step 9→8):** mechanism respecified — typed marker at the inner
  request-decode site (`:666`) ONLY; downcast-catch-all and text-sniff explicitly
  BANNED; response-encode (`:670`) stays 5xx. Test matrix (Step 9) now pins the
  encode-503 case that a wrong impl silently breaks.
- **Finding 2 (old Step 7):** bounced to user → **cap dropped**; Step 7 is now
  test-only (documents the composed budget, keeps B's self-heal).
- **Finding 4:** Step 4 consts given values + invariant; redundant `PASS_BUDGET` dropped.
- **Finding 5:** Step 1 scoped to migrate ~14 test call sites + fix the false doc comment.
- **Finding 6:** Step 3 grace constraint `< MODULE_STOP_GRACE_MS` stated (reuse 2s).
- **Finding 7:** Step 6 #2 test strengthened to assert `JoinHandle` completion, not a
  stable pass count.
- Clean verdicts (Steps 5, 8-composition, 10-docs, #5/#6 dispositions) unchanged.

## Context

An external review of the describe-routing + replicas rollout raised 8 points.
Four `proof-auditor` subagents verified each against actual code (file:line). The
verification substantially reclassified the review:

- **#1 retry budget** — PARTIAL, **not a bug** (mutations are cut off before the
  failover path by the `retry_mode` gate at `core/remote/src/lib.rs:1082`; only
  idempotent `#[retry_safe]` reads reach the compounding path, where N executions
  are safe). Real finding: a **test gap** (no test composes a real `Reconnecting`
  under a `Pool`; all Pool tests use fakes that ignore `retry_mode`).
- **#2 lifecycle leak** — TRUE, bug. `DescribeRouter::spawn` detaches a `tokio::spawn`
  loop, keeps no handle/stop sender; `Gateway` has no `Module::stop`. Violates
  lifecycle constraint 8.
- **#3 blocking sequential first-pass** — TRUE, already an in-code KNOWN GAP
  (`modules/gateway/src/lib.rs:1138-1142`).
- **#4 missing provider-prefix validation** — TRUE. `build_describe_table` never
  checks `provider_of(m.method) == provider`. Impact bounded to a spurious 503/404
  route entry (not an auth bypass), but the fail-closed invariant is genuinely absent.
- **#5 weles replicas end-to-end** — TRUE but **explicitly M2-scope**
  (`docs/reference/weles-design.md:782,835` — "Not in M1: replicas"). The review's
  "B3/C4" test labels are invented (do not exist in the repo). **Decision: defer to
  M2; add a known-gap note only.**
- **#6 dynamic describe ≠ dynamic instance set** — TRUE, doc/naming honesty only.
  Extend the existing C2 "boot snapshot, live re-resolve out of scope" callout
  (`modules/gateway/src/lib.rs:969-976`) to the D2 `DescribeRouter`/
  `production_describe_fetcher` path, which lacks it.
- **#7 typeless 400→5xx** — TRUE, already pinned by splitproof `[D4-ILLTYPED]`
  (`tools/splitproof/src/main.rs:1236-1260`). Fix is **not small** and **reverses a
  documented design stance** (`core/opsapi/src/databind.rs` caveat iv). **User
  decision: plan the full fix** with recorded errata.
- **#8 OAuth `take_state`** — TRUE, bug. `epic_oauth.rs:151-157` folds a genuine
  `sqlx::Error` into the same `None` as a WHERE-miss; caller (`:350-352`) maps every
  `None` to 400. `new_state` in the same file (`:309-319`) already distinguishes
  persistence failure → 503.

### Why not extend / depend on X (Open/Closed check)

Every fix here is a **correction inside an existing seam owner**, not a new module:

- #1 authority = `Pool::call` (`core/remote`), the only layer seeing both instances.
- #2/#3/#4/#6 authority = the `gateway` module (`DescribeRouter`, `Gateway::Module`,
  `build_describe_table`). The reference stop/join pattern already exists in
  `remote::Stub` (`core/remote/src/lib.rs:1563-1601`) — copy the discipline, add no
  new abstraction.
- #7 authority = the edge transport error taxonomy (`core/edge` + the `rpc-macro`
  generated `gen_server_adapter`) — a new typed class, threaded through the existing
  `Response.code`/`From<edge::Error>` path.
- #8 authority = `EpicOAuth::take_state`'s **return type**.

No new module, event, capability, or admin section. No overlapping-system rationale
needed beyond the above.

## User decisions (locked)

- **#1:** add the missing composition test ONLY. **No cap** — the grumpy review
  (Finding 2) showed capping via `RetryMode::Never` on the failover call disables
  instance B's reconnect self-heal (a stale-connection B would fail the whole op
  instead of redialing → 200, per the B1 recovery finding), fixing a documented
  non-bug at the cost of a real recovery regression. Test documents the current
  (safe-for-idempotent) behaviour; code unchanged.
- **#7:** full fix + recorded errata in `databind.rs`. The mechanism is a **typed
  marker applied at the inner request-decode site only** — never a serde-error
  downcast (would also catch the response-encode failure) nor a text sniff (Finding
  1/3).
- **#5:** defer to M2 — known-gap doc note only, no live test now.

---

## Dispatch legend

`[core-implementer]` = `subagent_type: "core-implementer"`, `model:"opus"` (session
tier; core/cross-seam + correctness-critical). `[test-author]` = `test-author` agent
at the noted model. `[sonnet]` = mechanical/doc lane. Every code step: paste the
"cargo fmt is NOT safe here — hand-format to house style" note + the research-mode
nav guidance into the subagent prompt. **One rollout at a time** — tests run
sequentially, never two Cargo runs at once.

---

## Step 1 — OAuth `take_state`: surface DB outage as 503 (#8)  `[core-implementer]`

**(a) What:** `modules/accounts/src/epic_oauth.rs`.
- Change `take_state(&self, s: &str, browser_binding: Option<&str>) -> Option<String>`
  (`:134`) to `-> Result<Option<String>, sqlx::Error>`. The `DELETE ... RETURNING`
  match (`:151-157`): `Ok(row) => Ok(row)`, `Err(err) => { tracing::error!(...); Err(err) }`.
  Keep the WHERE-miss → `Ok(None)` semantics (unknown/expired/wrong-binding/redeemed
  stay a legitimate 400).
- Caller `handle_callback` (`:350-352`): `match oauth.take_state(&state, browser_binding).await`
  — `Ok(Some(t)) => t`, `Ok(None) => 400 "invalid or expired state"`,
  `Err(_) => 503 SERVICE_UNAVAILABLE` (mirror the `new_state` caller precedent at
  `:309-319`, cite it in the code comment).

**(b) Why now / order:** fully isolated (accounts module only), no dependency on any
other step — lands first as a clean, independently-reviewable win.

**(c-oauth) How:** the only non-mechanical move is preserving the 400-vs-503 split: a
WHERE-clause miss (`Ok(None)`) must remain 400; only an actual query error becomes
503. Do NOT change the show-once sibling `admin::take_reveal`
(`modules/admin/src/lib.rs:829-853`) — its flatten-to-`None` is deliberately correct
(false-negative safer than double-serving a non-re-derivable secret). Record that
non-change in the commit body.

**Ripple (Finding 5 — in-scope for THIS step, not discovered at compile time):** the
signature change breaks ~14 existing test call sites that `assert_eq!(oauth.take_state(...).await, None/Some(...))`
(`modules/accounts/src/tests.rs:1271,1272,1274,1280,1314,1320,1325,1348,1491,1492,1598,1632`).
Migrate them all in this rollout (the new tests come in Step 2; these are the
mechanical `.unwrap()`/`Ok(...)` adaptations of existing ones). Also fix the now-false
doc comment at `epic_oauth.rs:132` ("A store error fails closed to `None`") — the
change reverses it (prose-is-not-evidence: correct the lying comment same rollout).

**(d) Dispatch:** `[core-implementer]` `model:"opus"`, effort medium.

## Step 2 — Test #8: DB-error branch returns 503, WHERE-miss still 400  `[test-author]`

**(a) What:** `modules/accounts/src/tests.rs` (or `epic_oauth_tests.rs` per repo
convention). New test exercising the previously-uncovered `Err` arm
(`epic_oauth.rs:153-156`).

**(b) Why now / order:** after Step 1 lands and compiles.

**(c) How:** induce a genuine query failure (e.g. close/drop the pool, or point at a
dropped table) and assert `handle_callback` returns 503 — NOT 400. Add/confirm a
companion asserting an unknown state still returns 400 (the branch must discriminate,
not just "return 503 always"). The at-risk topology is monolith+split equally (same
handler); a unit test on the accounts store suffices.

**(d) Dispatch:** `[test-author]` `model:"sonnet"` (follows existing `tests.rs:1258-1329`
pattern).

## Step 3 — DescribeRouter lifecycle: stop + join the refresh loop (#2)  `[core-implementer]`

**(a) What:** `modules/gateway/src/lib.rs`.
- Add to `Gateway` (`:137-179`): `stop_tx: std::sync::Mutex<Option<watch::Sender<bool>>>`
  and `task: std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>`.
- `DescribeRouter::spawn` (`:1195-1211`): take a `watch::Receiver<bool>`; inside the
  loop `tokio::select!` on `ticker.tick()` vs `stop_rx.changed()` → break. Return the
  `JoinHandle` instead of `()`.
- `Gateway::start` (`:344-346`): create `watch::channel(false)`, store the sender +
  the returned `JoinHandle` in the new fields.
- Add `impl Module for Gateway { fn stop() }` (`:269-354` currently has none): send
  `true`, then `tokio::time::timeout(GRACE, &mut task)`, on elapse `task.abort();
  let _ = task.await;`. Mirror `remote::Stub::stop` (`core/remote/src/lib.rs:1563-1601`)
  exactly.
- **Grace constraint (Finding 6):** reuse `PROBE_STOP_GRACE = 2s`
  (`core/remote/src/lib.rs:510`). It MUST be `< MODULE_STOP_GRACE_MS` (default 5000ms,
  `core/app/src/lib.rs:31,357`) or the app aborts the whole `stop` future before the
  inner join completes. State this constant relationship in a code comment.
- **Mid-pass stop (Finding 6):** the `select!` observes `stop_rx` only *between*
  passes (at `ticker.tick()`), so a stop fired mid-`refresh_once` waits out the
  in-flight pass (bounded by Step 4's `DESCRIBE_PEER_TIMEOUT`) or the 2s abort. This
  is why Step 4's per-peer timeout must stay well under 2s — so shutdown doesn't
  routinely force-abort. Note it where the const is defined.

**(b) Why now / order:** before Step 4/5 because all three touch the same
`DescribeRouter`/`build_describe_table` region of `modules/gateway/src/lib.rs`; doing
them in one sequential lane avoids edit conflicts. Lifecycle ownership is the
structural prerequisite for bounding the first pass (Step 4).

**(c) How:** the loop currently owns `self` by value (`fn spawn(mut self)`) — keep
that, just add the `stop_rx` param and the `select!`. The `Arc<FrontDoor>` + per-provider
`Pool`s drop when the task ends, closing the leak.

**(d) Dispatch:** `[core-implementer]` `model:"opus"`, effort high (lifecycle seam).

## Step 4 — Bound the first describe pass: per-peer timeout + bounded concurrency (#3)  `[core-implementer]`

**(a) What:** `modules/gateway/src/lib.rs` `DescribeRouter::refresh_once` (`:1143-1158`).
- Replace the serial `for p in &self.peers { (self.fetch)(...).await }` with bounded
  concurrency (`FuturesUnordered` + a semaphore, or `futures::stream::iter(...).buffer_unordered(N)`),
  each per-peer fetch wrapped in `tokio::time::timeout(PER_PEER, ...)`.
- Add an overall first-pass deadline. A peer that times out is treated as absent for
  this pass (keep-last: retain its prior manifest, do not drop the route table).
- Introduce named consts near `DESCRIBE_REFRESH_INTERVAL` (`:127`) **with concrete
  values and a stated invariant (Finding 4):**
  - `DESCRIBE_PEER_TIMEOUT = 1500ms` — well under the 2s stop-abort (Finding 6) and
    under the server-side `EDGE_STREAM_GRACE` 30s, so a half-alive peer is dropped
    fast, not waited out.
  - `DESCRIBE_FETCH_CONCURRENCY = 8` — bounded fan-out.
  - **Drop `DESCRIBE_PASS_BUDGET` as redundant**: per-peer timeout + bounded
    concurrency already bound the whole pass to
    `ceil(N_peers / CONCURRENCY) * PEER_TIMEOUT` (~2 waves × 1.5s ≈ 3s worst case for
    11 peers). A separate overall deadline only risks the invariant
    `PASS_BUDGET >= wave_count * PEER_TIMEOUT` being violated and dropping
    legitimately-slow-but-reachable peers from the boot table. If a pass budget is
    kept anyway, it MUST satisfy that inequality; state it in a comment.
- Delete the KNOWN-GAP comment (`:1138-1142`) — it is now closed; replace with a
  one-line note of the bound.

**(b) Why now / order:** after Step 3 — same file/region; the lifecycle ownership from
Step 3 is in place so the bounded pass composes with a stoppable loop.

**(c) How:** the client-side describe call (`core/remote/src/lib.rs:82-93`) has no app
timeout today; the bound must live in `refresh_once` (the pass owner), not in
`remote`. Keep-last semantics already exist for a failed fetch — extend the same
branch to cover a timeout. Do not let a build collision be swallowed: a genuine
`build_describe_table` error still fails the pass (Step 5's invariant).

**(d) Dispatch:** `[core-implementer]` `model:"opus"`, effort high (async concurrency +
boot-path correctness).

## Step 5 — Fail-closed provider-prefix validation in build_describe_table (#4)  `[core-implementer]`

**(a) What:** `modules/gateway/src/lib.rs` `build_describe_table` (`:1077-1094`).
- Before pushing `opsapi::databind::operation(m)`/`binding(m)` (`:1084-1086`), check
  `provider_of(&m.method) == provider` (the fetching-peer key). On mismatch: `bail!`
  matching the existing collision convention (`:781-790`, `:839-847`) — a misbehaving
  peer advertising a foreign prefix is a loud boot/refresh failure, not a silent
  route entry.

**(b) Why now / order:** after Step 4 — same region; last of the three gateway edits.

**(c) How:** `provider_of` already exists (`:1235-1240`, split on first `.`). Reuse it.
The mismatch is fail-closed (`bail!`) consistent with the two existing overlap guards,
not a silent drop — so a codegen/describe bug surfaces loudly.

**(d) Dispatch:** `[core-implementer]` `model:"opus"`, effort medium.

## Step 6 — Tests #2/#3/#4: lifecycle halt, bounded pass, prefix rejection  `[test-author]`

**(a) What:** `modules/gateway/src/tests.rs`. Three tests, each hitting a
previously-uncovered branch:
- **#2 (strengthened per Finding 7):** "no further pass after stop" is satisfied
  *even by a still-leaked task* if the fake simply isn't ticked — it proves nothing
  about the join. The test MUST assert the `JoinHandle` actually **completes** after
  `stop()`: either `JoinHandle::is_finished()` after the stop returns, or a drop-flag
  on the moved `DescribeRouter` that flips only when the task ends. Construct the
  module, `start` with a fake fetcher, `stop`, and assert task completion — not a
  stable pass count. Today no test calls `spawn()` at all
  (`tests.rs:1849,1891,1954` drive `refresh_once` directly).
- **#3:** inject a fetcher that sleeps one peer past `DESCRIBE_PEER_TIMEOUT`; assert
  the pass completes within `DESCRIBE_PASS_BUDGET` and treats the slow peer as absent
  (keep-last) while other peers refresh. Use a paused tokio clock (timing-sensitive
  doctrine — no real-clock races).
- **#4:** feed `build_describe_table` a `fetched` map where provider `"inventory"`
  contributes an op named `"characters.sneaky"` (non-colliding verb/path); assert
  HEAD-before-fix accepts it (proving the gap) and post-fix `bail!`s. The test must
  **fail on unpatched Step 5 code**.

**(b) Why now / order:** after Steps 3–5 all land and compile.

**(c) How:** at-risk topology is the gateway process (`cmd/gateway-svc` + monolith);
these are module-level unit tests against the real `DescribeRouter`/`build_describe_table`.
Paused-clock + happens-before, no sleeps racing a wall clock.

**(d) Dispatch:** `[test-author]` `model:"opus"` (novel lifecycle + paused-clock
harness, not a copy of an existing pattern).

## Step 7 — Test #1: real Reconnecting under Pool, DOCUMENT the composed budget  `[test-author]`

**No production code change (user re-decision).** The cap was dropped — capping B to
`RetryMode::Never` would disable B's reconnect self-heal on the failover path
(Finding 2), regressing recovery to fix a documented non-bug. This step is the sole
real finding: the missing composition test.

**(a) What:** `core/remote/src/tests.rs`. New test composing a **retry-honoring** inner
caller (a `Reconnecting` over a fatal-once-then-heal fake dialer) under a `Pool` with
two instances — the composition no existing test builds (all Pool tests use
`FailoverCaller`/fakes that ignore `_retry_mode`, `tests.rs:1531-1553`,`:1086`).

**(b) Why now / order:** independent of the gateway/accounts steps. No production
dependency (code unchanged), so it can land any time after the baseline compiles.

**(c) How:** drive one `OnceAfterReconnect` op where instance A fails `ConnectionFatal`
on both its initial and its one replay, then assert instance B receives an initial
call AND, on a `ConnectionFatal` first touch, does its own single redial+replay — i.e.
pin the **documented, intended** composed behaviour (A: initial+replay; B: initial+replay),
proving B retains self-heal. Add a companion asserting a `Never` (mutating) op totals
exactly 1 execution and never reaches failover (the WHETHER-gate at `:1082`). The point
is to pin that the two-layer budget is composed-and-safe, not to assert a cap. Also
update the `Pool::call` docstring (`:1037-1040`) to state the composed worst case
(≤4 wire executions for an idempotent `OnceAfterReconnect` op, ≤1 for `Never`) as an
explicit, tested promise — named, not smuggled.

**(d) Dispatch:** `[test-author]` `model:"opus"` (novel composition harness). The
docstring line is a trivial co-edit in the same step (test-author may touch the one
comment it is pinning).

*(Former Step 8b sibling sweep folded away: with no cap there is no new retry-budget
seam to sweep for. The composition test above is the closure.)*

## Step 8 — Full #7 fix: typed inner-decode marker → 400 parity  `[core-implementer]`

**The load-bearing difficulty (Finding 1/3) — read first.** `gen_server_adapter` has
TWO `?`-propagations into the same type-erased `HandlerResult`
(`Box<dyn Error + Send + Sync>`, `core/edge/src/server.rs:60`):
`:666` request-decode (`serde_json::from_slice(&__payload)?` — CLIENT's fault → 400)
and `:670` response-encode (`serde_json::to_vec(&__resp)?` — SERVER's fault → MUST stay
5xx). Both collapse to `Box<dyn Error>` and both hit the single dispatch arm
`Err(e) => err_response(&e.to_string())` (`:512`, `code: None`), which carries no type
to tell them apart. Therefore:
- **BANNED:** downcasting the error to `serde_json::Error` in `dispatch` — it also
  matches `:670`, turning a genuine response-encode bug into a **400 to the front
  client**. BANNED: string-sniffing the "invalid json" text — that re-introduces the
  exact fragility the 2026-07-13 `UnknownMethod` remediation deleted
  (`core/edge/src/lib.rs:79-87` documents why text-classification was removed).
- **REQUIRED:** a typed marker applied **at `:666` ONLY**. The macro wraps the
  `from_slice` failure in a new public `edge` marker type (e.g.
  `edge::InvalidRequestBody(serde_json::Error)`); `:670` and every other handler error
  stay bare `Box<dyn Error>` → internal/5xx. This is the whole mechanism `gen_local`
  gets "for free" because it returns a typed `opsapi::Error` (`:844-849`,
  decode→`invalid`, encode→`internal`); the adapter must reconstruct that split
  through the marker since `HandlerResult` is type-erased.

**(a) What (a new edge error class threaded through 4 layers):**
- `tools/rpc-macro/src/lib.rs` `gen_server_adapter`: wrap the `:666` decode failure ONLY
  in the new `edge::InvalidRequestBody` marker (leave `:670` untouched).
- `core/edge/src/server.rs`: `dispatch` (`:510-513`) downcasts the handler error to the
  marker; on match sets a NEW `Response.code` variant (`:645-661`, today only
  `UnknownMethod`), e.g. `ResponseCode::InvalidRequest`; every non-marker error keeps
  `code: None` → 5xx.
- `core/edge/src/client.rs` `call_raw_id` match (`:126-138`): map the new code to a
  distinct `edge::Error` variant.
- `core/edge/src/lib.rs` `impl From<edge::Error> for opsapi::Error` (`:105-111`): map
  the new variant to `opsapi::Error::invalid` (→ 400), alongside the existing
  `UnknownMethod → NotFound` special-case (confirm the two codes don't collide — they
  are distinct variants). Every OTHER edge error stays `unavailable`/503.
- **Errata (mandatory, same rollout):** rewrite `core/opsapi/src/databind.rs` caveat
  iv (`:40-52`) to record the 400/5xx parity gap is now CLOSED for ill-typed bodies
  via the edge `InvalidRequest` class, reversing the prior "not fixable without field
  types in the manifest / splitproof should pin it" stance. Name the reversal in the
  commit body. Also fix the imprecise prose that calls the current failure
  `Internal`/500 — the audit traced it to `Unavailable`/503
  (`server.rs:510-513` `code:None` → `client.rs:126-138` `Error::Remote` →
  `lib.rs:105-111` → `opsapi::Error::unavailable`).

**(b) Why now / order:** last impl step — cross-seam (`core/edge` + `rpc-macro` +
`opsapi`) and riskiest; isolating it after the gateway/remote/accounts fixes keeps its
review boundary clean.

**(c) How — boundaries the fix must NOT cross:** the marker applies only to the inner
per-method typed-payload decode (`:666`). The OUTER envelope decode
(`server.rs:485-488`, parsing `Request` itself) stays untouched — genuine wire/framing
corruption remains 503, never 400. The `:670` response-encode stays internal/5xx.
`ResponseCode` is an internal `edge` enum (`core/`, not an `api/*` snapshot), so it is
NOT in the public-api baseline — the `--bless-public-api` hedge below is a
belt-and-braces only if clippy/public-api flags an `api/` re-export, which it should
not.

**(d) Dispatch:** `[core-implementer]` `model:"opus"`, effort high (cross-seam edge
taxonomy). If public-api unexpectedly flags a surface change, re-bless intentionally
with `--bless-public-api` (recorded).

## Step 9 — Tests #7: flip D4-ILLTYPED to ==400 + pin the inner/outer/encode boundary  `[test-author]`

**(a) What:**
- `tools/splitproof/src/main.rs` `[D4-ILLTYPED]` (`:1236-1260`): change the assertion
  from `d4_ill >= 500 && d4_ill != 400` to `d4_ill == 400` (`Status::Invalid.http() == 400`,
  confirmed `opsapi/lib.rs:178`) — the ill-typed body now returns 400 through the real
  `gateway-svc`→`match-svc` split (the at-risk topology).
- A `core/edge` (or generated-adapter) unit test matrix proving ALL THREE boundary
  cases (Finding 3 — the encode case is the one that silently breaks on a wrong Step-8
  impl):
  1. inner payload-decode failure (`:666`) → `InvalidRequest` → `opsapi::Error::invalid`/400;
  2. **response-encode failure (`:670`) / a handler returning an internal error →
     `unavailable`/503, NOT 400** (proves the marker didn't over-catch);
  3. outer-envelope corruption (`server.rs:485`) → `unavailable`/503 (untouched).

**(b) Why now / order:** after Step 8 lands. Runs under the split-proof stage (one
rollout at a time).

**(c) How:** D4-ILLTYPED is the previously-wrong branch on the real split — flipping it
proves the parity gap closed. Case (2) is non-negotiable: without it the fix's own new
seam (400-vs-503 discrimination at a type-erased boundary) ships unproven. Note the
player-QUIC ill-typed path shares the same `gen_server_adapter` and inherits the fix;
add a one-line assertion or an explicit note that it is covered by the same class.

**(d) Dispatch:** `[test-author]` `model:"opus"` (splitproof assertion + edge boundary
matrix).

## Step 10 — Doc honesty: extend C2 callout to D2 (#6) + weles replicas known-gap (#5)  `[sonnet]`

**(a) What (doc/comment only):**
- `modules/gateway/src/lib.rs`: add the existing C2 "boot snapshot; live route-table
  re-resolve out of scope" callout (mirroring `:969-976`) to the D2 `DescribeRouter`
  (`:60-72,121-126`) and `production_describe_fetcher` (`:1046-1069`) — the instance
  ADDRESS SET is a boot snapshot; only manifests refresh. So "dynamic table" is not
  misread as instance-set liveness.
- `docs/reference/weles-design.md` (near `:782,835`): add an explicit known-gap note
  that no live single-run test yet exercises `replicas=2 → mint → weles up → /resolve
  returns 2 addresses` (splitproof's second instance is hand-built, independent of
  weles); this is deferred M2-scope, not a regression.

**(b) Why now / order:** last — pure documentation, no code dependency.

**(c) How:** copy the wording of the existing `:969-976` callout; do not change any
behavior. `weles-design.md` note is additive.

**(d) Dispatch:** `[sonnet]` `model:"sonnet"` — mechanical doc edit.

---

## Verification after landing

- `cargo run -p verifyctl -- --fast` (build, clippy -D, test, fortress, routecheck,
  split-proof) — the D4-ILLTYPED flip (Step 9) runs under split-proof here.
- If Step 8 unexpectedly alters a contract crate's public surface: `cargo run -p
  verifyctl -- --bless-public-api` (recorded). Not expected — `ResponseCode` is
  internal `core/edge`.
- Trailer audit after the multi-subagent rollout:
  `git log -12 --format="%h %B" | grep "Co-Authored"` — every core-implementer commit
  → Opus 4.8, test-author steps at their step's `model:` (Step 2 → Sonnet 4.6;
  Steps 6/7/9 → Opus 4.8), Step 10 sonnet → Sonnet 4.6.

## Deliberately NOT touched (review's own "leave alone" + audit)

rollback/generation integrity, weles-master boundary, redb (transitional), Pool
reconcile, keep-last route refresh, routecheck/archcheck rebuilt for describe,
best-effort port minting, `admin::take_reveal` flatten (deliberately correct),
replica_safe operator flag, placement "local".
