# Review-findings fix plan — 2026-07-10

Fixes for the 19 verified review findings (verdict record:
`docs/summaries/2026-07-10-1847-review-findings-verification-summary.md`).
Research inputs: four read-only subagent reports (event-plane surfaces,
shutdown/infra surfaces, gateway/accounts/contrib surfaces, checker surfaces) —
all file:line references below come from those reports against the current tree.

## Context — why these shapes, not others

- **F1 (inventory ordering): local tombstone, NOT a capability call in the
  handler.** Verified: no durable handler in the repo calls a registry
  capability inside `on_tx` (all handlers only run SQL on the handed delivery
  tx), and `asyncevents/README.md:52-56` ties exactly-once to "effect commits in
  the handed delivery transaction". Calling `charactersapi::Ownership::owner_of`
  inside the grant handler would be a first-of-its-kind sync QUIC round-trip
  while holding the subscription row lock, and its result would not be atomic
  with the grant. A tombstone row written by the wipe handler in the SAME
  delivery tx is fully local, atomic, and safe because character ids are UUIDs
  (never reused). Why not extend `characters`: the fortress rule forbids a
  cross-module FK, and the capability-call option is rejected above — the
  consumer-side guard is the only shape that stays inside inventory's own tx.
- **F2/F9: reuse the worker's own locking discipline**, not new machinery — the
  worker already serializes on `FOR UPDATE SKIP LOCKED`; eventctl just never
  takes that lock, and the timeout arm writes after releasing it. Fix = take the
  lock (eventctl) and CAS on the cursor (timeout arm).
- **F10: delete the cache, don't patch it.** `transport.rs:65-68` documents
  `contracts_seeded` as "purely a per-process round-trip optimization";
  `ensure_history_contract` is idempotent (`ON CONFLICT DO NOTHING` + drift
  RAISE). No cache = no staleness class at all; cost is one extra statement per
  emit, acceptable for this project.
- **F11: copy the audit-prune pattern verbatim** (raw durable sub to
  `scheduler.fired`, schedule row seeded in scheduler's own DDL, contract const
  in `schedulerevents`) — audit has no `requires()` on scheduler and accounts-svc
  hosts no scheduler module, so the durable-event reaction is the only shape that
  works in split.
- **G5: manual doc rewrite, NOT an archcheck token ban** — banning
  `outbox`/`inbox` in `RETIRED_EVENT_TOKENS` would false-positive on
  `core/asyncevents` (README + the `DROP TABLE ... outbox/inbox` migration in
  `lib.rs:49-55`), which legitimately names the dead mechanism.

Constraints observed throughout: fortress rule (no module→module), foundations
never import modules/api, one test rollout at a time (each step's verification
is sequential), never-monolith-only (split-proof assertions extended where a
fix changes cross-process behavior), wipe-over-migrations (new DDL is
idempotent `CREATE ... IF NOT EXISTS`, no data migration).

Commit per step, Conventional Commits, trailer = executing model.

---

## Step 1 — Gateway proxy: stop following redirects (F3) `[opus]`

**(a) What:** `modules/gateway/src/proxy.rs:67`, `modules/gateway/src/proxy_tests.rs`
(new test + test-helper client at `:24`), `split-proof.sh` + `split-proof.ps1`
(new named assertion), `cmd` env docs comment if any.

**(b) Why now:** smallest HIGH fix, no dependencies; unblocks the split Epic
flow and establishes the split-proof pattern step 11 reuses.

**(c) How:**
- `ProxyTable::from_routes`: replace `reqwest::Client::new()` with
  `reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build().expect("proxy client")`.
  Same change in the `table()` test helper (`proxy_tests.rs:24`).
- New test in `proxy_tests.rs` mirroring `forward_proxies_verbatim_and_extends_xff`
  (`:117-139`): the `spawn_upstream` fake handler returns
  `(StatusCode::FOUND, [("location", "/#token=abc")])`; assert
  `forward(...)` yields status 302 and the `Location` header byte-identical
  (proving the client no longer follows).
- Split-proof assertion (both scripts), name e.g. `epic-oauth-redirect-through-gateway`:
  start accounts-svc with `EPIC_CLIENT_ID=test EPIC_CLIENT_SECRET=test
  EPIC_TOKEN_URL=http://127.0.0.1:1/token` (unreachable port — `exchange_code`
  fails deterministically); gateway-svc already gets `ACCOUNTS_HTTP_ADDR`.
  Route methods CONFIRMED against `epic_oauth.rs:155-165`:
  `POST /accounts/epic/start`, `GET /accounts/epic/callback`.
  `POST /accounts/epic/start` through the gateway → JSON `{authorize_url}`;
  extract `state` from the URL; `GET /accounts/epic/callback?code=x&state=<state>`
  through the gateway with redirect-following disabled in the client
  (`curl` without `-L`; PS `Invoke-WebRequest -MaximumRedirection 0
  -SkipHttpErrorCheck`); assert status 302 and `Location: /?epic=error`.
  (Verified: `/start` does no network I/O; the callback's exchange-failure path
  returns exactly `Redirect::to("/?epic=error")` — `epic_oauth.rs:211-216`.
  Setting the `EPIC_*` vars is side-effect-free for other assertions: the OIDC
  verifier and OAuth client do no network I/O at construction — JWKS is fetched
  lazily on first `verify`, which this assertion never triggers.)

