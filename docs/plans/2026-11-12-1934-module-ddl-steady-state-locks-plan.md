# Module migrations take no table lock in steady state — plan

## Context

**Owner (2026-11-11):** "ok możesz, to się kwalifikuje na poprawę u źródła" — fix every
module's boot-time DDL at the source, same class as the plane fix
(`docs/plans/2026-11-11-1955-plane-ddl-steady-state-locks-plan.md`, 70aeb259..935dd9b3).
Owner: do not start implementation before approving this plan.

**Defect (sweep + plan review 2026-11-11, file:line evidence):**
- 43 `CREATE [UNIQUE] INDEX IF NOT EXISTS` statements in 11 modules + `core/admin` take
  SHARE on their table on EVERY boot even when the index exists (Postgres opens the table
  with ShareLock before the existence check). Hot tables include `accounts.sessions`,
  `accounts.refresh_tokens`, `wallet.ledger`, `inventory.holdings`, `mail.outbox`,
  `notifications.messages`, `audit.log`, `party.members`, `admin.forms`.
- `modules/accounts/src/lib.rs:187` `ALTER TABLE accounts.sessions ADD COLUMN IF NOT EXISTS
  family_id` takes ACCESS EXCLUSIVE (blocks reads — every login) on every boot; contradicts
  the wipe policy. No test depends on the ALTER.
- `modules/config/src/lib.rs:122` `CREATE OR REPLACE TRIGGER settings_notify` takes SHARE ROW
  EXCLUSIVE on `config.settings` on every boot.
- No bound: `App::migrate_with_lock_timeout` (`core/lifecycle/src/app.rs:140-214`) sets
  `lock_timeout` only on its detached advisory-lock connection (`:174`); every module's
  `migrate` runs `sqlx::raw_sql(SCHEMA_DDL).execute(pool)` on other pool connections with NO
  `lock_timeout`, so it waits unbounded behind an open write tx and queues every writer.
- Probe→create is race-free only under `App::migrate`'s advisory lock; tests call
  `module.migrate(&ctx)` directly without it, and parallel first-creation on a fresh DB races
  (`modules/rating/src/tests.rs:13-17` has a retry for exactly that).
- Test helpers that bypass `migrate` and run `raw_sql(SCHEMA_DDL)` directly:
  `modules/config/src/tests.rs:~155`, `modules/audit/src/tests.rs:~19`,
  `modules/leaderboard/src/tests.rs:~14`, `modules/scheduler/src/tests.rs:~38`,
  `modules/apikeys/src/store_tests.rs:~21`.
- `tools/adminctl/src/lib.rs:133` runs `sqlx::raw_sql(admin::USERS_DDL)` outside any DDL
  lock, so on a fresh DB it can race a booting server's `Admin::migrate`.
- Workarounds caused by the defect: `modules/config/src/tests.rs:~126-129` and
  `modules/characters/src/tests.rs:~122-145` (`OnceCell` "migrate exactly once" because
  idempotent re-runs deadlock); `core/admin/src/lib.rs:~165-176` (table order chosen to
  dodge a live-login deadlock against `CREATE INDEX IF NOT EXISTS`).
- Docs prescribing the old shape: `.agents/shared/core-rules.md:~175` (MANDATORY section —
  owner sign-off on the wording), `.agents/shared/gamebackend.md:~712`,
  `.agents/skills/add-game-module/SKILL.md:~54`, `docs/reference/module-reference.md`.

Not findings: `CREATE SCHEMA/TABLE IF NOT EXISTS` on existing objects, `CREATE OR REPLACE
FUNCTION`, `INSERT … ON CONFLICT DO NOTHING` seeds. No `CONCURRENTLY` or non-transactional
DDL exists in module migrations, so one transaction per module is safe.

**Why one shared mechanism:** the guard, the bound and the race-freedom are identical for
all 18 `migrate` sites (17 modules + `core/admin`) plus adminctl; a shared runner in `core/lifecycle` (every module already depends
on it for `Module`/`Context`) makes the next module correct by construction, and a gate
keeps raw locking DDL out. Modules keep owning their SQL text. One rail: EVERY module's
DDL goes through it (also match, rating, leaderboard, scheduler, apikeys, which have no
index today).

## Steps

