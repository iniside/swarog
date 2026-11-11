# Docker follow-ups — plan

## Context

**Owner's request (2026-11-11, Polish, own words):** "nic, trzeba to poprawić" — none of the
leftovers from the docker-compose-monolith rollout go to `docs/todo`; fix them. This plan
covers everything except the split-in-compose work (+ the dev-env generator that belongs to
it), which needs an owner decision on peer addressing and gets its own plan.

Leftovers (from `docs/plans/2026-11-11-1831-docker-compose-monolith-plan.md` rollout):
1. `PORT=host:port` is mangled: `normalize_addr` (`core/app/src/lib.rs:606-616`) prefixes `:`
   to any value not starting with `:`, so `127.0.0.1:9000` → `:127.0.0.1:9000` →
   `to_bind_addr` → `0.0.0.0:127.0.0.1:9000` → bind parse error. Its doc comment claims only
   `:8080`/`8080`; `to_bind_addr`'s doc (`:618-619`) already says "a fully-qualified
   `host:port` passes through unchanged" — the two disagree. Single caller `:437`.
2. Git Bash on Windows (MSYS) rewrites `/usr/local/bin/adminctl` (argv) and `/tls/cert.pem`
   (env values) into `C:/Program Files/Git/...` — observed in live acceptance; the README
   Docker section's commands fail there without `MSYS_NO_PATHCONV=1`. README has no PowerShell
   equivalents (no `read -rs`).
3. `RETENTION_PANIC_ONCE` (`core/asyncevents/src/retention.rs:148`, checked at the top of
   `retention::run` `:200`) is process-global; `retention_panic_flips_retention_flag_but_not_delivery_dead`
   (`core/asyncevents/src/tests.rs:229-`) sets it then calls `Plane::start` (`:239`). Any other
   `run` spawned in that window (`Plane::start` in `stop_terminates_active_backend_…` `tests.rs:302`;
   `healthy_sweeps_keep_retention_unstalled`, `persistent_sweep_failure_flips_retention_stalled`,
   `sweep_failure_increments_error_counter` in `retention_tests.rs`) steals the panic.
4. Comment at `core/asyncevents/src/retention_tests.rs:548-550` says `WRITER_LOCK_CHOREOGRAPHY`
   "does NOT apply" to `mid_sweep_registered_subscription_survives_stale_floor`, which takes it
   at `:557`.
5. Two `cargo test -p asyncevents --lib` runs (2026-11-11) failed many tests with
   "V2 plane advisory lock not acquired within 60s" (`store.rs:298-304`); `pg_stat_activity`
   was empty afterwards; following runs green. Cause unknown.
6. `docs/reference/testing.md` is mostly stale JVM/Quarkus prose; `docs/reference/hetzner-deploy-checklist.md:19`
   heading "Gateway (cmd/gateway-svc) env" — the TLS keys now apply to `cmd/server` too.

