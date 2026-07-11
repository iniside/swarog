---
name: split-proof-windows-harness-flaky
description: "Both split-proof harnesses are flaky on Windows independent of the code under test — .ps1 winctrl Start-Svc false-throws, .sh bash-parallel jobs hang under git-bash"
metadata: 
  node_type: memory
  type: project
  originSessionId: 38ae0b55-88ea-4c50-814e-ca71f55c726d
---

Running the live split-proof on this Windows box is unreliable for reasons in the
HARNESS, not the services — don't misattribute a harness hang to the code under test.
Discovered 2026-07-11 while verifying the all-findings remediation.

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

**How to apply / recover:**
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
- The real fix is the `.ps1 Start-Svc` ExitCode gate; the user deferred it 2026-07-11.
  See [[verify-the-at-risk-path-not-the-safe-one]].