### Step 1 — `lifecycle::Ddl` `[opus]`
(a) New `core/lifecycle/src/ddl.rs`, re-exported from `core/lifecycle/src/lib.rs`:
`pub struct Ddl` built from `&PgPool`; builder methods `sql(&'static str)` (idempotent
no-lock statements: schemas, tables, functions, seeds), `index(qualified_name: &'static str,
create_sql: &'static str)` (runs `create_sql` only when `to_regclass(qualified_name) IS
NULL`; tripwire `assert!` that `create_sql`, whitespace-collapsed, contains
`INDEX <unqualified name> ON <schema>.` so a mismatched name cannot silently skip),
`trigger(table: &'static str, name: &'static str, create_sql: &'static str)` (runs only when
no non-internal `pg_trigger` row named `name` exists on `table::regclass`; same tripwire on
`TRIGGER <name>`), `pub async fn run(self) -> anyhow::Result<()>` delegating to
`pub(crate) async fn run_with_lock_timeout(self, t: &str)`. The run: ONE transaction;
`SET LOCAL lock_timeout = '<t>'` (`run` passes the existing `MODULE_MIGRATE_LOCK_TIMEOUT`);
`SELECT pg_advisory_xact_lock(<DDL_LOCK_KEY>)` — a NEW key distinct from
`MODULE_MIGRATE_LOCK_KEY` (same key would self-deadlock against `App::migrate`'s session
lock) — so probe→create and first-creation are race-free for every caller incl. tests;
then the steps in order (`sql` via `sqlx::raw_sql` on the tx connection, probes as bound
queries); commit. A 55P03 names the step (advisory DDL lock, index/trigger name, or the
first ~120 whitespace-collapsed chars of the `sql` text) and says an open transaction holds
a conflicting lock (pg_locks / pg_blocking_pids). Doc: steady-state run takes no table lock;
changed index/trigger definitions follow the wipe policy (DROP + boot fresh). The doc
must not imply "never waits": `ON CONFLICT DO NOTHING` seeds can wait on an in-flight row
updater, now bounded by the `lock_timeout`.
(b) First; everything else calls it.
(d) `core-implementer`, `model:"opus"`. `cargo check -p lifecycle`. Commit
`feat(lifecycle): add a bounded create-only DDL runner for module migrations (module-ddl-steady-state-locks step 1)`.

### Step 2 — tests for `Ddl` `[test-author]`
(a) `core/lifecycle/src/ddl_tests.rs` (`#[cfg(test)] mod`), each test in its own unique
schema (`DROP SCHEMA … CASCADE` cleanup):
- steady state: table+index+trigger via `Ddl`; open a tx holding ROW EXCLUSIVE (uncommitted
  INSERT); a second `run_with_lock_timeout(.., "2s")` returns Ok while the tx is open, index
  oid unchanged (previously-wrong branch: raw `CREATE INDEX IF NOT EXISTS` /
  `CREATE OR REPLACE TRIGGER` would wait → 55P03);
- create branch: drop index/trigger, rerun → recreated;
- concurrent first creation: N parallel `run`s of the same spec on a fresh schema all
  return Ok (race-freedom from the xact lock);
- bounded: a raw locking statement via `sql(...)` behind an open conflicting tx fails with
  55P03 within the short bound and the message names the step;
- tripwire: mismatched `index` name / `create_sql` panics;
- message unit tests.
(b) After Step 1. (c) `cargo test -p lifecycle` 3× sequentially, each after its own
`Get-Process cargo,rustc` check.
(d) `test-author`, `model:"sonnet"`. Commit `test(lifecycle): prove Ddl takes no lock in steady state, is race-free and bounded (module-ddl-steady-state-locks step 2)`.

### Step 3 — `core/admin` on `Ddl` `[opus]`
(a) `core/admin/src/lib.rs` `Admin::migrate` (~345-353): one `Ddl` with the four consts'
tables/seeds via `sql`, each `CREATE INDEX IF NOT EXISTS` via
`index("admin.<idx>", "CREATE INDEX <idx> ON …")`. `admin` exposes one
`pub async fn migrate_users(pool: &PgPool) -> anyhow::Result<()>` that runs
`Ddl::new(pool).sql("CREATE SCHEMA IF NOT EXISTS admin;").sql(USERS_DDL).run()`, and
`tools/adminctl/src/lib.rs:133` calls it instead of `raw_sql(admin::USERS_DDL)` (adminctl
already depends on `admin`, which depends on `lifecycle`), so the user-table creation takes
the same DDL lock as a booting server. Delete the LOCK ORDER
deadlock rationale (~165-176): with no lock taken in steady state it no longer applies.
(b) After Step 1; disjoint from Steps 4-5.
(d) `core-implementer`, `model:"opus"`. `cargo check -p admin -p adminctl`. Commit
`fix(admin): migrate through the bounded create-only DDL runner (module-ddl-steady-state-locks step 3)`.

