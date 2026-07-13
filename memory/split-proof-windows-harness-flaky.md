---
name: split-proof-windows-harness-flaky
description: "Postmortem of the retired shell split-proof harnesses; current verification uses the Rust harness through verifyctl"
metadata: 
  node_type: memory
  type: project
  originSessionId: 38ae0b55-88ea-4c50-814e-ca71f55c726d
---

> **CURRENT STATUS (2026-07-13): historical postmortem, not recovery guidance.**
> Both shell harnesses and `winctrl` were deleted. The Rust `tools/splitproof`
> harness uses the centralized `processctl` fleet and runs inside exactly one
> terminal `verifyctl` manifest. Do not bypass it with the historical fallback below.

The retired live split-proof shell harnesses were unreliable on this Windows box for
reasons in the harness, not the services. Discovered 2026-07-11 while verifying the
all-findings remediation; the details below explain the deleted implementation.

**`split-proof.ps1` (native Windows harness):** `Start-Svc` spawns each service via
`winctrl` (added in af26dc5 for the graceful-shutdown W1/W2 proof) and throws
`"winctrl failed to spawn <svc>"` when `$spawn.ExitCode -ne 0`. But
`Start-Process -PassThru` + `.WaitForExit()` frequently leaves `.ExitCode` as `$null`,
and `$null -ne 0` is `$true` in PowerShell → a **false throw even though winctrl DID
spawn the child** (the .exe ends up listening on its port). Worse, the throw happens
during `$Proc = Start-Svc(...)`, so the assignment never lands → teardown has no handle
→ the spawned svc is **orphaned** (survives, holds its port + keeps piped stdout open).
Flaky: one run booted accounts-svc fine and failed later; the next false-threw on
accounts. Proper fix (not yet done — user deferred): gate spawn-success on
pid-file-written + process-alive, not on the flaky `$spawn.ExitCode`.

**`split-proof.sh` under git-bash:** DOES work on Windows for the fleet boot (spawns
the 12 `.exe` via `&`) and all SEQUENTIAL assertions — it booted 12/12 and passed 28
assertions (incl. new [K5]/[C4] + cross-process flows). But it **hangs on `[AD2b]`**,
the assertion that fires 12 parallel logins via bash background `&` + `wait`: git-bash
/ MSYS job-control wedges the `wait` (froze 25 min, admin-svc idle, 1 DB session — i.e.
the requests weren't even all dispatched). Any split-proof.sh assertion using
bash-parallel jobs is suspect on Windows.

**Historical recovery used before replacement:**
- To verify the CODE without a green split-proof: per-package `cargo test` (plane pkgs
  `--test-threads=1`, see [[asyncevents-single-invocation-parallelism-deadlocks]]) +
  archcheck + topiccheck --durability-strict + requirecheck --strict + confirm no
  `api/*/api|events` baseline file changed (public-api). That covered the 2026-07-11
  rollout when both live harnesses were unusable.
- When split-proof "hangs", check whether the LOG is still growing (mtime) and whether
  the relevant svc is CPU-busy vs idle — an idle svc + frozen log at a parallel-job
  assertion is the harness, not a service deadlock.
- Recovery: kill `*-svc`/`winctrl`/`server`/`curl` (spare `cowork-svc`), then
  `pg_terminate_backend` all non-psql gamebackend sessions, then re-run.
**DONE (2026-07-11): a Rust harness REPLACED both scripts; they + tools/winctrl are
deleted.** The blocking split-proof stage in `verifyctl` drives `tools/splitproof`,
which spawns the 12-svc fleet through `processctl` (typed env map + owned cleanup — no shell, so the whole
bug class is structurally gone: no quoting, no MSYS `wait`, no winctrl, no orphans),
health-checks over reqwest, asserts DB via sqlx, and drives the player QUIC front
through `edge::PlayerClient` as a library. The canonical fleet lives in
`tools/processctl/src/fleet.rs`; its drift preflight validates that fleet against
`cmd/*-svc`, and the harness is exempt in archcheck's
asyncevents-SQL allowlist (it asserts plane state like the scripts' `pg`). Full parity
reached: **66 named assertions** across the split (A/K/EP/[1-5]/C/MT/P/AD/AU/SC/SP/MX/RL)
+ monolith parity (M0-M3b, boots cmd/server on the same front) + native graceful
shutdown ([W2]: Ctrl-Break to the monolith's process group on Windows / SIGTERM on
unix). Current entry point: one selected `cargo run -p verifyctl -- <level>` manifest;
do not rehearse `--fast` before a broader terminal run. Commits
b0bacb2..e9ff199 (Batches A-G). Plan:
docs/plans/2026-07-11-1730-rust-splitproof-harness-plan.md.

Two harness insights worth keeping (baked into commit messages): sqlx's extended
protocol runs only ONE statement per `query()` (split multi-statement DELETEs); and the
harness drives HTTP far faster than the curl-per-process shell, so mutating helpers
retry on the gateway's transient per-IP 429 and concurrent admin-login bursts need a
long-timeout client (admin login holds an advisory lock across a 64 MiB Argon2). See
[[verify-the-at-risk-path-not-the-safe-one]] and [[config-as-code-anti-magic]].
