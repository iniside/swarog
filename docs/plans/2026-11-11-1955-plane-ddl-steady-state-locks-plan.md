# Plane DDL takes no table lock in steady state — plan

## Context

**Owner (2026-11-11):** fix the source ("zawsze naprawiamy u źródła"); no test-side
workaround.

**Confirmed root cause** (diagnosis, docker-followups Step 4; reproduced 2/8 runs with live
`pg_stat_activity` + `pg_blocking_pids` samples): `ensure_schema_with_lock_timeout`
(`core/asyncevents/src/store.rs:242-`) runs `V2_DDL_TEMPLATE` on EVERY boot/`test_pool()`.
Two statements take a table lock on `asyncevents.events` even when the object already exists:
`CREATE INDEX IF NOT EXISTS events_scan …` (SHARE) and
`CREATE OR REPLACE TRIGGER events_notify …` (SHARE ROW EXCLUSIVE). A migrator holding the
migrate advisory lock waits on an open append transaction; that transaction's own next
append queues behind the migrator's pending lock → an undetected cycle broken only by the
60s `lock_timeout`; all queued migrators fail together. Production impact: any process
booting against a busy plane (e.g. restarting one `*-svc` in the split) queues every
writer behind SHARE/SHARE ROW EXCLUSIVE for up to 60s.
Second defect: the error path maps ANY SQLSTATE 55P03 in the migrate tx to "V2 plane
advisory lock not acquired" (`store.rs:~294-307`), so the relation-lock case is mislabelled.

Other template statements (`CREATE SCHEMA/TABLE IF NOT EXISTS`, `CREATE OR REPLACE FUNCTION`)
take no lock on `events` when the objects exist. The splitter already handles `$$` bodies
(three plpgsql functions in the template).

Schema-change policy (`.agents/shared/core-rules.md` **No Dual-Write, No Topology Branch,
Wipe**): a changed index/trigger definition is a contract break → DROP + boot fresh. So
"create only when missing" matches policy; `CREATE OR REPLACE` semantics for the trigger are
not needed.

## Steps

### Step 1 — create the index and trigger only when missing `[opus]`
(a) `core/asyncevents/src/store.rs` `V2_DDL_TEMPLATE`: replace the `CREATE INDEX IF NOT
EXISTS events_scan …` statement and the `CREATE OR REPLACE TRIGGER events_notify …`
statement each with a `DO $$ BEGIN IF NOT EXISTS (<catalog probe>) THEN <same DDL>; END IF;
END $$;` block. Probes: `pg_class`/`pg_index` (or `to_regclass('asyncevents.events_scan') IS
NULL`) for the index; `pg_trigger` joined to the `asyncevents.events` relation with
`tgname = 'events_notify' AND NOT tgisinternal` for the trigger. Same DDL text inside; the
trigger becomes `CREATE TRIGGER` (create-only). `V2_DDL_STATEMENT_COUNT` stays 14 (one
statement replaced by one block each) — the splitter tripwire proves it. Update the
template's doc comment to state the invariant: a steady-state run takes no lock on
`asyncevents.events`; definition changes follow the wipe policy.
(b) First — Step 3's test depends on it.
(c) Probes run after `pg_advisory_xact_lock(migrate_key)` (same tx), so two migrators cannot
both see "missing".
(d) `core-implementer`, `model:"opus"`, effort medium; `cargo check -p asyncevents`. Commit
`fix(asyncevents): create the scan index and notify trigger only when missing so a boot takes no events lock (plane-ddl-steady-state-locks step 1)`.

### Step 2 — name the lock that actually timed out `[opus]`
(a) `store.rs` error path: extract a pure `fn lock_timeout_message(stmt: &str, lock_timeout: &str) -> String`
— the advisory wording only when the trimmed failing statement STARTS WITH
`SELECT pg_advisory_xact_lock(` (never a `contains` match: `append_event`'s body contains
`pg_advisory_xact_lock_shared`); keep the existing advisory wording's "V2 plane advisory
lock" and "pg_stat_activity" substrings (an existing test asserts them). Otherwise:
"asyncevents: lock on a plane relation not acquired within {lock_timeout} while running
`<whitespace-collapsed first ~120 chars of stmt>` — an open transaction holds a conflicting
lock; see pg_locks / pg_blocking_pids" (long enough to reach `events_scan` /
`events_notify` inside the Step 1 DO blocks). Use it at the 55P03 branch. Same commit:
rewrite the `V2_MIGRATE_LOCK_TIMEOUT` doc (`store.rs:~41-44`) and the 55P03 sentence near
`~192` — `SET LOCAL lock_timeout` bounds EVERY lock wait in the migrate tx, not only the
advisory one.
(b) Independent of Step 1 (same file → dispatched after Step 1 lands, never concurrently).
(d) `core-implementer`, `model:"opus"`. Commit `fix(asyncevents): say which lock a plane migrate timed out on (plane-ddl-steady-state-locks step 2)`.

### Step 3 — tests `[test-author]`
(a) `core/asyncevents/src/store_tests.rs`:
- Steady-state migrate does not wait on an open append: open a tx, `append(...)` (uncommitted),
  then `ensure_schema_with_lock_timeout(&pool, "2s")` on another connection must return Ok
  while the tx is still open; then roll back. Previously-wrong branch: the old template's
  `CREATE INDEX IF NOT EXISTS` waits for SHARE and fails with 55P03 after 2s. Must follow
  the file's `WRITER_LOCK_CHOREOGRAPHY` rule if it holds an appended tx across an await
  (read the rule at `store_tests.rs:39-55` and comply).
- Unit tests for `lock_timeout_message`: the real advisory statement → advisory wording;
  the real `append_event` CREATE FUNCTION statement → relation wording (not advisory); the
  real Step 1 DO-block statements taken from `split_sql_statements(V2_DDL_TEMPLATE…)` →
  message contains `events_scan` / `events_notify` respectively.
(b) After Steps 1 and 2.
(c) `cargo test -p asyncevents --lib` THREE times sequentially, each preceded by its own
`Get-Process cargo,rustc` check; report each. Then the orchestrator runs
`cargo test --workspace --no-fail-fast` once.
(d) `test-author`, `model:"sonnet"`. Commit `test(asyncevents): prove a steady-state plane migrate does not wait on an open append (plane-ddl-steady-state-locks step 3)`.

### Review
`core-reviewer` `model:"opus"` after each step's commit.