**(d) Verification:** `cargo test -p gateway`, then `./split-proof.ps1` (one
rollout at a time). Commit `fix(gateway): proxy must not follow upstream
redirects — relay 302s verbatim (Epic OAuth token fragment)`.

## Step 2 — Gateway verifier: distinguish outage from invalid session (F6) `[opus]`

**(a) What:** `modules/gateway/src/verifier.rs` (trait + both impls),
`modules/gateway/src/lib.rs:403-412` (QUIC front) and `:801-812`
(`authenticate`), gateway tests.

**(b) Why now:** independent of step 1 but same crate — do it before the
event-plane steps so all gateway work lands together.

**(c) How:**
- Change the trait (`verifier.rs:27-30`) to
  `async fn verify(&self, token: &str) -> Result<Option<String>, VerifyUnavailable>;`
  with `pub struct VerifyUnavailable;` (unit struct — the only non-auth failure
  class). `DevSessionVerifier` returns `Ok(prefix-match)`. `SessionsVerifier`
  maps `Ok(pid)` → `Ok(pid.filter(..))`, `Err(err)` → keep the
  `tracing::error!` and return `Err(VerifyUnavailable)`.
- `authenticate` (`lib.rs:801-812`): `Err(VerifyUnavailable)` →
  `error_response(StatusCode::SERVICE_UNAVAILABLE, "session verification unavailable")`;
  `Ok(None)` → 401 as today.
- QUIC front (`lib.rs:403-412`): `Err(VerifyUnavailable)` →
  `front_envelope(Status::Unavailable, "session verification unavailable")`
  (variant exists — `core/opsapi/src/lib.rs:134-135`, documented "→ HTTP 503").
- Tests: extend existing verifier tests with a failing `Sessions` fake asserting
  503 on HTTP and `Status::Unavailable` on the envelope path; keep a bad-token
  case asserting 401 unchanged.

**(d) Verification:** `cargo test -p gateway`, clippy. Commit
`fix(gateway): map accounts outage to 503/Unavailable instead of 401`.

## Step 3 — eventctl skip: same lock discipline as the worker (F2) `[opus]`

**(a) What:** `tools/eventctl/src/lib.rs:204-286` (`skip`),
`tools/eventctl/src/lib_tests.rs`.

**(b) Why now:** first event-plane step; step 4 (worker CAS) touches the same
mental model and reviewer context.

**(c) How:**
- Rewrite `skip` to run in ONE transaction on one acquired connection:
  `BEGIN` → claim the row with the worker's exact shape
  (`SELECT topic, contract_version, cursor_generation, cursor_xid::text, cursor_tie,
  state, consecutive_failures FROM asyncevents.subscriptions WHERE
  subscription_id = $1 FOR UPDATE` — plain `FOR UPDATE`, NOT `SKIP LOCKED`: the
  operator should wait for an in-flight delivery to finish, not silently no-op)
  → re-evaluate the refuse-healthy guard on the LOCKED row (`state == "active"
  && consecutive_failures == 0` → `ROLLBACK` + bail, same message) → select the
  target event from the locked cursor (same frontier query, `fetch_optional` on
  the tx conn) → cursor UPDATE (unchanged SQL) → `COMMIT`. The `before`/`after`
  snapshots stay pool-side (cosmetic reads).
- Behavior note in the fn doc: skip serializes against live workers via the row
  lock; a worker that advanced past the failure first makes skip refuse
  (failures were reset to 0) — the correct outcome.
- New live-Postgres test in `lib_tests.rs`: seed a sub with failures>0, then in
  a parallel task hold `FOR UPDATE` on the row (simulating a worker mid-delivery)
  and advance the cursor + reset failures before releasing; assert `skip` blocks
  and then refuses (healthy) instead of rewinding. Reuse `insert_sub` (`:116`).

**(d) Verification:** `cargo test -p eventctl` (one rollout). Commit
`fix(eventctl): skip claims the subscription row FOR UPDATE — no cursor rewind
race with live workers`.

## Step 4 — Worker timeout arm: CAS the failure write on the cursor (F9) `[opus]`

**(a) What:** `core/asyncevents/src/worker.rs:199-221` (timeout arm),
`record_failure` (`:229-247`), `core/asyncevents/src/worker_tests.rs`.

**(b) Why now:** completes the event-plane locking pack started in step 3.

