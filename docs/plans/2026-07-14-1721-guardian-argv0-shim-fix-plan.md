# Fix: Linux guardian must preserve `argv[0]` so a `cargo → rustup` shim dispatches as cargo

**Date:** 2026-07-14 17:21 (rev. after grumpy plan review)
**Status:** PLAN (awaiting approval)
**Scope:** `tools/processctl` (Linux guardian exec path) + one new Linux-only test module. No backend/module changes.

## Problem (confirmed root cause)

On Linux, every fleet tool (devctl, verifyctl, splitproof) resolves `cargo` off `PATH`
and spawns it through the **processctl guardian**. The guardian at
`tools/processctl/src/guardian.rs:51` does `std::fs::canonicalize(executable)` **before**
`Command::new(&executable)` at `:74`. On a standard Linux rustup install,
`~/.cargo/bin/cargo` is a **symlink → `rustup`**. `canonicalize` resolves it, so the
exec'd `argv[0]` basename becomes `rustup`. The rustup shim dispatches on `argv[0]`
basename, so it runs **as rustup itself**, and `rustup build --workspace …` →
`error: unexpected argument '--workspace' found / Usage: rustup[EXE] <+toolchain>`.

Every cargo-invoking `verifyctl --fast` stage FAILs (build, clippy, test, audit,
fortress, routecheck, codegen-freshness, contract-golden, conformance); stages that
run a pre-built real binary (docs-current, split-proof) PASS. Empirically confirmed in
WSL: `rustup build --workspace --exclude verifyctl` reproduces the exact log text; a
`cargo`-named symlink exec'd by basename `cargo` dispatches correctly as cargo 1.97.0;
standalone `cargo build --workspace` is exit 0. **This is a tooling bug, not a backend bug.**

## Why this authority, and why it's safe (research synthesis)

Three read-only research subagents converged; a grumpy plan review then verified each
claim against code:

- **Single authority.** All three tools funnel through this one guardian exec
  (verifyctl `runner.rs:185,205`; splitproof `main.rs:661`; devctl `supervisor.rs:376`).
  Fix `guardian.rs` → fix the fleet. Do **not** patch the three PATH resolvers.
- **No ownership/identity coupling.** `canonicalize@51` has exactly one reader,
  `Command::new@74` (verified: nothing between `:52` and `:396` touches the binding).
  Reaping keys off pgid/pidfd (`process.rs:373` Drop, `guardian.rs:161,237`), **never**
  the executable path. The identity path in the control handshake is read from
  `/proc/<pid>/exe` **after** exec (`guardian.rs:127`) — kernel-resolved regardless of
  `argv[0]`, so identity is unchanged. `ProcessError::IdentityMismatch` (`process.rs:100`)
  is dead code. The only path-equality checks
  (`devctl control.rs:274-276,455-458`; `supervisor.rs:163`; `lock.rs:159-165,570-573`)
  compare the **calling process's own** identity, never a spawned child's.
