# macOS (aarch64-darwin) rollout port — COMPLETE

**Status: done and proven end-to-end on Apple Silicon (M4 Max), 2026-07-17.**
Plan: [`docs/plans/2026-07-17-1601-macos-rollout-port-plan.md`](../plans/2026-07-17-1601-macos-rollout-port-plan.md).

## Proof (the live rollout, staged, each a gate)

| Stage | Result |
| --- | --- |
| `cargo test --workspace` | 186 crates green (sanctioned path: `--exclude verifyctl`, verifyctl self-tested in an isolated target dir, mirroring the `test` verify stage) |
| `devctl up monolith` + smoke | monolith healthy on darwin; `POST /match/report` → 202, `GET /leaderboard` → 200 `[{"player":"alice","wins":1}]` (full durable event-bus flow: match → asyncevents → leaderboard projection), `/healthz` `/readyz` 200; clean `devctl down` (1 process reaped) |
| **`cargo run -p verifyctl -- --fast`** | **16/16 blocking stages PASS, exit 0** — incl. `split-proof` (the 12-service split fleet + monolith parity) and `weles-managed-gateway` |

## What the port touched (every darwin gap fixed, reviewed, and verified)

Core work — `processctl`'s darwin backend (the hard part; the real containment is
`guardian.rs`, not the thin `platform/linux.rs`):
- **Step 3** (`466bacd`) — widen the `libc` dep gate to `cfg(unix)` (prerequisite).
- **Step 5** — decision gate: a scratch probe proved `posix_spawn` with
  `POSIX_SPAWN_START_SUSPENDED|SETPGROUP|CLOEXEC_DEFAULT` captures `(pid, exe,
  start-time)` before the child image runs — parity with the Linux ptrace exec-trap.
- **Steps 4/6/7** (`76d52c7`) — POSIX-widen `lock.rs`/`state.rs`/`process.rs`;
  `platform/darwin.rs` + a kqueue guardian (`EVFILT_PROC`/`NOTE_EXIT` +
  `EVFILT_SIGNAL`). Review follow-up (`72619ec`): `POSIX_SPAWN_SETSIGDEF` for
  SIGPIPE parity + a crate `SpawnGuard` closing the non-atomic `pipe()`+`fcntl`
  window. 45 tests; two independent reviews (core-reviewer + proof-auditor) clean.
- **Step 7b** (`3706882`) — a Linux behaviour fix the port surfaced: `forced_group`
  now enumerates the group *before* the reap (kill while the pid is still pinned),
  closing a reused-pid window on both platforms without regressing `[W2]`.

Tooling:
- **Step 8/8b** (`a8a55a7`, `a62274c`) — devctl darwin: `LOCAL_PEERCRED`+`LOCAL_PEERPID`
  peer-cred (two getsockopts; `xucred` has no pid field); relocate 4 supervised-child
  tests to a guardian-dispatching `harness = false` target.
- **Step 9/9b/9c** (`be0f21b`, `a6d3764`, `089728d`) — weles darwin: real control
  endpoint replacing a bit-rotted stub (closing the gap `weles-design.md:490`
  recorded); the lock/platform layer (mirror processctl's `observe_process_identity`,
  tolerate `sweep_group` EPERM); reap the force-killed root in a tree test.
- **Step 10/10b** (`ff20e40`, `3c8579b`) — verifyctl+edgeca cfg widening; TMPDIR
  fleet-parity sync; a `cfg(unix)` test helper arm.
- **Step 11** (`17312f1`) — the `NotApplicablePlatform` scoring authority: applicability
  declared statically in the STAGE table; a BLOCKING stage can no longer green-escape;
  csharp's runtime exit-3→skip sniff deleted (a real client bug can't hide). Reversed a
  documented rule; proof-auditor confirmed the reversal and the short-circuit are pinned.
- **Step 12** (`27ebbd8`) — the `supported-targets` tripwire: `cargo check --target`
  over the ring-free `processctl`/`weles` for the three triples, self-provisioned by
  `rust-toolchain.toml`, with an always-on positive control and FAIL-not-SKIP on a
  missing target. proof-auditor: catches E0061-class rot from Windows/Linux without a Mac.
- **Step 14 fixes** — splitproof fleet_liveness harness (`2ebe036`); invalidation
  `try_recv`+`eager_reconnect(false)` so a terminated LISTEN backend surfaces to the
  reconnect authority (`e7363c4`); weles deploy sets the executable bit on unix
  (`c757c66`); managed-gateway `wait_fleet_serving` (a readiness race, not an address
  bug — `1143a00`); admin `AUTH_DDL` lock-order fix (`153bf4b`).

## Product bug surfaced (not a test issue)
`modules/admin`'s `AUTH_DDL` created `sessions` before `login_attempts`, the inverse of
the login write-path lock order (`authenticate_and_mint` DELETEs `login_attempts` then
INSERTs `sessions`) — a concurrent `migrate` + login could deadlock. This machine's high
test parallelism (~16-wide) exposed it as ~25% red admin runs; a slower/fewer-core box
(the Windows dev box) masked it. A more powerful machine is a better concurrency-bug
detector, not a worse one. Fixed by ordering the DDL to match the DML; 30/30 green.

## Two named degradations (macOS containment, accepted not hidden)
- `PR_SET_PDEATHSIG` — no darwin equivalent: a SIGKILLed guardian orphans its target.
- `PR_SET_CHILD_SUBREAPER` — none either: a `setsid()` escapee leaves the group
  unreachable by `kill(-pgid)`.
Primary paths keep full strength; `[W2]`'s `forced_remainder` reports `forced_group`
only (stronger than Windows, weaker than Linux).

## Deferred to a Linux run (cannot be proven from a Mac)
1. splitproof `[W2]` as a Linux *runtime* assertion (Step 7b changed the Linux
   `forced_remainder` derivation; reviewed clean + Linux compiles here, but the runtime
   drain is Linux-only).
2. A unit test for the Linux `/proc` pgrp parse (`guardian.rs` `list_process_group`).

## Governed-doc follow-up (pending user approval)
`CLAUDE.md` and `AGENTS.md` still say "Rollout tooling runs on Windows and Linux ONLY —
a Mac can build and review but never verify" — now false. Proposed correction awaiting
approval (these are the governing rules docs, not edited by a subagent lane).