**(c) How:**
- Give `record_failure` a claim-state guard: add params
  `(cursor_generation: i64, cursor_xid_text: &str, cursor_tie: i64, claimed_failures: i32)`
  and extend the WHERE to
  `WHERE subscription_id = $1 AND cursor_generation = $6 AND cursor_xid = $7::xid8 AND cursor_tie = $8 AND consecutive_failures = $9`.
  The `consecutive_failures` leg is NOT redundant (reviewer item 2): the cursor
  does not move on failure, on `eventctl retry` (resets failures, cursor
  untouched), or on `resume` — cursor-only CAS would let a stale
  `failures + 1 = 20` write pause a subscription an operator just `retry`-reset
  or another replica just failed-once. Guarding both cursor AND the
  claim-time `consecutive_failures` kills that ABA.
  Return `rows_affected`; on 0 rows, `tracing::info!` "subscription state
  changed concurrently; stale failure not recorded" and treat as success.
- Error arm (`:186-197`) passes the cursor + failures it read under the
  still-held lock — the guard is trivially true there (lock held), so one
  signature serves both.
- Timeout arm: unchanged sequence (detach → terminate backend → fresh conn),
  but the `record_failure` call now carries the cursor read at claim time
  (`:100-112` already selects it) — a replica that delivered in the window makes
  the UPDATE match 0 rows.
- Test: extend `timeout_poisons_only_the_delivery_connection` (`:445`) or add a
  sibling: after the terminate-backend point, advance the cursor + zero failures
  out-of-band (simulating replica B), then let the timeout arm record — assert
  `consecutive_failures` stays 0 and `state` stays `active`.

**(d) Verification:** `cargo test -p asyncevents` (one rollout). Commit
`fix(asyncevents): timeout-arm failure write is CAS-guarded on the claimed
cursor — no stale backoff/pause on a healthy subscription`.

## Step 5 — Inventory: wipe tombstone kills late grants (F1) `[fable]`

**(a) What:** `modules/inventory/src/lib.rs` (`SCHEMA_DDL` `:43-64`,
`grant_starter` `:268-274`, `wipe_character` `:278-282`, `Store::grant_exec`
`:125-147`, `Store::clear_owner_exec` `:180-186`), `modules/inventory/src/tests.rs`,
`split-proof.sh`/`.ps1` (extend the existing wipe assertion).

**(b) Why now:** depends on nothing above, but it is the subtlest correctness
change (bus-seam semantics) — do it after the mechanical event-plane fixes so
the reviewer context from steps 3–4 is fresh.

**(c) How:**
- DDL: add
  `CREATE TABLE IF NOT EXISTS inventory.wiped_characters (character_id uuid PRIMARY KEY, wiped_at timestamptz NOT NULL DEFAULT now());`
  (idempotent, wipe-over-migrations compliant — a dev DB wipe also works).