### Step 4 — mechanical module conversion `[sonnet]`
(a) `migrate` + `SCHEMA_DDL` of wallet, inventory, characters, storage (`src/store.rs`),
notifications, mail, groups, friends, party, audit: tables via `sql(...)`, every
`CREATE [UNIQUE] INDEX IF NOT EXISTS <idx> ON <schema>.<table> …` →
`index("<schema>.<idx>", "CREATE [UNIQUE] INDEX <idx> ON …")` with identical columns,
uniqueness and partial predicates. match (`lib.rs:~330`), rating (`~170`), leaderboard
(`~143`), scheduler (`~649`, `LazyLock<String>` deref to `&'static str`), apikeys (`~264`):
`raw_sql(SCHEMA_DDL)` → `Ddl::new(pool).sql(SCHEMA_DDL).run()`. Comment fix in the same
step: `modules/groups/src/projection.rs:~82` (index no longer "in `SCHEMA_DDL`").
(b) After Step 1; disjoint from Steps 3 and 5.
(d) `core-implementer`, `model:"sonnet"`. `cargo check --workspace`. One commit
`fix(modules): migrate module DDL through the bounded create-only runner (module-ddl-steady-state-locks step 4)`.

### Step 5 — accounts and config `[opus]`
(a) accounts: delete the `ALTER … ADD COLUMN IF NOT EXISTS family_id` and put `family_id`
(same type/constraints) inline in `CREATE TABLE IF NOT EXISTS accounts.sessions`; indexes
via `index(...)`; the unclaimed-handles check (`~1279-1295`) stays AFTER
`Ddl::run(...).await?`, outside its transaction. config: `settings_notify` via
`trigger("config.settings", "settings_notify", "CREATE TRIGGER …")`; `CREATE OR REPLACE
FUNCTION notify_changed` and the `config.revision` seed stay in `sql(...)`;
`seed_history_contract` (`~502`) stays after `run`, outside the transaction.
(b) After Step 1; disjoint files.
(d) `core-implementer`, `model:"opus"`. `cargo check -p accounts -p config`. Commit
`fix(accounts,config): no ALTER or trigger replace on boot (module-ddl-steady-state-locks step 5)`.

### Step 5b — test helpers through the module's migrate `[test-author]`
(a) `modules/config/src/tests.rs:~155`, `modules/audit/src/tests.rs:~19`,
`modules/leaderboard/src/tests.rs:~14`, `modules/scheduler/src/tests.rs:~38`,
`modules/apikeys/src/store_tests.rs:~21`: replace the direct `raw_sql(SCHEMA_DDL)` with the
module's own `migrate` via `Context::with_db` (or a `pub(crate) fn ddl(pool) -> Ddl` on the
module if `migrate` is unreachable there), so tests create exactly the production schema
incl. trigger/index and get the DDL lock.
(b) After Steps 4-5. (c) `cargo test -p <crate>` for each of the five, sequentially.
(d) `test-author`, `model:"sonnet"`. Commit `test(modules): build test schemas through the module migrate (module-ddl-steady-state-locks step 5b)`.

### Step 6 — gate: no raw locking DDL outside the runner `[opus]`
(a) `tools/archcheck`: new source rule — non-test Rust (reuse `is_test_source`,
`main.rs:~1448`) under `core/`, `modules/`, `cmd/`, `tools/`, `demos/` must not contain,
after whitespace collapse (multi-line literals), `INDEX\s+IF\s+NOT\s+EXISTS` (covers
UNIQUE), `OR\s+REPLACE\s+TRIGGER`, or `ADD\s+COLUMN`; and under the module-DDL owners
(`modules/`, `core/admin/`, `tools/adminctl/`) no `sqlx::raw_sql(` whose argument is a
`*_DDL` const. Exclusions are an explicit list: `core/asyncevents/src/store.rs` (plane
template, already DO-guarded), `core/asyncevents/src/lib.rs` (`LEGACY_DROP_DDL`, drops of
absent objects), `core/lifecycle/src/ddl.rs`, and `tools/archcheck` itself (following the
`BAN_SELF_EXCLUDE` precedent, `main.rs:~151`); the rule FAILS if an excluded path no longer
exists (memory `didnt-forget-scripts-must-self-check`).
(b) After Steps 3-5 (tree must be clean).
(d) `core-implementer`, `model:"opus"`. `cargo run -p archcheck` green. Commit
`feat(archcheck): forbid boot-time DDL that locks existing tables (module-ddl-steady-state-locks step 6)`.

### Step 6b — gate self-check tests `[test-author]`
(a) `tools/archcheck/src/tests.rs`: fixtures for each banned form incl. `CREATE UNIQUE INDEX
IF NOT EXISTS` and a multi-line `CREATE INDEX\n  IF NOT EXISTS` (red), the converted
`index(...)` form (green), a test-file location (ignored), and a missing exclusion path
(red).
(b) After Step 6. (d) `test-author`, `model:"sonnet"`; review adds `proof-auditor`.
Commit `test(archcheck): pin the boot-time locking DDL rule (module-ddl-steady-state-locks step 6b)`.

