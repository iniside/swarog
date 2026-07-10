# Review round 2 — hardening fix plan (2026-07-10 21:53)

Fixes for the second review round (10 findings pasted 2026-07-10 evening; finding
#8's split-proof fleet tripwire already landed as `5f547cc`). Distinct from the
executed first-round plan `docs/plans/2026-07-10-1902-review-findings-fix-plan.md`
(F1–F14/G1–G5 — all 16 steps verified present in the tree).

Research inputs: six read-only subagent reports (asyncevents pause/metrics surface,
lifecycle/readyz/migrate-lock, gateway proxy, config-value validation sites,
topiccheck/checkmodules internals, house test/knob patterns). All file:line refs
below come from those reports against the current tree.

User decisions folded in:
- **#2 (paused subscription visibility): metrics + eventctl, NOT /readyz.**
- **#7 (sinkless events): explicit allowlist + blocking under `--durability-strict`.**
- Scope: all findings 1–10 (including #4, lock_timeout).

## Context — why these shapes, not others

- **#1 (starter validation): validate at the consumer, fall back to compiled
  defaults — never bail per-event.** The plane's poison-pause is the designed
  response to *undeliverable events*; a bad **config value** is not a property of
  the event, so failing the delivery poisons `inventory.character-created.v1` for
  every subsequent character — the exact failure the finding names. Inventory
  already owns both guards on the player-IAP path (`Store::item_exists`
  lib.rs:232-238; `qty <= 0 → invalid` lib.rs:410-411) — the starter path simply
  bypasses them. Why not validate in `modules/config` `Service::set`: config is a
  generic string store with no schema knowledge (`set` validates only ident
  syntax, lib.rs:222-239), a raw psql write bypasses it anyway (by design — the
  trigger path), and inventory is the only party that knows what a valid item is.
  Why not a CHECK/FK message parse: the fallback must also cover psql-written
  values, so it must sit on the read path. Cost stated plainly: the validation
  adds one `EXISTS` PK lookup (3-row table) to every starter grant's delivery
  tx — negligible, and it buys never-poisoning.
- **#2: gap-fill, not build-out.** Research shows the decision's substance already
  exists: `asyncevents_subscriptions_paused` (IntGauge, plane_metrics.rs:58-62),
  `asyncevents_subscription_consecutive_failures{subscription}`
  (plane_metrics.rs:50-57), `asyncevents_retention_blocked_age_seconds`
  (retention.rs:36-50), and `eventctl list` already prints STATE/FAILURES/LAG.
  Remaining gaps: no per-subscription paused indicator (the count gauge is
  unlabeled), and no "paused since when" anywhere (`SubInfo` has no `updated_at`,
  eventctl lib.rs:21-32). Step 7 adds exactly those two.
- **#3: the bound lives in core/lifecycle, the knob in core/app.** `core/lifecycle`
  has tokio only as a dev-dep (Cargo.toml), so per-module `tokio::time::timeout`
  needs `tokio = { workspace = true, features = ["time"] }` promoted to real deps
  — fine under constraint 1 (tokio is infrastructure, not a module/api crate).
  Bounding only the whole loop from core/app was rejected: one hung module would
  still consume the entire budget and skip the remaining modules' stops.
- **#4: `SET lock_timeout`, not `statement_timeout`** — statement_timeout would
  also cancel a blocked `pg_advisory_lock`, but it is broader than needed (it
  bounds every statement on the connection); `lock_timeout` scopes the abort to
  lock waits only (SQLSTATE 55P03). Set on the dedicated `lock_conn` only AND
  explicitly `RESET` before the connection returns to the pool — sqlx does NOT
  reset session GUCs on release, so without the RESET the cap would ride back
  into the pool and silently apply to later statements on that connection.
  First `SET`-GUC usage in the repo (verified: none exists).
- **#5: `connect_timeout` + `read_timeout`, NOT a whole-request `timeout()`.**
  reqwest 0.12's whole-request timeout bounds the entire body stream — wrong
  primitive for a streaming proxy. `read_timeout` resets per chunk: it tolerates
  a large-but-flowing admin page and kills a stalled origin. Constants hardcoded
  in proxy.rs (module may not read env; threading a Duration through
  `ProcessWiring`→`with_passthrough` adds public surface for a knob nobody tunes
  — config-as-code, anti-magic). Repo precedent for magnitude: accounts' two
  reqwest clients use 10s (epic_oauth.rs:66, epic.rs:51).
- **#7: the failure folds into `any_seam`, not `any_advisory`** — the blocking
  fortress invocation passes only `--durability-strict` (verify.sh:177,
  verify.ps1:153) and never `--strict`, so an advisory-bucket change would not
  block anything. `ALLOW_UNSUBSCRIBED` (main.rs:62) is already the allowlist
  mechanism and is legitimately empty today (all 6 topics have live subscribers
  in both profiles — audit alone covers all 6).
- **#8 (remainder): semantic test supersedes nothing, complements.**
  `checkmodules::split_process_modules()` already CALLS every svc's `modules()`
  and holds live `Box<dyn Module>` values — `any(|m| m.name() == prefix)` is a
  true construction proof. archcheck's textual `svc_lib_references_module`
  heuristic (main.rs:581-590) STAYS: it runs without executing module code and
  guards a different failure (dep present, constructor call deleted) at a
  different layer. `remote::Stub::name()` returns the *provider* name
  (core/remote lib.rs:288-290) and no svc hosts a stub named after itself
  (verified exhaustively), so the same-string check cannot false-positive.

Constraints observed: fortress rule; foundations never import modules/api; modules
never read env (knobs read in core/app / cmd mains); **one test rollout at a
time** (every step's verification is sequential — check for running cargo/rustc
first); never-monolith-only; wipe-over-migrations (new DDL stays idempotent
`CREATE … IF NOT EXISTS`; no ALTER-based data machinery); tests in separate
files; commit per step, Conventional Commits, trailer = executing model.

---

## Step 1 — Inventory: validate starter item + qty at the read (finding 1, HIGH) `[fable]`

**(a) What:** `modules/inventory/src/lib.rs` (`starter_spec` :299-305,
`grant_starter` :320-340), `modules/inventory/src/tests.rs`.

**(b) Why now:** the only HIGH; everything else is independent of it, and it
sets the "validate at the consumer" pattern step 2 mirrors.

**(c) How:**
- `grant_starter` (inside the delivery tx, after the tombstone check at :328):
  replace `let (item, qty) = self.starter_spec();` with a validated read:
  - `let (mut item, mut qty) = self.starter_spec();`
  - `if qty <= 0` → `tracing::warn!(qty, default = STARTER_QTY, "inventory: configured starter_qty invalid — using default")`,
    `qty = STARTER_QTY;` (covers negative → CHECK-violation poison, and 0 →
    silent no-op grant).
  - Item guard: the existing `Store::item_exists(&self, item_id)` (:232-238)
    queries `self.pool` — it CANNOT be used here (pool conn ≠ delivery tx; the
    TOCTOU-free claim requires the check on the same tx). Add a conn-taking
    variant mirroring `grant_exec`'s shape:
    `async fn item_exists_exec(&self, conn: &mut PgConnection, item_id: &str) -> Result<bool, sqlx::Error>`
    (same `SELECT 1 FROM inventory.items WHERE id = $1` via `fetch_optional`,
    on the handed conn); optionally reimplement the pool version over it. Then:
    `if !self.store.item_exists_exec(&mut *conn, &item).await.map_err(bus::Error::transport)?`
    → `tracing::warn!(%item, default = STARTER_ITEM, "inventory: configured starter_item unknown — using default")`,
    `item = STARTER_ITEM.to_string();` — check and insert in ONE tx. The compiled default `starter_sword` is seeded by this module's own
    idempotent migrate DDL (:51-55) in its own schema, so the fallback row is
    guaranteed present — the FK cannot fire on the default. Doc-comment this
    guarantee on the fallback branch.
- Extend the `grant_starter` doc comment (:307-319): bad config degrades to
  compiled defaults with a warn — a config typo must never poison the
  subscription; poison-pause remains reserved for genuinely undeliverable events.
- No caching added anywhere (preserves the Step-8 "no local cache" contract and
  the live-reload test's expectations).
- Tests (`tests.rs`, reuse `FakeConfig` + the live-Postgres harness of
  `grant_starter_then_wipe_on_conn` :161-188):
  1. `FakeConfig` returning `("no_such_item", 1)` → grant succeeds with
     `STARTER_ITEM`, holdings row exists, qty 1.
  2. `FakeConfig` returning `(STARTER_ITEM, -5)` → grant succeeds with qty
     `STARTER_QTY`.
  3. `FakeConfig` returning `(STARTER_ITEM, 0)` → same fallback to `STARTER_QTY`.
  4. Existing `grant_starter_reflects_config_after_invalidation_refresh`
     (:338-428) must pass UNCHANGED — `health_potion` is a seeded item, so the
     valid live-reload path still flows through.

**(d) Verification:** `cargo test -p inventory`, then `./split-proof.ps1` (the
config live-reload assertion exercises the changed path cross-process; one
rollout at a time). Commit `fix(inventory): starter grant validates configured
item/qty — bad config degrades to defaults instead of poisoning the subscription`.

## Step 2 — Audit retention + scheduler interval guards (finding 10) `[sonnet]`

**(a) What:** `modules/audit/src/lib.rs` (`init` :332, `PruneHandler::call`
:169-189), `modules/audit/src/tests.rs`; `modules/scheduler/src/lib.rs` (DDL
:73-85, `due_schedules` :96-104, `fire_locked` :160-192),
`modules/scheduler/src/tests.rs`.

**(b) Why now:** same "validate dangerous values" theme as step 1, both are
small and independent — land the theme as one pair before the timeout pack.

**(c) How:**
- **Audit:** in `init` (:332), after `env_int("AUDIT_RETENTION_DAYS", …)`:
  `if retention_days <= 0 { anyhow::bail!("audit: AUDIT_RETENTION_DAYS must be > 0 (got {retention_days}) — a non-positive retention would delete the ledger; unset it for the default {DEFAULT_RETENTION_DAYS}") }`.
  Fail-closed at boot (admin `ADMIN_USER` bail pattern, admin lib.rs:74-93): the
  value is env-sourced, read once at init — a typo should stop the process, not
  silently truncate history. (Unparseable/unset still falls back to 30 via
  `env_int` — only a *parseable* non-positive bails.) Belt-and-braces: add
  `debug_assert!(self.retention_days > 0)` in `PruneHandler::call` before the
  DELETE bind (:181).
- **Scheduler:** two layers, no ALTER:
  1. SQL filter: add `AND interval_seconds > 0` to BOTH due checks —
     `due_schedules` (:98-100) and the re-check in `fire_locked` (:164-170).
     **Extract both inline SQL literals to file-level `const DUE_SQL: &str` /
     `const FIRE_RECHECK_SQL: &str`** (today they are inline strings, which is
     why the anti-drift assertion below would otherwise have nothing to
     reference — `SCHEMA_DDL` is already a const and testable for the same
     reason). A bad row simply never fires; no per-tick log spam.
  2. Fresh-DB constraint: extend the `CREATE TABLE IF NOT EXISTS` DDL (:74-78)
     with `CHECK (interval_seconds > 0)`. `IF NOT EXISTS` no-ops on an existing
     table, so the LOCAL dev DB does not get the constraint until wiped —
     therefore the rollout of this step includes a one-time
     `DROP SCHEMA scheduler CASCADE` via psql BEFORE running the tests
     (sanctioned wipe-over-migrations; the next boot/test run recreates the
     schema with the CHECK, and the seed rows re-insert idempotently). State
     this in the step's (d).
- Tests:
  - audit `tests.rs`: env-based init test using the admin `with_admin_env`-style
    env-mutex harness (admin tests.rs:354-410 is the copy source):
    `AUDIT_RETENTION_DAYS=-1` → `init` errors; `=0` → errors; unset → ok.
    Keep `prune_deletes_aged_rows_only_for_prune_name` (:188-227) untouched.
  - scheduler `tests.rs`, exactly two new assertions:
    (i) `zero_interval_insert_violates_check` (live-Postgres):
    `INSERT INTO scheduler.schedules VALUES ('test-zero', 0)` must fail with a
    check-constraint violation (valid because (d) wipes the schema first, so
    the table carries the CHECK).
    (ii) `due_checks_filter_non_positive_intervals` (no DB): assert
    `DUE_SQL.contains("interval_seconds > 0")` AND
    `FIRE_RECHECK_SQL.contains("interval_seconds > 0")` — anti-drift on the
    consts, same style as `seeded_schedule_names_are_contract` (:159-170).
  - `fires_again_after_interval` (:217-245, interval=1) must pass unchanged —
    1 stays legal.

**(d) Verification:** one-time
`DROP SCHEMA scheduler CASCADE` via psql (wipe-over-migrations — the test run
recreates it with the CHECK + seed rows), then `cargo test -p audit -p scheduler`
(one rollout). Commit `fix(audit,scheduler): reject non-positive
AUDIT_RETENTION_DAYS at init; guard interval_seconds > 0 in DDL + due checks`.

## Step 3 — Bounded per-module stop + start-unwind (finding 3) `[opus]`

**(a) What:** `core/lifecycle/Cargo.toml`, `core/lifecycle/src/app.rs`
(`App::start` :135-161, `App::stop` :165-174, new field/setter),
`core/lifecycle/src/tests.rs`; `core/app/src/lib.rs` (`Config` :28-29/:57-73,
`from_env`/`from_values` :79-148, `ordered_teardown` :646, the `App`
construction site ~:335).

**(b) Why now:** the timeout pack (steps 3–5) shares one mental model
(bounded lifecycle waits); this is its anchor and the only one needing a Cargo
change.

**(c) How:**
- `core/lifecycle/Cargo.toml`: move `tokio` from `[dev-dependencies]` to
  `[dependencies]` as `tokio = { workspace = true, features = ["time"] }` (keep
  the dev-dep's richer features for tests).
- `App` gains `stop_grace: std::time::Duration` (default `Duration::from_millis(5000)`
  set in `App::new`) + builder-style `pub fn with_stop_grace(mut self, g: Duration) -> App`.
- `App::stop` (:165-174): wrap each `m.stop(&self.ctx)` in
  `tokio::time::timeout(self.stop_grace, …)`; `Err(Elapsed)` →
  `tracing::error!(module = m.name(), grace_ms = …, "module stop timed out; abandoning and continuing teardown")`
  and continue to the next module (extends the existing best-effort
  log-and-continue policy — the doc comment at :163-164 finally becomes true).
  Same wrap in the start-unwind loop (:143-155).
- `core/app`: new `DEFAULT_MODULE_STOP_GRACE_MS: u64 = 5000` const, `Config`
  field `module_stop_grace: Duration`, env `MODULE_STOP_GRACE_MS`, parsed in
  `from_values` with the exact trim/filter/parse/unwrap_or idiom of
  `EDGE_DRAIN_GRACE_MS` (:127-138), doc comment "read HERE, never by a module".
  Apply via `.with_stop_grace(cfg.module_stop_grace)` where the `App` is built.
- CLAUDE.md constraint 8: extend the drain-knob parenthetical with
  `MODULE_STOP_GRACE_MS default 5000 — read in core/app, never in modules`
  (mirror in AGENTS.md, repo practice).
- Tests (`core/lifecycle/src/tests.rs`, `RecMod` pattern :26-31): new `RecMod`
  variant whose `stop` is `tokio::time::sleep(Duration::from_secs(60))`;
  (1) `stop()` with `with_stop_grace(Duration::from_millis(100))` returns in
  well under 60s and the OTHER modules' stops are still logged in reverse order;
  (2) same for the start-unwind path (failing module N, hanging module N-1,
  module N-2 still stopped).

**(d) Verification:** `cargo test -p lifecycle -p app`, clippy (one rollout).
Commit `fix(lifecycle,app): per-module stop/start-unwind bounded by
MODULE_STOP_GRACE_MS (default 5000) — one hung module can no longer stall teardown`.

## Step 4 — Bounded /readyz checks (finding 9) `[sonnet]`

**(a) What:** `core/app/src/lib.rs` (`readyz_response` :655-678),
`core/app/src/tests.rs`.

**(b) Why now:** second member of the timeout pack; depends on nothing, but
reviewing it together with step 3 keeps the "bounded waits" context.

**(c) How:**
- New const in core/app: `READY_CHECK_TIMEOUT: Duration = Duration::from_secs(2)`
  (no env knob — a readiness probe budget is not a tuning surface;
  config-as-code/anti-magic. LB probe timeouts are typically 5–10s, so 2s per
  check keeps even DB-ping + 2 checks under budget). Const comment states the
  deliberate trade-off: the bound on the DB ping INCLUDES pool-acquire wait, so
  a pool-saturation spike now yields a fast 503 (instance pulled from rotation
  while busy) instead of waiting out contention — for a readiness probe, "busy
  to the point of 2s acquire wait" IS not-ready; chosen, not accidental.
- `readyz_response` gains a `bound: Duration` parameter (the route closure
  passes `READY_CHECK_TIMEOUT`); wrap the DB ping (:663) and each
  `check.run()` (:669) in `tokio::time::timeout(bound, …)`; `Err(Elapsed)`
  inserts `("db" | check.name(), format!("timed out after {bound:?}"))` into
  `failures` — a fast diagnostic 503 instead of an LB-side timeout. The param
  keeps the fn unit-testable without 2s wall-clock waits (its stated design,
  :653-654).
- Tests (`core/app/src/tests.rs`, next to the existing fabricated-check tests
  :178-188): a
  `ReadyCheck::new("hang", || std::future::pending::<Result<(), String>>())`
  (turbofish required — the closure's future type is otherwise unbounded) with
  `bound = Duration::from_millis(50)` → response is 503 quickly and the JSON
  body maps `"hang"` to the timeout message; existing pass/fail tests pass the
  const and stay unchanged.

**(d) Verification:** `cargo test -p app` (one rollout). Commit
`fix(app): /readyz bounds the DB ping and every contributed check at 2s — fast
diagnostic 503 instead of a hung probe`.

## Step 5 — Migrate advisory-lock deadline (finding 4) `[sonnet]`

**(a) What:** `core/lifecycle/src/app.rs` (`App::migrate` :81-115),
`core/lifecycle/src/tests.rs`.

**(b) Why now:** last member of the timeout pack; touches the same file as
step 3, so it lands after it to avoid churn.

**(c) How:**
- New const `MODULE_MIGRATE_LOCK_TIMEOUT: &str = "60s"` beside
  `MODULE_MIGRATE_LOCK_KEY` (both need `pub(crate)` — the sibling `tests`
  module can't see private items). After acquiring `lock_conn` (:93-96) and
  BEFORE `pg_advisory_lock`, run
  `sqlx::query("SET lock_timeout = '60s'").execute(&mut *lock_conn)`.
  On the subsequent `pg_advisory_lock` erroring with the lock-timeout SQLSTATE
  (`55P03`), map the context to
  `"module-migrate advisory lock not acquired within 60s — another process is stuck mid-migrate; see pg_stat_activity"`.
  **After the unlock (both success and error paths, before `drop(lock_conn)`),
  run `RESET lock_timeout` on the same connection** — sqlx does not reset
  session GUCs on release, and the connection returns to the shared pool where
  module DDL would otherwise inherit the cap. Comment: `lock_timeout` scopes
  the abort to lock waits only (statement_timeout would also work but bounds
  every statement — broader than needed). The pool-max ≥ 2 invariant comment
  (:88-92) stays.
- 60s rationale in the comment: real contention is another replica actively
  migrating (seconds); only a stuck holder exceeds a minute, and failing loudly
  beats a silent hang given /readyz isn't serving yet at this phase.
- Test (`tests.rs`, shape of
  `concurrent_migrate_runs_are_serialized_by_advisory_lock` :191-221): acquire
  `pg_advisory_lock(MODULE_MIGRATE_LOCK_KEY)` on a raw held connection, then —
  with a test-lowered timeout (make the GUC value a parameter of a
  `pub(crate) async fn migrate_with_lock_timeout(&self, t: &str)` that
  `migrate` calls with the const, so the test passes `"200ms"` without waiting
  a minute) — assert the call returns `Err` containing the "not acquired"
  context instead of hanging; release, re-run, assert Ok.
  **Lock choreography (the `763f1d9` lesson):** this test holds the GLOBAL
  migrate advisory lock on the shared DB while
  `concurrent_migrate_runs_are_serialized_by_advisory_lock` may run on a
  parallel test thread — serialize the two through a shared
  `static LOCK_TESTS: std::sync::Mutex<()>` (same remedy that commit applied
  in asyncevents), stated in both tests' doc comments.

**(d) Verification:** `cargo test -p lifecycle` (one rollout). Commit
`fix(lifecycle): module-migrate advisory lock bounded by SET lock_timeout 60s —
a stuck holder fails startup loudly instead of hanging it`.

## Step 6 — Gateway proxy: timeouts + Connection-token stripping (findings 5, 6) `[opus]`

**(a) What:** `modules/gateway/src/proxy.rs` (builder :67-70, `HOP_BY_HOP`
:25-35, `forward` :114-117, `relay_response` :146-159),
`modules/gateway/src/proxy_tests.rs` (incl. the `table()` helper :16-29).

**(b) Why now:** independent of steps 1–5; one crate, one commit, both proxy
findings together.

**(c) How:**
- Builder (:67-70): add
  `.connect_timeout(Duration::from_secs(5)).read_timeout(Duration::from_secs(30))`.
  Consts `PROXY_CONNECT_TIMEOUT` / `PROXY_READ_TIMEOUT` at the top of proxy.rs
  with a comment: hardcoded by design (modules never read env; nobody tunes
  this — config-as-code), `read_timeout` chosen over whole-request `timeout()`
  because it resets per chunk (kills a stalled origin, tolerates a long flowing
  body). **Mirror the builder change into the `table()` test helper**
  (proxy_tests.rs:24) — known duplication, flagged by the round-1 plan too.
- New helper in proxy.rs:
  `fn strip_hop_by_hop(headers: &mut HeaderMap)` — first collect the tokens of
  every `Connection` header value (comma-separated, trimmed, lowercased;
  `headers.get_all("connection")` handles repeats), remove each named header,
  then remove the fixed `HOP_BY_HOP` list. Call it in `forward` (replacing
  :115-117) and in `relay_response` (replacing the `HOP_BY_HOP.contains`
  filter :148-152 — build the response `HeaderMap` first, then strip; or
  compute the connection-token set from `upstream.headers()` before the copy
  loop and filter on `token_set.contains(name) || HOP_BY_HOP.contains(name)`).
- Tests (`proxy_tests.rs`):
  1. Unit: `strip_hop_by_hop` removes `x-internal-auth` when
     `Connection: x-internal-auth` is present (and the `connection` header
     itself), leaves an unrelated header; handles a comma list
     (`Connection: keep-alive, x-a, x-b`).
  2. End-to-end request side (mirror `forward_proxies_verbatim_and_extends_xff`
     :120-142): send `Connection: x-secret` + `x-secret: v` through
     `forward()`; the `spawn_upstream` echo handler proves `x-secret` did NOT
     arrive.
  3. End-to-end response side: a `spawn_upstream` variant responding with
     `Connection: x-leak` + `x-leak: v` headers; assert the relayed response
     lacks both.
  4. Read-timeout: a third spawn helper `spawn_stalling_upstream()` — accepts
     the TCP connection, never writes a response
     (`tokio::time::sleep(Duration::from_secs(600))` in the handler). With a
     test-visible way to shrink the bound — make `from_routes` delegate to a
     `pub(crate) fn from_routes_with_timeouts(routes, connect: Duration, read: Duration)`
     and have the test build the table with `read = 300ms` — assert `forward()`
     returns 502 within ~1s (the existing Err→502 arm :135-138 already maps it).
     No connect-timeout integration test (no reliable un-acceptable address on a
     Windows dev box) — the builder line is covered by the read-timeout test
     exercising the same construction path.

**(d) Verification:** `cargo test -p gateway`, clippy (one rollout). Commit
`fix(gateway): proxy client gets connect/read timeouts; hop-by-hop stripping
honors Connection-header tokens both directions`.

## Step 7 — asyncevents: per-subscription paused gauge + pause age in eventctl (finding 2) `[sonnet]`

**(a) What:** `core/asyncevents/src/plane_metrics.rs` (`Gauges` :16-83, refresh
:85-117), `tools/eventctl/src/lib.rs` (`SubInfo` :21-32, `info` :72-113),
`tools/eventctl/src/main.rs` (`cmd_list` :96-118), their test files.

**(b) Why now:** pure additive observability, no behavior change; after the
behavior fixes so its verification tree is final.

**(c) How:**
- `plane_metrics.rs`: add `IntGaugeVec` `asyncevents_subscription_paused_state`
  (labels: `subscription`; 1 = paused, 0 = active/other) to the `Gauges` struct,
  registered in the same `OnceLock` block (:16-83, the crate's existing
  self-registration pattern — asyncevents already deps core/metrics), set in the
  same 10s `refresh` pass that already reads per-subscription state
  (:103-117). NOT on the delivery hot path. Name deliberately NOT
  `asyncevents_subscription_paused`, which would differ from the existing count
  gauge `asyncevents_subscriptions_paused` by one letter — a dashboard footgun.
  The existing unlabeled count stays (additive change only). Test read path:
  `fn gauges()` is module-private — add a `pub(crate)` accessor for the new
  gauge (or have the test read via `metrics::scrape()` text output; pick the
  accessor, it's assertion-friendlier).
- `eventctl`: add `paused_since: Option<String>` to `SubInfo` — sourced from
  the existing `updated_at` column (store.rs:72-86) as
  `CASE WHEN state = 'paused' THEN updated_at::text END` in the `info` query.
  `Option<String>` via `::text`, NOT a chrono type — eventctl's stated design
  is text-only ("the CLI never needs to name an xid8/timestamp codec",
  lib.rs:34-35; `next_attempt_at` is already `Option<String>` the same way).
  `cmd_list` prints a `PAUSED SINCE` column (empty for non-paused rows). Note
  in the fn doc: `updated_at` moves on any row update, so for a paused row it
  is "last state/failure write" — exact enough for an operator (pauses stop
  further writes).
- Tests: extend `poison_backs_off_then_pauses_never_skips`-adjacent coverage —
  in `worker_tests.rs`, after the existing forced pause (:339-391), call the
  metrics refresh fn directly and assert (via the new `pub(crate)` accessor)
  the labeled gauge reads 1 for that subscription id; eventctl `lib_tests.rs`:
  seed a paused sub via `insert_sub` + UPDATE state, assert `info()` returns
  `paused_since.is_some()` for it and `None` for an active one.

**(d) Verification:** `cargo test -p asyncevents -p eventctl` (one rollout).
Commit `feat(asyncevents,eventctl): per-subscription paused gauge + PAUSED
SINCE column — poison pauses visible per sub, not only as a count`.

## Step 8 — topiccheck: sinkless-event allowlist goes blocking (finding 7) `[opus]`

**(a) What:** `tools/topiccheck/src/main.rs` (`ALLOW_UNSUBSCRIBED` :62,
`unsubscribed()` :309-321, `run_profile` fold :396/:424-468, exit logic
:472-516), CLAUDE.md + AGENTS.md one-liner, topiccheck self-tests.

**(b) Why now:** checker pack (steps 8–9) runs last so the strengthened gates
validate the finished tree.

**(c) How:**
- Reclassify: `unsubscribed()`'s result (already allowlist-filtered) moves from
  the advisory bucket into the seam findings — in `run_profile`, a non-empty
  `unsub` sets `any_seam` (print block :462-468 reworded to
  `UNSUBSCRIBED (SEAM) — defined contract has no subscriber in this profile and
  is not in ALLOW_UNSUBSCRIBED`). `--durability-strict` (the blocking fortress
  invocation) now fails on sinkless topics automatically — no verify.sh/ps1
  flag changes needed (verified: fortress already passes `--durability-strict`,
  verify.sh:177 / verify.ps1:153).
- **`--strict` survives as a flag** (the advisory bucket becomes empty today,
  making the two flags momentarily equivalent — fine; `--strict` remains the
  "block on any future advisory too" gate and the advisory verify stage keeps
  calling it, verify.sh:558 / verify.ps1:540). Two now-stale doc comments MUST
  be rewritten in the same commit: the module header claiming
  "`--durability-strict` … but not on the advisory `unsubscribed`"
  (main.rs:42-46) and the exit-logic comment (:480-483) — both must state the
  new classification (unsubscribed-outside-allowlist = seam).
- `ALLOW_UNSUBSCRIBED` comment rewritten: it is now a *sanctioned-sinkless
  registry* — adding a topic here is the explicit "emitting today, consumer
  comes later" decision, reviewed in diff. Stays empty today (all 6 topics have
  live subscribers in both profiles).
- CLAUDE.md constraint 6 + the AGENTS.md mirror: the "defined-vs-subscribed
  drift" clause gains "(blocking under `--durability-strict`; sanctioned
  sinkless topics live in topiccheck's `ALLOW_UNSUBSCRIBED`)" so the docs stop
  overpromising relative to the checker (the original finding's core).
- Test: unit test on `unsubscribed()` (pure function, takes defined + subscribed
  + allow): a defined (topic, v) with no sub and empty allow → returned; same
  with the topic in allow → not returned. Plus an integration-shaped assertion
  in the existing self-test location (round-1 Step 13 added one) that the
  CURRENT tree yields zero unsubscribed in both profiles.

**(d) Verification:** `cargo run -p topiccheck -- --durability-strict` (must
exit 0 on the tree), `cargo test -p topiccheck` (one rollout). Commit
`feat(topiccheck): defined-but-unsubscribed topics fail --durability-strict
unless allowlisted — sinkless events become an explicit decision`.

## Step 9 — checkmodules: monolith completeness + semantic svc-construction tests (finding 8 remainder) `[sonnet]`

**(a) What:** `tools/checkmodules/src/tests.rs` (two new tests beside
`split_fleet_matches_cmd_dirs` :12-38).

**(b) Why now:** last — pure test additions over the final module lists.

**(c) How:**
- `monolith_hosts_every_modules_dir`: collect
  `monolith_modules().iter().map(|m| m.name().to_string())` into a set; read
  `CARGO_MANIFEST_DIR/../../modules` dirs (same read_dir shape as
  :19-30, filtering directories only); assert the dir set is a SUBSET of the
  name set (the monolith also lists `metrics` (core-infra) and, in future,
  possible stubs — extras are fine, gaps are not). All 12 `Module::name()`
  strings match their dir names verbatim today, including `match` (crate
  renamed `match_module`, `name()` still `"match"` — keyed off `name()`, so no
  keyword special-case). No exemption list: archcheck's `SVC_EXEMPT_MODULES` is
  empty and its own test asserts it stays so; if a legitimately monolith-absent
  module ever appears, THIS test is where its exemption gets added, with a
  comment.
- `each_svc_constructs_its_own_module`: for `(name, mods)` in
  `split_process_modules()`: `let prefix = name.strip_suffix("-svc").unwrap();`
  assert `mods.iter().any(|m| m.name() == prefix)`, with a failure message
  naming the svc and pointing at `cmd/<name>-svc/src/lib.rs`. Sound because
  `remote::Stub::name()` is the *provider's* name and no svc lists a stub for
  its own capability (self-stub would be a bug this test would rightly fail).
  archcheck's textual heuristic stays as the source-layer tripwire (different
  failure class, runs without executing module code) — add a cross-reference
  comment on both sides.

**(d) Verification:** `cargo test -p checkmodules` (one rollout), then the
full `./verify.ps1 --all` as the plan-closing gate (all steps landed: build,
clippy, test, fortress incl. the new blocking topiccheck behavior, split-proof,
public-api — no contract crate was touched, so no re-bless expected; core/*
changes are outside the baseline by construction). Commit
`test(checkmodules): monolith hosts every modules/ dir; every svc constructs
its own module (semantic complement to archcheck rule 12)`.

---

## Rollout notes

- **One test rollout at a time** — sequential verification per step; check
  `Get-Process | Where-Object { $_.ProcessName -match '^cargo$|^rustc$' }`
  before each. At most one test-running subagent at any moment; the check goes
  into every implementation subagent's prompt.
- Natural review pairs: 1–2 (value validation), 3–5 (bounded waits), 6 (proxy),
  7 (observability), 8–9 (checkers). Each commits separately.
- Dispatch: every code-writing Agent call passes explicit `model:` per its tag
  (`[fable]`→fable, `[opus]`→opus, `[sonnet]`→sonnet); trailer = executing
  model; trailer audit (`git log --format="%h %B" | grep Co-Authored`) before
  declaring done.
- Public-api baseline: NO step touches an `api/*` contract crate's surface —
  no re-bless expected. If clippy/public-api disagrees, STOP and inspect, don't
  bless blind.
- Deferred out of scope (deliberate): readiness semantics for paused
  subscriptions (user decision: metrics + eventctl only); deriving split-proof
  port maps (manual config, membership drift-guarded by `5f547cc` + round-1
  Step 15); a shared env-parsing helper crate (two coexisting house styles are
  fine — don't introduce a third).

## Reviewer punch list — disposition

Grumpy-reviewer pass (Fable, think hard) returned 13 items, verdict
SHIP-WITH-FIXES. All 13 addressed inline above: (1) Step 1 —
`item_exists_exec` conn-taking variant specified (pool-backed `item_exists`
would break the one-tx claim); (2) Step 2 — one-time
`DROP SCHEMA scheduler CASCADE` added to (d) so the CHECK exists when the
insert-violation test runs; (3) Step 2 — deliberation paragraph replaced with
the two final assertions; (4) Step 2 — due/re-check SQL extracted to consts so
the anti-drift assertion has something to reference; (5) Step 5 —
`RESET lock_timeout` before the connection returns to the pool (GUC leak);
(6) Context #4 + Step 5 — statement_timeout claim corrected (it WOULD cancel
the lock wait; lock_timeout is merely narrower); (7) Step 5 — `pub(crate)`
visibility + static-mutex serialization against the concurrent-migrate lock
test (the `763f1d9` choreography class); (8) Step 4 — pool-acquire-wait
trade-off stated as deliberate in the const comment; (9) Step 7 — gauge
renamed `…_paused_state` (one-letter collision with the count gauge) +
`pub(crate)` test accessor; (10) Step 7 — `paused_since` is `Option<String>`
via `::text` (eventctl's text-only design); (11) Step 8 — two stale topiccheck
doc comments added to the step, `--strict` explicitly survives; (12) Context #1
— per-grant EXISTS cost stated; (13) Step 4 — `readyz_response` takes the
bound as a param (no 2s wall-clock in tests) + `pending::<Result<(), String>>`
turbofish. Nothing deferred.