- **Both handlers first take a per-character advisory lock inside the delivery
  tx** (reviewer item 1 — without it the tombstone check races a CONCURRENT
  wipe delivery under READ COMMITTED: grant SELECTs tombstone-absent while
  wipe's insert is uncommitted, wipe's DELETE can't see grant's uncommitted
  insert, both commit → orphaned row coexisting with a tombstone):
  `SELECT pg_advisory_xact_lock($1)` with a key derived from the character
  uuid via the FNV-style i64 hash discipline the scheduler already uses
  (`modules/scheduler` `lock_key`, tests at scheduler tests.rs:142-153 — copy
  the shape, namespace the seed so inventory's keys can't collide with
  scheduler's). The xact-lock releases at delivery-tx commit; two concurrent
  deliveries for the same character serialize, so tombstone-check → insert is
  atomic w.r.t. the sibling handler.
- `wipe_character`: after the lock, in the SAME delivery tx,
  `INSERT INTO inventory.wiped_characters (character_id) VALUES ($1::uuid) ON CONFLICT DO NOTHING`,
  then the existing `DELETE FROM inventory.holdings ...`.
- `grant_starter`: after the lock, check
  `SELECT 1 FROM inventory.wiped_characters WHERE character_id = $1::uuid`;
  if present, `tracing::info!` and return `Ok(())` (skip — the character is
  gone; the checkpoint still commits, exactly-once preserved). Soundness:
  UUIDs never recur, so a tombstone is permanent truth; sequential orders are
  covered by the tombstone, concurrent delivery by the advisory lock.
- Doc comment on the two handlers replacing the current "integrity without a
  cross-module FK" note (`:589-590`) with the tombstone rationale + the
  cross-subscription-ordering contract quote (README:55).
- Tests (`tests.rs`, real-plane harness of `grant_on_created_via_on_tx` `:227`):
  (1) deliver `deleted` before `created` for the same character id → assert NO
  holdings row and a tombstone row; (2) normal order still grants then wipes;
  (3) concurrency: two parallel txs on separate connections — one holding the
  grant path pre-commit, the other running the wipe path — assert the second
  blocks on the advisory lock until the first commits (mirrors the scheduler
  lock tests' shape), so the lock is actually exercised, not just present.
- Split-proof: the existing DB-verified wipe assertion gains a second query
  asserting `inventory.wiped_characters` has the row.

**(d) Verification:** `cargo test -p inventory`, then `./split-proof.ps1`
(one rollout). Commit `fix(inventory): wipe tombstone — deleted-before-created
reorder can no longer resurrect holdings`.

## Step 6 — Shutdown signals: SIGTERM + Windows console events (F4) `[opus]`

**(a) What:** `core/app/src/lib.rs:651-658` (`shutdown_signal`), no Cargo
changes (tokio 1.52 `signal` feature already on in `core/app/Cargo.toml`).

**(b) Why now:** prerequisite for step 7 (the drain timeout wraps the same
signal future) — signal coverage first, then bounding.

**(c) How:**
- Rewrite `shutdown_signal` with cfg-gated arms:
  - `#[cfg(unix)]`: `tokio::select!` over `ctrl_c()` and
    `tokio::signal::unix::signal(SignalKind::terminate()).recv()`.
  - `#[cfg(windows)]`: `tokio::select!` over `ctrl_c()`,
    `tokio::signal::windows::ctrl_close()`, and `ctrl_shutdown()` (streams,
    `.recv()`). Note in the doc comment: `Stop-Process -Force` /
    `taskkill /F` are `TerminateProcess` — no console event, unavoidable;
    graceful stop on Windows requires a non-forced console event (interactive
    Ctrl-C) — scripts stay hard-kill by design (documented limitation).
  - Keep the existing "handler install failure = shut down" logging shape.
- `run.sh`/`split-proof.sh` need no change to benefit (plain `kill` = SIGTERM);
  add to `split-proof.sh` `teardown()` a bounded wait-for-exit loop (up to ~10 s
  per PID: `kill`, then poll `kill -0`) so a draining service isn't racing the
  next run's port bind.

**(d) Verification:** `cargo test -p app`, clippy — AND (reviewer item 5:
`#[cfg(unix)]` code is compiled OUT on Windows, clippy never sees it)
cross-compile-check the unix arm:
`rustup target add x86_64-unknown-linux-gnu` (once), then
`cargo check -p app --target x86_64-unknown-linux-gnu`. If the target's
system deps make the full check impractical on this box, the fallback is a
`cargo check -p app` inside WSL — either way the unix arm MUST be
compile-verified before commit; runtime SIGTERM behavior remains
unverified-on-Windows and the commit message states that. Commit `fix(app):
shutdown_signal listens for SIGTERM (unix) and ctrl_close/ctrl_shutdown
(windows), not only Ctrl-C`.

## Step 7 — Bounded HTTP drain (F7) `[opus]`

**(a) What:** `core/app/src/lib.rs` (`Config` fields ~`:28,57-73,119-131`,
serve wiring `:521-533`), CLAUDE.md one-liner in the lifecycle constraint,
AGENTS.md mirror of the same note (repo practice — cf. commit `6910d6b
docs(agents-md): mirror CLAUDE.md drain + psql-mandatory notes`).

**(b) Why now:** builds directly on step 6's signal future.

**(c) How:**
- New `Config` field `http_drain_grace: Duration`, env `HTTP_DRAIN_GRACE_MS`,
  default 5000 — mirror the `EDGE_DRAIN_GRACE_MS` pattern exactly
  (`DEFAULT_..._MS` const, `from_env` var, `from_values` parse-with-default).
- Restructure the serve await: share the shutdown signal through a
  `tokio::sync::watch` — specifically watch, NOT `Notify` (reviewer item 7:
  two consumers wait on the signal and it can fire before serve starts;
  `Notify` stores at most one permit / loses `notify_waiters` with no waiter —
  only a level-triggered multi-receiver primitive is correct here) — so
  `run()` observes WHEN the signal fired:
  `let (sig_tx, sig_rx) = watch::channel(false);` spawn
  `shutdown_signal().await; sig_tx.send(true)`. Pass a future awaiting `sig_rx`
  to `.with_graceful_shutdown(...)`. Then
  `tokio::select! { r = serve_fut => r, _ = async { wait for sig_rx; tokio::time::sleep(http_drain_grace).await } => { tracing::warn!("http drain grace expired; abandoning in-flight connections"); Ok(()) } }`.
  Teardown (`:541-550`) proceeds either way. The timeout only starts AFTER the
  signal — normal serving is unaffected.
- Read the knob in `core/app` only (constraint 8 phrasing: like
  `EDGE_DRAIN_GRACE_MS`, "read in core/app, never in modules").

**(d) Verification:** `cargo test -p app` + `cargo test --workspace` sanity
(one rollout). Commit `feat(app): bounded HTTP drain — HTTP_DRAIN_GRACE_MS
(default 5000) caps with_graceful_shutdown before ordered teardown`.

## Step 8 — Advisory lock around module migrations (F8) `[opus]`

**(a) What:** `core/lifecycle/src/app.rs:66-74` (`App::migrate`), a new lock-key
const, lifecycle tests.

**(b) Why now:** independent; grouped with the app-plane steps 6–7 for reviewer
context.

**(c) How:**
- In `App::migrate`, when `self.ctx.db()` is `Some(pool)`: acquire ONE
  connection, `SELECT pg_advisory_lock($1)` with a new
  `const MODULE_MIGRATE_LOCK_KEY: i64 = 0x6C69_6665_6D69_6767;` (ASCII
  "lifemigg"; must differ from asyncevents' `0x6173_796E_636D_6967`), run the
  existing module loop, then `SELECT pg_advisory_unlock($1)` on the SAME
  connection inside a `finally`-shaped flow (unlock also on error — wrap the
  loop result, unlock, then propagate). Session lock (not `_xact`) because the
  loop spans many independent module transactions; the dedicated connection is
  held for the duration and dropped after unlock. DB-less process (`None`):
  run the loop unlocked as today (no DDL possible anyway).
  **Invariant (reviewer item 10):** the lock connection is held while each
  module's `migrate` acquires further pool connections — pool max MUST be ≥ 2
  during migrate or the process self-deadlocks. Defaults are fine; state the
  invariant in a comment at the acquire site.
- `core/lifecycle` already deps sqlx — no new edge; foundations rule untouched.
- Test: two concurrent `App::migrate` invocations against live Postgres with a
  probe module whose `migrate` records enter/exit timestamps — assert no
  overlap.

**(d) Verification:** `cargo test -p lifecycle` (one rollout), `cargo run -p
archcheck`. Commit `fix(lifecycle): serialize module migrations across replicas
with a pg advisory lock (mirrors asyncevents' migrate lock)`.

## Step 9 — split-proof hardening: /readyz + strict SQL helper (F13, F14) `[sonnet]`

**(a) What:** `split-proof.sh` (`pg()` `:171-173`, health poll `:233`, teardown),
`split-proof.ps1` (`Invoke-Sql` `:154-156`, `Wait-Healthy` `:161-169`),
`run.sh` (health loop `:69-72`), `run.ps1` (`Wait-Healthy` `:86-96`).

**(b) Why now:** after steps 1–8 so the strengthened proof gates the new
behavior; before step 11 (whose split-proof assertion relies on strict SQL).

**(c) How:**
- All four health polls switch `/healthz` → `/readyz` (same 200-check; on the
  final failed attempt, print the 503 JSON body for diagnosis).
- bash `pg()`:
  `out=$("$PSQL" "$DATABASE_URL" -v ON_ERROR_STOP=1 -t -A -c "$1" 2>&1)` then
  `rc=$?; if [ $rc -ne 0 ]; then echo "FATAL psql rc=$rc for: $1" >&2; echo "$out" >&2; kill -s TERM $$; fi; printf '%s\n' "$out"`.
  **Trap change required** (reviewer item 6): today's
  `trap teardown EXIT INT TERM` (`:195`) RESUMES the script after the trap —
  a FATAL-SQL run would keep asserting against a killed fleet and could still
  exit 0 (the exact false-pass class F14 fixes). Split the trap:
  `trap 'teardown; exit 1' INT TERM` + `trap teardown EXIT`, so the
  psql-death path terminates non-zero.
- PS `Invoke-Sql`: capture output, check `$LASTEXITCODE -ne 0` → write the SQL
  + stderr via `Write-Error` and `throw` (the script's existing failure path
  handles teardown).
- No assertion-content changes — pure plumbing.

**(d) Verification:** `./split-proof.ps1` full run (one rollout). Commit
`fix(split-proof,run): poll /readyz not /healthz; SQL helpers die on psql
failure (ON_ERROR_STOP + exit-code check)`.

## Step 10 — Drop the history-contract RAM cache (F10) `[sonnet]`

**(a) What:** `core/asyncevents/src/transport.rs` (`contracts_seeded` field
`:62-70`, `enqueue_tx` `:93-119`), transport/store tests.

**(b) Why now:** trivial once step 4 has touched asyncevents; independent
otherwise.

**(c) How:** delete the `contracts_seeded` field and the insert/remove logic;
`enqueue_tx` always calls `ensure_history_contract` (idempotent
`ON CONFLICT DO NOTHING` + drift RAISE) before `append`. Update the struct doc
(`:65-68`) — the optimization is removed deliberately; rollback staleness class
gone. Adjust any test asserting single-seeding behavior.

**(d) Verification:** `cargo test -p asyncevents` (one rollout). Commit
`fix(asyncevents): drop the pre-commit contracts_seeded cache — seed
idempotently on every emit`.

## Step 11 — Accounts session prune + expires_at index (F11) `[opus]`

**(a) What:** `modules/accounts/src/lib.rs` (DDL `:65-71`, `init`),
`modules/accounts/src/store.rs` (new prune fn), `api/scheduler/events/src/lib.rs`
(`schedule_names` `:39-41`), `modules/scheduler/src/lib.rs` (seed DDL `:64-82`
+ anti-drift test), accounts tests, split-proof assertion,
public-api baseline re-bless.

**(b) Why now:** last code step because it crosses two contract surfaces
(schedulerevents const = additive public-api change → needs
`-BlessPublicApi`) and reuses step 9's strict SQL helper in its assertion.

**(c) How:** copy the audit pattern verbatim:
- `schedulerevents::schedule_names::SESSIONS_PRUNE: &str = "accounts-sessions-prune"`;
  scheduler seed DDL gains
  `INSERT INTO scheduler.schedules (name, interval_seconds) VALUES ('accounts-sessions-prune', 86400) ON CONFLICT (name) DO NOTHING;`
  and the `seeded_schedule_names_are_contract` test gains the name.
- Accounts DDL adds
  `CREATE INDEX IF NOT EXISTS sessions_expires_idx ON accounts.sessions(expires_at);`
- `store.rs`: `pub async fn prune_expired_sessions(&self, conn: &mut PgConnection) -> Result<u64, sqlx::Error>`
  running `DELETE FROM accounts.sessions WHERE expires_at <= now()`.
- `init`: `PruneHandler` (TxHandler) parsing only `name` from raw
  `scheduler.fired` JSON, acting only on `accounts-sessions-prune`
  (mirror `modules/audit/src/lib.rs:63-71,159-181`), registered via
  `bus.on_tx_raw(SubscriptionSpec { id: "accounts.prune-on-scheduler.v1", start: StartPosition::Genesis }, schedulerevents::FIRED.topic(), handler)`.
  No `requires()` change (audit precedent: contract-crate dep only).
- topiccheck: new subscription id is globally unique; run the checker.
- Tests: prune deletes only expired rows (insert one expired via direct SQL,
  one live); handler no-ops on foreign schedule names but still commits.
- Split-proof: after the scheduler exactly-once assertion, insert an expired
  session row via `pg`, then force a natural fire the way the existing
  proof-tick does (`split-proof.sh:793-811` precedent):
  `UPDATE scheduler.schedules SET last_fired = to_timestamp(0) WHERE name = 'accounts-sessions-prune'`
  and poll (bounded) until the expired row is gone. Do NOT append a synthetic
  `scheduler.fired` via `asyncevents.append_event` (reviewer item 3: forging
  an event the scheduler solely produces violates publisher-owns-the-event and
  feeds audit's raw sink a fake row; also a reused dev DB has `last_fired`
  advanced, so only the reset makes the fire deterministic).
- Re-bless: `./verify.ps1 -BlessPublicApi` for the additive
  `schedulerevents` surface change (state ADDITIVE in the commit).

**(d) Verification:** `cargo test -p accounts -p scheduler`, `cargo run -p
topiccheck`, `./split-proof.ps1` (sequential, one rollout each). Commit
`feat(accounts,scheduler): durable session prune on scheduler.fired +
expires_at index`.

## Step 12 — Loud contrib downcast misses (F12) `[sonnet]`

**(a) What:** `core/contrib/src/lib.rs:43-48`, contrib tests.

**(b) Why now:** independent one-liner; late because it's pure hygiene.

**(c) How:** in `contributions`, replace the bare `filter_map` with a loop that
counts downcast misses; if `misses > 0`:
`tracing::error!(slot, expected = std::any::type_name::<T>(), misses, "contributions: type mismatch — values silently skipped")`
plus `debug_assert!(misses == 0, ...)` so tests/dev builds fail loudly while
release keeps the documented skip semantics. Update the doc comment (`:36-42`).
Add a test asserting the mismatch is detected (debug_assert panics under
`cfg(debug_assertions)` — use `catch_unwind` or gate the test).
`core/contrib` gains a `tracing` dep only if it doesn't already have one
(check; if adding is undesirable, `eprintln!`-free option is debug_assert +
returning the count via a `#[cfg(test)]` hook — prefer tracing, it's already a
workspace dep).
**Blast radius (reviewer item 11):** `contributions()` runs per-request on
`/readyz` (READINESS_SLOT read lazily), so under `debug_assertions` a
type-mismatch becomes a request-handler panic in every debug run including the
split-proof fleet. That loudness is the point — but before landing, grep every
`contribute(`/`contributions::<` call site and confirm no slot is legitimately
mixed-type today (research pass found none; the landing step re-verifies
exhaustively).

**(d) Verification:** `cargo test -p contrib` (one rollout). Commit
`fix(contrib): downcast misses in contributions() are logged + debug-asserted,
not silent`.

## Step 13 — topiccheck: (topic, version) keys (F5) + admin-svc planeless (G4) `[opus]`

**(a) What:** `tools/topiccheck/src/main.rs` (`:74` PLANELESS, `:177-178`,
`:196`, `:246-252`, `:255-261`, `:311`, `:320-353`), `core/bus` (additive
accessor), topiccheck self-tests if present.

**(b) Why now:** checker pack starts after all behavior fixes so the
strengthened checkers validate the final tree.

**(c) How:**
- Key changes: `by_topic: BTreeMap<(&str, u32), u32>` → really
  `BTreeMap<(&str, u32), &Contract>` keyed `(topic, version)`; the sub lookup
  uses `(s.topic.as_str(), s.version)` (`Sub` already carries `version` —
  `Recorded` records it at `:117-122`). The `:196` history cross-check switches
  to `find(|c| c.topic == s.topic && c.version == s.version)`. `subscribed` set
  (`:311`) becomes `BTreeSet<(String, u32)>`; check 5 (`:255-261`) filters on
  `(c.topic.clone(), c.version)`. The report table (`:320-353`) keys
  `(topic, version)` and prints `topic v{n}` per row.
- Check 4 (`:246-252`, in-process durability) — reviewer item 4 correction:
  `Bus::on` erases the version at registration (`lib.rs:159` delegates to
  `subscribe(et.topic(), …)`), and the same registry also receives plain
  string-keyed `Bus::subscribe(topic, …)` calls that carry NO version. So a
  single tuple-keyed check would REGRESS coverage for raw `subscribe` calls.
  Do both instead: add a PARALLEL record in `core/bus` —
  `subscribed_contracts() -> Vec<(String, u32)>` populated ONLY by `on()`
  (which holds the `EventType` and thus the version) — and keep the existing
  topic-level `subscribed_topics()` check for string-`subscribe`
  registrations. Check 4 runs the tuple-aware pass over `on()` registrations
  AND the today-shaped topic-level pass over the raw set; a finding from
  either fails. `subscribed_topics()` itself stays (other callers unaffected).
  Note: `bus` is a contract-adjacent core crate but NOT in the public-api
  baseline list (baselines cover `api/*` crates only — verify; if bus IS
  baselined, re-bless ADDITIVE).
- G4: `PLANELESS_PROCESSES = ["gateway-svc", "admin-svc"]` with a comment
  quoting `cmd/admin-svc/src/main.rs:47` (`without_db` — hosts no plane).
- Add a unit test in topiccheck: two synthetic contracts sharing a topic at v1
  and v2 with one sub each → zero findings; a v2 sub against a v1-only contract
  → drift finding.

**(d) Verification:** `cargo run -p topiccheck`, `cargo test -p topiccheck -p bus`
(one rollout). Commit `fix(topiccheck,bus): version-aware contract keys —
(topic, version) everywhere; admin-svc classified planeless`.

## Step 14 — archcheck: core-purity rule (G1) + registration tripwire (G2) `[opus]`

**(a) What:** `tools/archcheck/src/main.rs` (`classify` `:113-148`, new rule
beside `:182-204`, rule 12 `:370-417`), archcheck self-tests if present.

**(b) Why now:** after step 13 — checkers land as one pack; G1's new rule must
run against the post-fix tree (steps 2, 7, 8 touched core).

**(c) How:**
- G1: add `Kind::Core(String)` arm in `classify` (`segment_after(&p, "/core/")`,
  inserted before the `Kind::Other` fallback) and a new numbered rule iterating
  ALL packages: for `Kind::Core(_)` consumers, any non-dev dep resolving via
  `by_name` to `Kind::Module|Api|Events|Rpc|Demo` is a violation (hard
  constraint 1 verbatim). Expect zero violations today (the Cargo graph was
  verified clean); if the run surfaces one, STOP and surface it — do not
  whitelist.
- G2: extend rule 12's `boots_its_module` with a source tripwire in the style of
  `grep_option_edge_server` (`:755-789`): scan `cmd/<name>-svc/src/lib.rs` for
  the boundary-checked token `<module>::` (the module crate ident followed by
  `::`, e.g. `characters::`) — presence anywhere in lib.rs proves the svc
  constructs its module (`Box::new(characters::Characters::new())` per
  `cmd/characters-svc/src/lib.rs:14`). Missing token = new violation "svc
  depends on its module but never constructs it in modules()". Document it as a
  heuristic tripwire (same caveat class as `is_inline_test_mod` `:684-703`).
- Renumber/announce the new rules in archcheck's header comment.

**(d) Verification:** `cargo run -p archcheck` (must PASS on the tree),
`cargo test -p archcheck` if tests exist. Commit `feat(archcheck): core-purity
rule (core/* never deps modules|api) + svc-constructs-its-module tripwire`.

## Step 15 — Derive the fortress crate list; cross-check the fleet (G3) `[sonnet]`

**(a) What:** `verify.sh:146` + `verify.ps1:108` (fortress build list),
`tools/checkmodules/src/lib.rs` (new test), NOT the split-proof port maps
(ports are inherently manual config; the cross-check test covers membership).

**(b) Why now:** with steps 13–14 the checkers are trustworthy; this step makes
the lists they gate self-maintaining.

**(c) How:**
- verify.sh: new `fortress_crates()` mirroring `public_api_crates()` (`:93-100`):
  `for f in cmd/*-svc/Cargo.toml; do sed -n 's/^name = "\(.*\)"/\1/p' "$f" | head -1; done`
  plus literal `server`; the fortress stage builds `$(fortress_crates)`.
  verify.ps1: twin `Get-FortressCrates` mirroring `Get-PublicApiCrates`.
- checkmodules: new test `split_fleet_matches_cmd_dirs` — assert the name set
  from `split_process_modules()` equals the filesystem set of `cmd/*-svc` dirs
  (read via `CARGO_MANIFEST_DIR/../../cmd` glob). A 13th svc crate then fails
  this test until added to the vec — the manual list stays (compile-time calls
  can't be derived) but drift becomes loud.
- split-proof port tables: add a comment in both scripts pointing at the
  checkmodules test as the membership source of truth.

**(d) Verification:** `./verify.ps1 --fast` (one rollout; fortress stage must
still build all 13). Commit `chore(verify,checkmodules): derive fortress crate
list from cmd/*-svc; fleet-membership drift test`.

## Step 16 — Stale outbox/inbox doc sweep (G5) `[sonnet]`

**(a) What:** exactly the 30 verified doc-comment lines: scheduler `lib.rs:6,172`;
characters `lib.rs:10,68,76,169,193,236`; match `lib.rs:52,70,94`; accounts
`lib.rs:166,207` + `store.rs:3,71`; inventory `lib.rs:106,123,265,277,574,591`;
audit `lib.rs:11,17,111,116-117,326,334,354`; leaderboard `lib.rs:11,44,148`.
(Line numbers shift after steps 5 and 11 touch inventory/accounts — re-grep
`outbox|inbox` over `modules/` at execution time; the token list is the spec,
not the line numbers.)

**(b) Why now:** last — pure prose, applied to the final code.

**(c) How:** rewrite vocabulary, meaning-preserving:
- "outbox row / outbox write / outbox emit" → "durable event append
  (`emit_tx` on the same tx)"; "bus → outbox → sink seam" (scheduler:6) →
  "bus → shared event log seam".
- "inbox-dedup tx / inbox dedup row" → "delivery tx (effect + checkpoint
  commit together)"; drop dedup-row phrasing entirely (README:47: no inbox, no
  dedup table).
- Do NOT touch `core/asyncevents` (its outbox/inbox mentions correctly describe
  the dead mechanism) nor `experiments/`, `docs/plans/`, `memory/`.

**(d) Verification:** `cargo test --workspace` docs don't compile-break
(doc-comment only; run clippy), grep confirms zero `outbox|inbox` hits under
`modules/`. Commit `docs(modules): replace retired outbox/inbox vocabulary with
the pull-log delivery-tx model`.

---

## Rollout notes

- **One test rollout at a time** — every step's verification is sequential;
  no parallel implementation subagents running tests. Check for running
  cargo/rustc before each rollout.
- Steps 1–2 (gateway), 3–4 (event plane), 6–8 (app plane), 13–15 (checkers)
  are natural pairing for review but each commits separately.
- Subagent dispatch: every code-writing Agent call passes explicit `model:`
  per its tag; trailer = executing model; trailer audit after the rollout.
- Deferred (explicitly out of scope): making `.ps1` scripts send a real
  console event on Windows (needs a `GenerateConsoleCtrlEvent` helper process —
  documented limitation in step 6 instead); deriving split-proof port maps
  (manual config, drift-guarded by step 15's membership test).

## Reviewer punch list — disposition

Grumpy-reviewer pass (Fable, think hard) returned 11 items, verdict
SHIP-WITH-FIXES. All 11 addressed inline above: (1) Step 5 advisory
xact-lock per character — the tombstone alone raced concurrent deliveries;
(2) Step 4 CAS extended with `consecutive_failures` — cursor-only had a
retry/failure ABA; (3) Step 11 split-proof uses the proof-tick `last_fired`
reset, synthetic-append branch deleted; (4) Step 13 dual check — tuple pass
for `on()` + topic pass for raw `subscribe` (version is erased at
`Bus::on`); (5) Step 6 unix arm cross-target `cargo check`; (6) Step 9 trap
split (`INT TERM` → `teardown; exit 1`); (7) Step 7 `watch` only, `Notify`
alternative deleted; (8) Step 7 AGENTS.md mirror added; (9) Step 1 route
methods confirmed (`POST /start`, `GET /callback`, epic_oauth.rs:155-165) +
EPIC_* env safety noted; (10) Step 8 pool ≥ 2 invariant stated; (11) Step 12
readyz-panic blast radius + exhaustive slot-caller re-verification noted.
Nothing deferred.