- **The invariant already exists — on Windows.** `platform/windows.rs:125-130` already
  canonicalizes *for validation only* and execs the **original** path (comment: "Validate
  the target without passing canonicalize's `\\?\` path through argv[0]"), with a
  Windows-only test `owned_child_preserves_non_verbatim_argv_zero` (`tests.rs:350-371`)
  pinning `argv[0] == original text`. The Linux guardian violates an invariant the
  codebase already committed to. This fix aligns Linux to it.
- **arg0 mechanics.** `CommandExt::arg0` sets `argv[0]` independently of the exec'd
  program, stable since Rust 1.45, composes with `pre_exec` with no ordering caveat.
  rustup proxies dispatch on `argv[0]` **basename** only and locate the toolchain via
  RUSTUP_HOME/their own exe — they are `argv[0]`-**directory-independent**, so exec'ing
  the canonical `rustup` binary with `arg0 = original .../cargo` path is behaviorally
  identical to exec'ing the symlink. The bug only manifests for **symlink** rustup
  installs (hardlink proxies canonicalize to themselves, basename stays `cargo`).

## Chosen approach: canonicalize for validation, set `argv[0]` to the original path

Keep `canonicalize` for its early, precisely-attributed I/O validation error; exec the
resolved real binary; set `argv[0]` back to the original (symlink) path via `arg0`.
Rejected Option B (drop `canonicalize`, `Command::new(&original)` and let `execve` follow
the symlink): functionally identical child behavior, one line shorter, but loses the early
validation error. We keep validation — a recorded decision.

---

## Step 1 — Fix the guardian exec (`tools/processctl/src/guardian.rs`) `[opus]` (core-implementer)

**(a) What:** `tools/processctl/src/guardian.rs`, lines 48-51 and 74.

**(b) Why now / order:** Single authority; everything else verifies it. Must land before
the test (Step 2) can go red→green and before the WSL re-verify (Step 4).

**(c) How (non-mechanical):** Split the canonical path (validation) from the `argv[0]` the
child observes. `CommandExt` is **already imported** at `guardian.rs:2` — do NOT add a new
`use` (would be E0252). Current:

```rust
let executable = args.next().ok_or_else(|| invalid("missing target executable"))?;
let executable = std::fs::canonicalize(executable)?;       // :51
…
let mut command = Command::new(&executable);                // :74
```

Change to:

```rust
let original_executable = args
    .next()
    .ok_or_else(|| invalid("missing target executable"))?;
// Canonicalize only to validate existence with a precise error; hand the ORIGINAL path
// to the child as argv[0]. A `cargo -> rustup` shim dispatches on argv[0]'s basename, so
// exec'ing the resolved `rustup` path would make it run as rustup. Mirrors the Windows
// split in platform/windows.rs:125-130.
let resolved_executable = std::fs::canonicalize(&original_executable)?;
…
let mut command = Command::new(&resolved_executable);
command.arg0(&original_executable);
```

`original_executable` is an `OsString` (from `args.next()`); `canonicalize`/`arg0` both
accept `AsRef` and take it fine. Confirm no other line reads the old `executable` binding.

**(d) Dispatch:** `[opus]` via `core-implementer` — authority-first, ships the
failing-branch proof (Step 2) in the same rollout.

## Step 2 — Failing-branch test in a NEW Linux-only module `[opus]` (same agent, same commit)

**Why not `tests.rs`:** `tools/processctl/src/tests.rs` is gated `#[cfg(all(test, windows))]`
(`lib.rs:66-67`) and has unconditional `windows_sys` uses — it does **not** compile on
Linux. A `#[cfg(target_os="linux")]` fn placed there is `windows && linux` = never compiled
(this is already the fate of the linux-cfg'd tests inside it — see Known Gap). So a test
there would be a **silent dead green**. We add a self-contained Linux module instead.

**(a) What:** New file `tools/processctl/src/guardian_tests.rs`, declared in `lib.rs`
beside the existing `protocol_tests` line (`lib.rs:15`):
`#[cfg(all(test, target_os = "linux"))] mod guardian_tests;`.

**(b) Why now / order:** Fix-the-Authority rule 5 — a test that executes the previously
wrong branch, on the at-risk topology (Linux).

**(c) How (self-contained, public API only — `OwnedChild`, `SpawnSpec`,
`ProcessGroupPolicy`, `OutputDestination` are re-exported at `lib.rs:21-24`):**

1. A child-entry `#[test] fn argv0_child()` that no-ops unless its env marker is set
   (mirrors `tests.rs:22-25`): `let Ok(ready) = std::env::var("PROCESSCTL_ARGV0_READY") else { return };`
   then write `std::env::args_os().next().unwrap()` (the child's `argv[0]`) to `ready`.
2. The proof `#[test] fn guardian_preserves_symlink_argv_zero()`:
   - tmpdir under `std::env::temp_dir()`; `symlink(std::env::current_exe()?, tmp/"cargo")`
     via `std::os::unix::fs::symlink`.
   - Build a `SpawnSpec` (copy the shape from `tests.rs:468-495`): `executable = tmp/"cargo"`
     (the symlink), `args = ["--exact", "guardian_tests::argv0_child", "--nocapture"]`,
     `env` carrying `PROCESSCTL_ARGV0_READY=<ready file>` + `PATH`,
     `process_group: ProcessGroupPolicy::Owned`, stdout/stderr to a file or null.
   - `OwnedChild::spawn(spec)`, poll for the ready file, read it, and assert the child's
     `argv[0]` **equals the symlink path** `tmp/"cargo"`, and explicitly **`!=
     std::fs::canonicalize(tmp/"cargo")`** (the resolved test binary). Reap.
   - **Red/green proof:** pre-fix the guardian execs `canonicalize(tmp/cargo)` → child
     `argv[0]` = the real test-binary path ≠ `tmp/cargo` → assertion FAILS. Post-fix
     `arg0` = `tmp/cargo` → PASSES. The `!= canonicalize(...)` assert nails the exact
     failing branch.

**(d) Dispatch:** `[opus]` — bundled into the core-implementer rollout with Step 1
(single commit `fix(processctl): …`).

## Step 3 — Independent adversarial review `[opus]` (core-reviewer)

**(a) What:** One `core-reviewer` pass over the Step 1+2 diff.

**(b) Why / order:** Attack the fix's own new seams first: does the new test actually go
red pre-fix (reason about it, or momentarily revert Step 1)? does `arg0` compose with
`pre_exec`? is the module cfg correct and Linux-only (no Windows-build breakage)? does the
child-entry no-op guard prevent it firing during the normal `cargo test` pass?

**(c) How:** Class-keyed read-only pass, method different from the implementer; verdict +
punch list; confirm `guardian.rs:127` identity readback untouched.

**(d) Dispatch:** `[opus]` core-reviewer (≥ implementer tier).

## Step 4 — End-to-end verification on the at-risk topology (Linux/WSL) `[inline]`

**(a) What:** In the WSL ext4 checkout (`~/GameBackend`): sync the fix, rebuild, run
`cargo test -p processctl guardian_tests` (new test green — it compiles on Linux, unlike
`tests.rs`), then rerun `cargo run -p verifyctl -- --fast` and confirm the previously
FAILing cargo stages now PASS.

**(b) Why / order:** Proves the fix end-to-end on the topology that failed. WSL Postgres is
a separate instance from the Windows one, so this does not contend on the shared Windows PG
(one-rollout rule) — still, no other Cargo rollout runs concurrently.

**(c) How:** `[inline]` — I drive WSL via PowerShell/`wsl`. Sync = **commit on Windows,
then `git -C ~/GameBackend pull`** (or re-clone). Do NOT hand-apply the edit in WSL — that
invites Windows/WSL divergence for a fix whose only executing proof runs in WSL. Capture the
verify summary table.

**(d) Dispatch:** `[inline]` (environment driving, not code authorship).

## Step 5 — Sibling sweep + record `[inline]`

**(a) What:** The genuine siblings are the **other rustup proxies** — `rustc`,
`cargo-clippy`, `rustdoc`, `cargo-fmt` — all `~/.cargo/bin/*` symlinks → `rustup`, spawned
through the same guardian, and all **fixed by Step 1** (the fix is basename-agnostic).
`dotnet` (`verifyctl/src/stages/csharp.rs:39-42`) shares the guardian code path but is a
host loader, **not** an `argv[0]`-multiplexing shim — it was never broken; do not claim it
as a fixed sibling. Confirm no Linux exec path bypasses the guardian.

**(b) Why:** Fix-the-Authority rule 6 — record siblings; single-authority is what makes the
sweep come back empty.

**(c) How:** One grep/trace confirmation, recorded in the commit body + errata below.

**(d) Dispatch:** `[inline]`.

## Commit plan

- One commit after Steps 1+2 (reviewed via Step 3): `fix(processctl): preserve argv[0] in Linux guardian so cargo→rustup shim dispatches as cargo`. Body names the authority, the Windows-invariant alignment, and the rustup-proxy siblings covered.
- Step 4/5 add no repo changes (verification only). Push only if the user asks.

## Known gap (recorded, NOT fixed here)

`tools/processctl/src/tests.rs` is gated `#[cfg(all(test, windows))]` (`lib.rs:66`), yet it
contains `#[cfg(target_os = "linux")]` tests (`target_cannot_inherit_guardian_control_pipe_descriptors`
at `:436`, the `fd-check` linux block at `:134`, and dead linux helpers at `:630-636`).
Those never compile → the Linux guardian exec path has **no** unit coverage today. Reviving
them means un-gating the module to `any(windows, target_os = "linux")` and cfg-gating every
unconditional `windows_sys` use (`:249-250,:279`, `stdin_is_safe_eof` `:595-617`), which may
surface unrelated Linux failures. That is a **separate follow-up**, deliberately out of this
minimal-closure rollout. Our new `guardian_tests.rs` gives the argv[0] branch its own real
Linux coverage without touching that harness.

## Out of scope (recorded)

- Not fixing the three PATH resolvers individually — the guardian is the authority.
- Not touching the Windows path (already correct; no import change needed).
- Not un-gating `tests.rs` (see Known Gap) — separate follow-up.
- Not adding env workarounds — the code fix is the authority.
