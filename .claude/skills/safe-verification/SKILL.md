---
name: safe-verification
description: Run tests/verification on this machine WITHOUT tripping the one-test-rollout-at-a-time constraint (shared local Postgres, events-plane advisory locks). Use BEFORE any cargo test, devctl fleet, verifyctl rollout, or dispatching a subagent that will run tests — and whenever a test run hangs, fails with "tuple concurrently updated", or seems stuck on migration. Also selects the minimal verification stage for the change instead of defaulting to full verify.
---

# Safe Verification

Every rollout-bearing run here (`cargo test`, `devctl up`, `verifyctl`) shares
ONE local Postgres. Concurrent runs contend on the events plane's migrate advisory
lock and concurrent DDL — which looks like a hang or fails with
`tuple concurrently updated`. This protocol is mandatory (CLAUDE.md: "One test
rollout at a time"), and it exists because the failure mode looks like *your*
change is broken when it isn't.

## Step 1 — Pre-flight: is anything already running?

```powershell
Get-Process | Where-Object { $_.ProcessName -match '^cargo$|^rustc$' }
```
(bash: `pgrep -x cargo; pgrep -x rustc`)

If Cargo/rustc is active, **never invoke another Cargo command**. To inspect or
stop an already-running foreground `devctl` fleet, use the already-built direct
binary: `target/debug/devctl.exe status` / `down` on Windows or
`target/debug/devctl status` / `down` on Unix (under the configured
`CARGO_TARGET_DIR` when it differs). Then **WAIT for or stop the owning rollout**
as appropriate. If you started it in the background, monitor it; do not launch
anything that compiles or tests (including `cargo clippy`) alongside it.

Only when Cargo/rustc are clear, run `cargo run -p devctl -- status`; it must
report no active fleet. After status exits, re-check Cargo/rustc before launching
exactly one selected rollout. Never start a second run "to check something
quickly" — that is the classic cause.

If status says no active fleet but `*-svc` or `server` processes remain, treat
them as leftovers and identify ownership before stopping anything; never kill
unrelated processes by name.

## Step 2 — Check for leftovers from a dead/hung run

A killed run can orphan test binaries holding advisory locks or
idle-in-transaction sessions. Check Postgres:

```bash
PGPASSWORD=gamebackend "/c/Program Files/PostgreSQL/18/bin/psql.exe" -U gamebackend -h localhost -d gamebackend -c \
 "SELECT pid, state, application_name, left(query,80) AS query, now()-xact_start AS xact_age
  FROM pg_stat_activity
  WHERE datname='gamebackend' AND pid <> pg_backend_pid()
  ORDER BY xact_start NULLS LAST;"
```

Suspects: `idle in transaction` sessions with old `xact_age`, or sessions
sitting on `asyncevents` statements. Kill the orphaned OS process first
(stray test binaries under `target\debug\deps\`), then if a session persists:
`SELECT pg_terminate_backend(<pid>);`. Only then retry.

## Step 3 — Pick the MINIMAL stage for the change

Don't reflexively run full verify after every edit. Match stage to blast
radius (cheapest that actually exercises the change):

| Change | Minimal check |
|---|---|
| Single-crate logic edit | `cargo test -p <crate>` (+ `cargo clippy -p <crate> --all-targets -- -D warnings`) |
| Touches a contract crate (`api/*`) | build workspace + tests of producer AND consumers; expect the `public-api` stage to need a bless if intentional |
| New/changed event or subscription | `cargo run -p topiccheck` + the consumer's tests |
| New require/capability wiring | `cargo run -p requirecheck` + `cargo run -p archcheck` (both cheap, no test DB contention) |
| Cross-process behavior, new module, gateway/edge change | Targeted/static checks now; reserve the live split-proof for the single terminal `verifyctl` manifest in Step 4 |
| Anything in core/ | `cargo test --workspace` |

archcheck / topiccheck / requirecheck are static (seconds); they can run while
you think, but not alongside a compiling test run of the same workspace
(they compile too).

## Step 4 — Full net LAST, once, at the end

When the rollout is done, run exactly one selected `verifyctl` manifest; it is
the local safety net and includes the blocking split-proof. Use
`cargo run -p verifyctl -- --fast` for blocking tiers only, or
`cargo run -p verifyctl -- --all --strict` when the advisory tiers must also
block. Never rehearse with `--fast` before a broader terminal manifest. Re-run
pre-flight (Step 1) before launching it.

## Subagents

At most ONE subagent may run tests at a time, and its prompt must include the
Step-1 pre-flight check verbatim. Sequential test steps, never parallel test
runs. A subagent doing pure static checks (archcheck/topiccheck) still compiles
— treat it as a test run for scheduling purposes.

## When a run hangs

Do not immediately kill and retry — that creates the Step-2 leftovers. First
check `pg_stat_activity` (Step 2 query): if the run is *waiting on the migrate
advisory lock*, something else holds it — find and resolve the holder. Kill the
run only after identifying what it's stuck on.