Out of scope (separate plan after the owner's decision): split in compose; generating
compose env from `tools/processctl/src/fleet.rs`.

## Steps

### Step 1 — `PORT` accepts `host:port` `[opus]`
(a) `core/app/src/lib.rs` `normalize_addr`: a value containing `:` (`host:port`,
`[::1]:port`, `:port`) is returned trimmed and unchanged; a bare port gets `:` prefixed;
blank → `DEFAULT_LISTEN_ADDR`. Doc comment rewritten to that contract, and the `PORT`
sentences in `Config::from_env`'s doc (`:282`) and the field doc (`:211`) updated to match
(note: under TLS the bind goes through a `SocketAddr` parse, so a hostname like
`localhost:9000` works only for plain HTTP — the doc says IP:port).
(b) First — Step 2's test exercises it.
(c) No other caller; `to_bind_addr` unchanged.
(d) `core-implementer`, `model:"opus"`, effort medium; `cargo check -p app`. Commit
`fix(app): accept a host:port PORT instead of mangling it (docker-followups step 1)`.

### Step 2 — tests for Step 1 `[test-author]`
(a) `core/app/src/tests.rs` next to `to_bind_addr_expands_colon_port`: `normalize_addr` cases
`"127.0.0.1:9000"`, `"[::1]:9000"`, `":9000"`, `"9000"`, `"  "`; previously-wrong branch =
`"127.0.0.1:9000"` (was `":127.0.0.1:9000"`). `cmd/server/tests/tls_front.rs`: switch `PORT`
back to `127.0.0.1:<p>` (loopback only, no firewall prompt) — the end-to-end proof that a
`host:port` PORT now binds.
(b) After Step 1. (c) `cargo test -p app normalize`, `cargo test -p server --test tls_front`.
(d) `test-author`, `model:"sonnet"`. Commit `test(app,server): pin host:port PORT handling (docker-followups step 2)`.

### Step 3 — per-instance retention panic seam `[opus]`
Fix the authority: the defect is that the panic-injection seam is process-global and any
`retention::run` consumes it; guarding it with `GLOBAL_SWEEP` would give that lock a second
job and the next unguarded spawner would steal the panic again.
(a) `core/asyncevents/src/retention.rs`: delete the `RETENTION_PANIC_ONCE` static (`:147-148`)
and its check in `run` (`:199-201`); `run`'s signature and the retention `Config` type are
UNCHANGED (no test state on the env parser's value type).
`core/asyncevents/src/lib.rs`: a `#[cfg(test)] retention_panic_once: bool` field on `Plane`
next to `retention` (`:245`; the only struct literal is `:259-268` — set it `false` there under
`#[cfg(test)]`) and a `#[cfg(test)]` builder `with_retention_panic_once(self) -> Self`. In
`Plane::start` (`:380-387`) the spawned future that is wrapped by
`AssertUnwindSafe(..).catch_unwind()` becomes an `async move` block that, under
`#[cfg(test)]`, panics if the flag is set and otherwise awaits `retention::run(..)` exactly as
today — the panic MUST stay inside the `catch_unwind` future so it flips `retention_dead`.
Non-test behaviour is byte-for-byte the same future.
(b) Before Step 3b (the test switches to the builder). Independent of Steps 1-2.
(c) `cargo check -p asyncevents` (lib) must be green; `cargo check -p asyncevents --tests`
may fail ONLY at the panic test's `RETENTION_PANIC_ONCE` use (fixed in 3b) — report the
error sites.
(d) `core-implementer`, `model:"opus"`, effort medium. Commit
`refactor(asyncevents): make the retention panic seam per plane instead of process-global (docker-followups step 3)`.

### Step 3b — panic test on the new seam + false comment `[test-author]`
(a) `core/asyncevents/src/tests.rs` `retention_panic_flips_retention_flag_but_not_delivery_dead`:
use the Step 3 builder instead of `RETENTION_PANIC_ONCE.store(true)`; no lock needed. Fix the
false comment at `retention_tests.rs:548-550` (the test does take `WRITER_LOCK_CHOREOGRAPHY`
at `:557`). Previously-wrong branch: a concurrently spawned `run` (other test) can no longer
consume the panic — state why in one line; optionally prove with a test that spawns a second
plane without the flag and asserts it does not panic.
(b) After Step 3.
(c) `cargo test -p asyncevents --lib` THREE times SEQUENTIALLY (one cargo at a time, each
with default test-thread parallelism), each preceded by its own
`Get-Process cargo,rustc` check; report each exit code.
(d) `test-author`, `model:"sonnet"`. Commit `test(asyncevents): drive the retention panic through the per-plane seam (docker-followups step 3b)`.

### Step 4 — diagnose the plane advisory-lock timeouts `[research]`
(a) Read-only: `core/asyncevents/src/store.rs` `ensure_schema_with_lock_timeout` and every
holder of the V2 plane advisory lock in the lib test binary; find what can hold it ≥60s.
Lead hypothesis first: `store.rs:290-304` labels ANY 55P03 in the migrate transaction as
"advisory lock not acquired", but `SET LOCAL lock_timeout` covers every lock wait there —
incl. `CREATE OR REPLACE TRIGGER events_notify` (SHARE ROW EXCLUSIVE on `asyncevents.events`)
and `CREATE INDEX IF NOT EXISTS events_scan` — which can wait behind an open append
transaction or a test's trigger-DDL ACCESS EXCLUSIVE while all other `test_pool()` callers
queue on the advisory lock. Others: a test blocking on `GLOBAL_SWEEP`/`WRITER_LOCK_CHOREOGRAPHY`
while holding a connection mid-migrate, a leaked idle-in-tx session, pool exhaustion. The
prompt loads the `safe-verification` skill and gamebackend.md **One test rollout at a time**. May run `cargo test -p asyncevents --lib` repeatedly (serialized) and
sample `pg_locks`/`pg_stat_activity` WHILE a run is slow.
(b) After Step 3b lands (it changes which tests spawn sweepers).
(d) `general-purpose`, `model:"sonnet"`. Output: confirmed cause + fix shape, or "not
reproducible in N runs" with what was sampled. A fix, if any, comes back as a new step for
owner approval.

### Step 5 — docs `[docs]`
(a) `README.md` Docker section (`:255-296`): a Git Bash on Windows note — MSYS rewrites
POSIX-looking argv, so `docker compose run … --entrypoint /usr/local/bin/adminctl` fails with
the observed `exec: "C:/Program": stat C:/Program: no such file or directory`; prefix the
command with `MSYS_NO_PATHCONV=1`. For `TLS_*` paths, advise putting them in `.env` (compose
reads `.env` itself, so no shell conversion applies) rather than exporting them in Git Bash —
do not claim `MSYS_NO_PATHCONV` covers env values. PowerShell equivalents of the
admin-creation command and of setting `SWAROG_DB_PASSWORD`: password via
`Read-Host -AsSecureString` + Marshal BSTR into `$env:ADMINCTL_PASSWORD` for the `run`, then
removed — reuse the precedent in `install.ps1:32-42`. Only the admin-creation block gets a
PowerShell equivalent (`SWAROG_DB_PASSWORD` is set by editing `.env`; an exported
`$env:SWAROG_DB_PASSWORD` would silently override it — say so). README does not document
`PORT`; leave it. `docs/reference/testing.md`: delete the JVM/Quarkus prose; LINK (never
restate) gamebackend.md **Database** (testdb strict gate, local Postgres, no
Docker/testcontainers — `:693-705`), **Commands** (verifyctl stage map — `:565-581`),
**One test rollout at a time**, and the `safe-verification` skill. testing.md OWNS only test
placement and fixture-isolation conventions (e.g. `*_tests.rs` sibling files, per-test unique
topics/rows, shared-DB guards like `GLOBAL_SWEEP`/`WRITER_LOCK_CHOREOGRAPHY`) — check each for
an existing owner in `.agents/` and `docs/reference` first and link instead if owned.
`hetzner-deploy-checklist.md` section at `:19`: heading covers both public front mains
(`cmd/gateway-svc`, `cmd/server`); mark `ADMIN_HTTP_ADDR` (`:29`) gateway-only (only
`cmd/gateway-svc/src/addrs.rs:234` reads it); state that with `cmd/server` as the public front
`TRUSTED_PROXY_CIDRS` (`:35`) is unset when nothing proxies it, or the reverse proxy's range.
(b) Last; after Step 1 (PORT contract) lands.
(d) `docs-writer`, `model:"sonnet"`. Commit `docs: windows shells for docker, current testing reference, front-door env heading (docker-followups step 5)`.

### Review / verification
`core-reviewer` `model:"opus"` after each step's commit (Step 4 has no diff unless a fix is
approved). Every cargo run in every step: one at a time, each preceded by its own
`Get-Process cargo,rustc` check. Steps 1→2 and 3→3b run as two chains whose cargo runs never
overlap (the orchestrator dispatches the next cargo-running step only after the previous one
returns). Final: `cargo test --workspace --no-fail-fast` once, all green.