### Step 7 — remove the deadlock workarounds `[test-author]`
(a) `modules/config/src/tests.rs` `ASYNCEVENTS_READY` and `modules/characters/src/tests.rs`
`SCHEMA_READY` `OnceCell`s, the `core/admin/src/tests.rs` `SCHEMA_READY` `OnceCell` (~173-195;
erratum 2026-11-12: found during Step 3, its comment blames idempotent DDL deadlocks), and the rating retry (`modules/rating/src/tests.rs:13-17`): their
causes (locking idempotent DDL; unguarded first-creation race) are removed by the plane fix
and Steps 1-5 (`Ddl`'s own xact lock). Replace with plain per-test migrate. Prove on a
FRESH schema, not only the warm DB: before the runs, the orchestrator-approved recipe is to
run the crate's tests once against a freshly provisioned throwaway database
(`DATABASE_URL` pointing at a DB created via `scripts/db-provision.*` with a new name; on
Windows set `PGSUPERUSER` as platform-notes says; drop the throwaway DB afterwards), then
3× on the normal DB, sequentially. Any deadlock/duplicate/55P03 ⇒ keep that workaround and
report the evidence (no retry loop). Fix their comments either way.
(b) After Steps 4, 5, 5b.
(d) `test-author`, `model:"sonnet"`. Commit `test(config,characters,rating): migrate per test now that module DDL is lock-free and race-free (module-ddl-steady-state-locks step 7)`.
(e) Erratum 2026-11-12 (Step 7 review): the same `SCHEMA_READY` `OnceCell`, blaming
idempotent DDL deadlocks, also lives in wallet, groups, party, friends, accounts,
notifications, inventory, match and mail `src/tests.rs`; Step 7 removes those too, with
their "migrated once" doc lines and `modules/characters/src/tests.rs:~139-140`, under the
same rule (any deadlock/duplicate/55P03 ⇒ keep and report). The fresh-DB run needs the
owner's superuser credential; without it, Step 7 is proven on the warm DB only and says so.

### Step 7b — characters purge tests share one sweep lock `[test-author]`
(Erratum 2026-11-12.) `modules/characters/src/tests/purge.rs`: every test that plants or
sweeps `characters.purged_players` holds one test `DB_LOCK`, mirroring
`modules/friends/src/tests.rs:~36-44` and groups' `purge_tests::DB_LOCK`; cleanup runs on
failure too. Cause (Step 7 review): a sibling's whole-table `FOR UPDATE SKIP LOCKED` batch
holds this test's rows until it commits, after this test asserts. Same check for the other
modules with a `purged_players` sweep test lacking such a lock. Before the fix, show the
interleaving deterministically (no sleeps). Commit
`test(characters): serialize the purge sweep tests on one lock (module-ddl-steady-state-locks step 7b)`.

### Step 7c — bound the plane_meta seed `[opus]`
(Erratum 2026-11-12.) `core/asyncevents/src/store.rs` `plane_meta` seed (`INSERT … ON
CONFLICT DO NOTHING`, ~369-376) waits on an in-flight `bump_generation` without any
`lock_timeout`, now once per test migrate. Run it under the same bound as the rest of the
plane migrate. `core-implementer`; tests by `test-author` after. Commit
`fix(asyncevents): bound the plane_meta seed by the migrate lock_timeout (module-ddl-steady-state-locks step 7c)`.

### Step 8 — docs `[docs]`
(a) Module DDL goes through `lifecycle::Ddl` (`sql`/`index`/`trigger`); never raw
`CREATE INDEX IF NOT EXISTS` / `CREATE OR REPLACE TRIGGER` / `ALTER … ADD COLUMN` /
`raw_sql(SCHEMA_DDL)`; archcheck enforces it; link (don't restate) the wipe policy. Files:
`.agents/shared/gamebackend.md` (**Adding a module** + `~712`),
`.agents/skills/add-game-module/SKILL.md:~54`, `docs/reference/module-reference.md`, and
`.agents/shared/core-rules.md:~175` — that one is MANDATORY text: the docs-writer drafts the
replacement sentence and the orchestrator shows it to the owner before committing it.
(b) Last.
(d) `docs-writer`, `model:"sonnet"`. Commit `docs: module DDL goes through the bounded create-only runner (module-ddl-steady-state-locks step 8)`.

### Verification / review
`core-reviewer` `model:"opus"` after each step; `proof-auditor` `model:"opus"` on Steps 6/6b.
Every cargo run one at a time, each after its own `Get-Process cargo,rustc` check; Steps 3,
4, 5 may be implemented in parallel (disjoint files) but their cargo runs are serialized.
Final: `cargo test --workspace --no-fail-fast`, `cargo run -p archcheck`, and
`cargo run -p verifyctl -- --fast` (split-proof: every `*-svc` boots through the changed
migrate path).
