# macOS (aarch64-darwin) rollout port — plan

**Goal:** full rollouts on macOS — `cargo test`, `devctl up`, `verifyctl --fast/--all`,
`splitproof`, and `weles up` run natively on Apple Silicon, with every degraded
guarantee named rather than discovered later.

**Status:** research complete (10 parallel subagents, 2026-07-17), plan not started.

---

## Context

### This is not a new feature — it closes a gap against a written non-negotiable

`docs/reference/weles-design.md:41` lists as a **non-negotiable**:

> **Cross-platform: Windows, Linux, macOS.** All platform abstraction lives in the
> agent behind a small trait (spawn_supervised / kill_tree / alive): process groups
> on unix, Job Objects on Windows.

`weles-design.md:205` (recorded 2026-07-10) commits macOS as a *live* platform:

> master on the Windows dev box + agent on the MacBook beside it, over a real LAN —
> non-loopback resolve, mTLS across a physical network, mixed-platform fleet. macOS
> is therefore a live test platform, not a theoretical target.

`weles-design.md:490` already records the exact gap this plan executes:

> **Known gap surfaced by that review:** the operator control endpoint supports
> Windows and Linux only, despite weles claiming macOS support — and macOS is the
> planned second machine. The fix is a native macOS UDS peer-cred implementation,
> not weakening local auth everywhere.

So the direction is decided; this plan is the execution. Two corrections to that
record, both established below: the weles defect is a **compile failure**, not the
graceful runtime restriction `:490` implies; and the port's hard part is not the
control endpoint at all — it is `processctl`'s guardian.

### Why not "just run a Linux VM" (the option not taken)

OrbStack is installed on this Mac and a Linux VM would run the fleet today. It is
**not rule-barred**: `weles-design.md:15` ("Native OS processes. No containers")
scopes what weles *orchestrates* and what the deploy artifact *is*; CLAUDE.md:487
("no Docker/testcontainers") scopes the *test DB provisioning strategy*. A VM
running devctl against a Postgres inside it satisfies both literally.

It is rejected as **the port** because it proves the wrong thing: `weles-design.md:205`
wants a *mixed-platform* fleet — a Linux agent on Mac hardware is still a Linux
agent. It remains legitimate as a **stopgap** for running verifyctl on this Mac
before Step 9 lands, and is the only option that unblocks that today. Recorded, not
adopted.

### Verified state (compiler and live probes, not inference)

Toolchain installed this session: rustc/cargo 1.97.1 `aarch64-apple-darwin`;
`~/.cargo/bin` added to `~/.zshrc`. The repo pins no toolchain (`rust-toolchain.toml`
absent).

`cargo check --workspace --all-targets` on darwin: **two crates have their own errors;
four more fail to build downstream of processctl.** ("The workspace compiles except two
crates" would be false — `splitproof`, `edgeca`, `verifyctl`, and `devctl` all depend on
`processctl` and build nowhere on darwin until it does.)

| Crate | Errors | Where |
| --- | --- | --- |
| `processctl` | 14 (15 incl. the rollup line) | all in `lock.rs` — 8 helpers + `OwnedChild::spawn_with_input` have `linux`+`windows` arms and no darwin arm |
| `weles` | 1 | `control.rs:103` calls 5-arg `serve`; the `cfg(not(any(windows, target_os="linux")))` arm at `:897` declares 4 |
| `devctl`, `verifyctl`, `splitproof`, `edgeca` | — | no errors of their own; blocked on `processctl` (`tools/edgeca/Cargo.toml:14` et al.) |

`core/`, `modules/`, `api/`, `cmd/`, `demos/` are platform-clean: **two** cfg gates
in the entire backend (`core/app/src/lib.rs:1418/1441`, a legitimate unix/windows
`shutdown_signal` pair). All platform mass is in `tools/` + `weles/`: processctl 122
gates, weles 36, devctl ~20, verifyctl ~9, splitproof ~4, edgeca 3. No `build.rs`
anywhere. No `nix`/`rustix`/`io-uring`.

Live probes on this machine: Postgres 18 **running** (Homebrew service, listening on
5432, socket `/tmp/.s.PGSQL.5432`) but role/db `gamebackend` **absent** — no
provisioning script exists in the repo; `lukasz` is superuser. `max_connections=100`,
exactly what `processctl/src/fleet.rs:45`'s `PG_SESSION_BUDGET` assumes. `ring` builds
(5 build-script dirs present); `aws-lc-rs` absent from `Cargo.lock` — the pin holds on
darwin. Xcode + CLT present. `ulimit -n` = 1048576 (the classic 256 default does not
apply). `dotnet` 9.0.301 arm64 present. Absent: `cargo-audit` (blocking stage),
nightly, `cargo-fuzz`, `cargo-mutants`.

Scripts are already macOS-clean: `install.sh` and `scripts/memory-sync.sh` use no
`sed -i`, `readlink -f`, `mktemp`, `date -d`, `grep -P`, or bash-4 constructs. They
only lack the git exec bit (mode `100644`), hence `bash scripts/…`.

### What the compiler cannot see — three traps

1. **`edgeca::atomic_replace`** (`tools/edgeca/src/lib.rs:78`) compiles on darwin and
   returns `Unsupported` at runtime. It mints the internal mTLS CA, so the BLOCKING
   `weles-managed-gateway` stage (`weles_managed_gateway.rs:250` → `weles/src/prep.rs:600`
   → spawns the `edgeca` binary) FAILs. Invisible to `cargo check`. `splitproof` is
   unaffected — it mints in-process via `edge::DevCA::generate()` (`main.rs:562`).
2. **The C# stage green-SKIPs.** `System.Net.Quic` needs msquic, which does not ship
   for macOS. `clients/csharp/Program.cs:44` exits 3; `csharp.rs:214` maps `c1`+exit-3
   to `Skip(SkipReason::NotApplicablePlatform)`; `model.rs:104` scores that
   `=> false` in `failed()` — **never fails, regardless of `--strict`, regardless of
   stage class**. A green SKIP wearing a PASS, the exact class CLAUDE.md and
   `weles_managed_gateway.rs:34` name.
3. **DB tests self-skip.** Every `modules/*/src/tests.rs` and `core/asyncevents/*`
   hides DB tests behind `eprintln!("SKIP: postgres unreachable"); return;`. With no
   `gamebackend` role, `cargo test` on this Mac is green while proving nothing about
   the DB layer. Platform-agnostic convention, but it bites *now*.

### The two guarantees macOS cannot match (converged independently by 3 agents)

| Linux mechanism | Purpose | darwin |
| --- | --- | --- |
| `PR_SET_PDEATHSIG` (`guardian.rs:86`) | guardian is SIGKILLed → target dies | **no equivalent, unsynthesizable** — kqueue-on-parent needs a live watcher, and the dead guardian *was* the watcher. Target-side cooperation is out (targets include `cargo`). |
| `PR_SET_CHILD_SUBREAPER` (`guardian.rs:59`) | adopt descendants that escaped the process group | **no equivalent** — an escapee that `setsid()`s reparents to launchd; unreachable by `kill(-pgid)`, invisible to `proc_listchildpids` without adoption |

Both are **backstops**. The primary paths keep full strength: `setpgid(0,0)` +
`kill(-pgid)` reaches the whole fleet, and the liveness-pipe EOF still catches
*supervisor* death. Concrete cost to name: without the subreaper, the
`forced_remainder` bit (`Frame::Completion` → `completion_forced_remainder` →
`ShutdownOutcome::Forced`) is less accurate, and splitproof's `[W2]` asserts
"clean drain, **no force-kill**" — so `[W2]` is quietly weaker on macOS unless
declared. Windows already hardcodes `completion_forced_remainder → false`
(`windows.rs:278`), so a darwin arm doing the same has precedent.

Everything else ports, several pieces more cleanly than Linux: `EVFILT_SIGNAL` >
`signalfd`; one kqueue replaces the 3-fd `poll()` loop (`guardian.rs:205`);
`proc_listchildpids` ≈ `/proc/self/task/<pid>/children`; `proc_pidpath` +
`proc_pidinfo(PROC_PIDTBSDINFO)` → `pbi_start_tvsec/tvusec` replace `/proc/<pid>/exe`
and `/proc/<pid>/stat` field 22. Every symbol verified present in the pinned
`libc-0.2.186` apple module. `libc::kinfo_proc` is **absent** from that module — the
sysctl `KERN_PROC_PID` route is closed; libproc is the door.

**The PID-reuse invariant survives untouched.** It was never pidfd-derived:
`pidfd_open` itself takes a pid (`linux.rs:164`) and `kill(-pgid)` is pid-derived
(`guardian.rs:242`). The real race-freedom is POSIX zombie pinning — an unreaped
child's pid cannot be recycled — which macOS has in full. `EVFILT_PROC` registration
therefore has no misidentification window: between spawn and `EV_ADD` the target is
alive or a zombie, and `EV_ADD` on an exited pid returns `ESRCH` (an "already exited"
signal, never a wrong-process signal).

### Why not extend an existing seam instead of adding `platform/darwin.rs`

The repo has two seams, and they are a **layering, not a conflict**:
`weles/src/platform/mod.rs:16` gates `cfg(unix)`/`cfg(windows)` because its
containment genuinely *is* POSIX (`setpgid`, `kill(-pid)`, `waitid`);
`processctl/src/platform/mod.rs:1` gates `linux`/`windows` because its containment
genuinely *is* Linux (guardian + pidfd). weles already put a darwin arm *inside*
`unix.rs` (`platform/unix.rs:158`, `cfg(all(unix, not(target_os="linux")))` for the
`si_pid` field). So the codebase's own style says: widen where the mechanism does not
differ (`lock.rs`, `state.rs`, `process.rs` — POSIX modulo 3 call sites), and write a
real backend where it does (`platform/darwin.rs` + guardian). This plan does both, at
the layer each fits.

### Pre-existing defects this research surfaced (fix, don't inherit)

- **`guardian.rs:245-250` breaks its own invariant on Linux today**: it calls
  `target.wait()` — releasing the pid — and *then* `kill(-target_pid, SIGKILL)` at
  `:248`, outside the zombie-pinning guarantee. Narrow (needs a recycled pid that is
  also a group leader) but real. Step 8.
- **The docs' ownership claim is overstated on Linux**, independent of this port:
  "no path ever falls back to a PID or name lookup" vs `pidfd_open(pid)` and
  `kill(-pgid)`. Errata in Step 1.
- **`clippy -D warnings` fails on darwin** — `process.rs:3` unused `Write` import,
  `fleet.rs:166` unused `mut` (the `cfg(windows)` `append_msvc_linker_path` is
  stripped). Both fire on *any* non-Windows target, proving this blocking stage has
  only ever run on Windows. Step 4.
- **`TMPDIR` is absent from `fleet.rs`'s `BUILD_ENV_ALLOWLIST`/`ENV_ALLOWLIST`**
  (`:8`, `:16`) — devctl freezes the env it hands cargo, and macOS's canonical temp
  var is unrepresented (`TEMP`/`TMP` are Windows and unset here). Step 6.
- **`weles`'s missing 5th arg is the stop authority**, not a cosmetic param:
  `fleet_stop: &AtomicBool` is threaded to `response()` (`control.rs:294`), whose
  `"down"` arm (`:303`) is the *only* place allowed to store into it — a named
  invariant at `control.rs:69`. Step 9 implements the arm, not the signature.
- **`AGENTS.md` has drifted from `CLAUDE.md`** on exactly the platform claim
  (`AGENTS.md:395` lacks the caveat `CLAUDE.md:415` carries) — created by this
  session's earlier CLAUDE.md-only edit. Step 1.
- **`docs/reference/platform-notes.md` contains two proven falsehoods of mine**
  (`:14`/`:26` "weles builds and supervises natively on macOS"; `:94` the `1<<63`
  offset "is shared by both implementations on both platforms"). The lock offset is
  **Windows-only** — Linux/darwin use whole-file `flock` (`lock.rs:857`) and the
  offset exists solely because `LockFileEx` is mandatory byte-range and metadata
  lives at offset 0 (`lock.rs:952`). Step 1.

### The bit-rot mechanism, and the only tripwire this repo permits

**There is no CI.** `README.md:208` makes it policy: "No CI: `cargo run -p verifyctl
-- --fast` is the safety net, run locally before every push." That is *why* weles's
non-linux fallback rotted to the wrong arity — nothing compiles it. verifyctl has no
cross-target check (`--target` appears only in `weles_async_island.rs:78` for feature
resolution, not typeck), so it would never have caught the E0061.

The mold exists: `weles_async_island.rs` is a BLOCKING, cargo-driven, DB-free stage
guarding a cross-cutting claim, and it already carries both house habits — a
**positive control** (`:84`: "if cargo's feature rendering ever changes, the bans
below would match nothing and pass forever") and **no green-on-broken-tooling**
(`:186`, citing the `b78444f` cargo-audit scar). `archcheck`'s curated-const + loud
fail is the same shape. Step 12 follows it.

---

## Steps

### Step 1 — Tell the truth in the docs first `[sonnet]` + `[user approval]`

**(a) What.** `docs/reference/platform-notes.md`: delete the weles-builds-on-macOS
claim (`:12-14` table row, `:24-27` prose) and the `1<<63`-is-cross-platform claim
(`:92-96`); replace with the verified state (weles fails to compile, 1 error at
`control.rs:103`; the offset is Windows-only because `LockFileEx` is mandatory
byte-range while unix uses whole-file `flock`). Add an errata line naming both as
corrections. `AGENTS.md:395-409`: port CLAUDE.md's caveat + platform-notes pointer
verbatim. `README.md:159`: note that "Unix" excludes macOS for rollouts *today*, with
a pointer. `CLAUDE.md:459`: drop "psql is REQUIRED" for split-proof — false; the
harness is sqlx-only (`splitproof/src/main.rs:8` states "No `psql.exe`"), what is
required is a reachable Postgres with the `gamebackend` role. Add the Linux ownership
errata (pidfd_open takes a pid; `kill(-pgid)` is pid-derived).

**(b) Why now.** Every later step is judged against these docs; a plan executing off
a document with two known falsehoods repeats the failure that produced them. Cheap,
zero code risk, and it makes the AGENTS/CLAUDE drift visible before more edits widen
it.

**(c) Split the lane (review finding).** `platform-notes.md` and `README.md` are
ordinary doc edits → `[sonnet]`. `CLAUDE.md` and `AGENTS.md` are the **governing rules
documents**, and "drop 'psql is REQUIRED'" deletes a stated requirement from them —
a `[sonnet]` subagent must not do that silently, however well-evidenced. Those two
files' diffs go to the user for approval as a separate unit. (The psql claim *is*
false — `splitproof/src/main.rs:8` states "No `psql.exe`" and no verify stage invokes
it; what is genuinely required is a reachable Postgres with the `gamebackend` role.)

**(c2) How.** `docs-current` is BLOCKING and scans `README.md`, `CLAUDE.md`,
`AGENTS.md` + all of `docs/reference/**.md`: every `[text](path)` must resolve; no
literal `./run.sh`/`./verify.sh` token inside a fence or standalone backtick span; no
`-p <name>` that is not a real workspace package; and `docs/README.md`'s
`## Current reference` section must keep ≥1 live local link whose target's first
non-empty line is not an `> **ARCHIVED` marker. Do not mark platform-notes.md
archived — it is linked from that section, which would trip the tripwire.

**(d) Verify.** Re-run the manual link/token check; the stage itself runs in Step 14.

### Step 2 — Provision Postgres and give the repo a seed script `[sonnet]`

**(a) What.** New `scripts/db-provision.sh` (+ `.ps1` twin, matching the repo's paired
convention): create login role `gamebackend` password `gamebackend`, create database
`gamebackend` owned by it, idempotent (`DO $$ … IF NOT EXISTS`). Document it in
CLAUDE.md's Database section.

**(b) Why now.** Without it `cargo test` is green-by-skipping (trap 3) — so no later
step's test evidence means anything. Independent of every code step, so it goes early.

**(c) How.** CLAUDE.md's "wipe is the migration strategy" says the answer to missing
dev data is a **seed script, not a migration** — this is that script, and its absence
is why a fresh machine has no documented path. Connect as `lukasz` (superuser here);
take the DSN from `DATABASE_URL` with the CLAUDE.md default as fallback. No exec bit
(repo convention) — invoke via `bash scripts/db-provision.sh`.

**(d) Verify.** `psql` as `gamebackend` connects; then `cargo test -p asyncevents`
prints no "SKIP: postgres unreachable".

### Step 3 — Widen the `libc` dependency gate `[sonnet]`

**(a) What.** `tools/processctl/Cargo.toml:26`, `tools/devctl/Cargo.toml:29`,
`tools/verifyctl/Cargo.toml:29`: `[target.'cfg(target_os = "linux")'.dependencies]`
→ `[target.'cfg(unix)'.dependencies]`.

**(b) Why now.** Hard prerequisite: on darwin these crates have no `libc` at all, so
every later darwin arm fails to resolve the crate before its own logic is judged.
Nothing else can start.

**(c) How.** Precedent is in-repo: `weles/Cargo.toml:55` already declares `libc` under
`cfg(unix)`. Mechanical.

**(d) Verify.** `cargo tree -p processctl -i libc --target aarch64-apple-darwin`
resolves.

### Step 4 — POSIX-widen `lock.rs`, `state.rs`, `process.rs` `[opus]` (`core-implementer`)

**(a) What.** `tools/processctl/src/lock.rs`: widen to `cfg(unix)` — `open_lock_file`
(`:730`), `validate_private_regular_linux` (`:776`), `sync_parent_directory_linux`
(`:794`), `try_lock_exclusive` (`:854`), `unlock` (`:916`), `flush_file` (`:961`),
`cleanup_consumption_marker` (`:379`), `create_consumption_marker` (`:1164`),
`consume_credential_stdin` (`:1047`), `inherited_credential_present` (`:575`, and
delete its `:609` "supports only Windows and Linux" fallback). Rename `*_linux` →
`*_posix`. `state.rs`: same widening — it has **zero** Linux-only primitives across
~1006 lines (`write_linux`, `validate_private_regular_linux`, `open_directory_linux`,
`open_state_for_read`; delete the `:386`/`:642` Unsupported arms). `process.rs`: widen
`spawn_with_input` (`:226`) and the `PlatformChild` gates; fix the unused
`use std::io::Write as _` (`:3`). New darwin arm for `credential_pipe` (`:983`).
Widen `lock_tests.rs:250/262` and `state_tests.rs:174/193/369` to `cfg(unix)`; widen
`protocol_tests.rs` (gated Linux-only at `lib.rs:14` while testing a platform-neutral
codec).

**(b) Why now.** ~2261 lines of POSIX are gated Linux for no reason the code exhibits;
widening them shrinks the real port to `platform/` + guardian and makes Step 5's
prototype the only open question. Must follow Step 3 (`libc`).

**Dependency correction (review finding — the first draft had this inverted).**
This step does **not** precede Step 6; it is **blocked by it**. `InheritedInput` is
defined at `platform/mod.rs:19`, *inside* the platform module, and `lib.rs:11` gates
`mod platform` on `#[cfg(any(windows, target_os = "linux"))]` — so on darwin the
module does not exist and neither does the type. Both `lock.rs:984`
(`credential_pipe() -> Result<(crate::platform::InheritedInput, File), _>`) and
`process.rs:226` (`spawn_with_input` → `crate::platform::spawn`) reach into it.
Therefore **Steps 4, 5, 6 and 7 land as one rollout**, in that internal order, and
`lib.rs:11`'s gate is Step 4's problem to widen — not Step 6's. Step 4 alone cannot
produce a compiling crate, so it cannot produce a test result.

**(c) How.** `credential_pipe` is the **only** genuine darwin variant: `pipe2` is
absent from libc's apple module (it exists for linux_like/freebsdlike/netbsdlike/
solarish/hurd/redox/cygwin/fuchsia — not apple), and `F_SETPIPE_SZ` is Linux-only.
Use `pipe()` + `fcntl(F_SETFD, FD_CLOEXEC)` on **both** ends and drop the 4096 sizing:
it is a resource bound, not a handshake — the credential is a few hundred bytes of
`LockMetadata` JSON and the real size bound is enforced independently at `:285`/`:1156`
(`MAX_CREDENTIAL_BYTES`). Name the cost in a comment: `pipe()`+`fcntl` is **not
atomic** the way `pipe2(O_CLOEXEC)` is, and `deliver_credential` (`:340`) does spawn a
thread, so a concurrent fork/exec can leak the fd; weles serializes spawns via
`SPAWN_LOCK` (`weles/src/platform/mod.rs:85`), processctl does not. Do **not** switch
the lock primitive: the design depends on per-fd ownership — `is_locked_by_other`
(`:907`) probes by taking the lock on a *second* fd and `lock_tests.rs:11/23` require
a second same-process `acquire` to return `AlreadyOwned`; under per-process `fcntl`
locks that probe would succeed and closing it would release the owner's lock.
`flock` is immune on both Linux and darwin (verified on this Mac/APFS: second fd gets
EWOULDBLOCK 35; the owner's lock survives the probe's close). macOS's lack of
`F_OFD_SETLK` is therefore irrelevant — no fcntl locks are used.

**(d) Verify.** `cargo check -p processctl` on darwin, asserting the **expected
residual error set**: only unresolved `crate::platform::{spawn, InheritedInput}` paths
remain — every `lock.rs`/`state.rs` cfg error is gone. Nothing else. **No test
evidence at this step**: the crate does not compile until Step 6, and a crate that
does not compile runs no tests. The `lock_tests`/`state_tests`/`protocol_tests`
evidence moves to Step 6(d).

### Step 5 — Prototype the exec-boundary identity handshake, then decide `[opus]` (`core-implementer`)

**(a) What.** A throwaway probe **outside the repo** (scratchpad) answering exactly
three questions, each with a runnable result: (1) does `posix_spawn` with
`POSIX_SPAWN_START_SUSPENDED` (0x0080) + `POSIX_SPAWN_SETPGROUP` +
`POSIX_SPAWN_CLOEXEC_DEFAULT` (0x4000) let a parent observe `(pid, exe, start-time)`
*before* the child runs, and resume it? (2) does `proc_pidinfo(PROC_PIDTBSDINFO)` read
back `pbi_start_tvsec`/`pbi_start_tvusec` on a **zombie** — required for the post-exit
identity re-check? (3) does the argv0 subtlety survive: `guardian.rs:51` documents
that the cargo→rustup shim dispatches on argv[0]'s basename, and `posix_spawn` takes
argv explicitly.

**(b) Why now.** It decides Step 6's and Step 7's shape. On Linux the handshake is
`PTRACE_TRACEME` + `waitpid(WUNTRACED)` for a SIGTRAP-at-exec
(`guardian.rs:100-124,169-189`). On darwin `PT_TRACE_ME`/`PT_DETACH` exist but ptrace
is SIP/hardened-runtime-crippled with no `PTRACE_EVENT` semantics; our targets are
unsigned cargo output so it *probably* works — and "probably" is what
"don't half-build it" warns against.

**What the handshake actually buys — and what it does not (review finding).** The
first draft called this the port's "single point of failure" while the Context argues
pid identity is safe *independent* of any handshake. Both cannot be true, and the
Context is right. The two guarantees are distinct:
- **pid stability** — that the pid we hold is still *our* child. Comes from POSIX
  zombie pinning, needs no handshake, transfers to darwin whole.
- **post-exec image identity** — that `proc_start_marker` reads the *target* and not
  the rustup shim `cargo` exec'd through. This, and only this, is what
  `wait_for_exec_trap` (`guardian.rs:169-189`) buys.

So a Step 5 failure is **not** a port-stopper. It degrades one guarantee, and the
plan carries a named plan B rather than gating the port behind an experiment.

**(b2) Plan B, written out (not "figure it out").** If neither
`POSIX_SPAWN_START_SUSPENDED` nor darwin ptrace can stop the child at the exec
boundary: spawn normally via `std::process::Command`, hold the target unreaped, and
read `proc_pidpath` + `PROC_PIDTBSDINFO` immediately after spawn. The cost is a
bounded window in which the marker may describe the pre-exec image (the shim) rather
than the target. Mitigation, concrete: re-read the identity once after the first
`try_wait` observes the target still alive, and treat a mismatch between the two reads
as "exec happened between them" — take the second. Record the residual window as a
named degradation beside the other two. This is strictly weaker than Linux's trap and
must be labelled as such, not silently adopted.

**(c) How.** `POSIX_SPAWN_START_SUSPENDED` is how debuggers launch on macOS and is
confirmed in the pinned libc. Its cost, which the prototype must price honestly:
it **replaces `std::process::Command` wholesale** for the guardian's target spawn —
stdio via `posix_spawn_file_actions_t`, `env_clear` semantics by hand, `setpgid` via
`POSIX_SPAWN_SETPGROUP`, argv0 set explicitly. Write the probe to spawn a real
suspended child, read its identity, resume it, let it exit, then re-read the identity
as a zombie. If (2) fails, the fallback is capturing the start marker *before* resume
and caching it — record which was chosen and why.

Add a fourth question, which Step 7 otherwise defers here without asking: does
`POSIX_SPAWN_CLOEXEC_DEFAULT` (0x4000) make `close_unrelated_fds` (`guardian.rs:255`,
`/proc/self/fd`) unnecessary, or does darwin still need a `/dev/fd` sweep?

**(d) Decision gate.** Output is a one-page finding appended to this plan as an
erratum, naming the chosen spawn mechanism and whether plan B was taken. Do **not**
put an LOC number on Step 6/7 until this lands.

**(e) Verify.** The probe runs on this Mac and prints the four answers. Nothing is
committed to the repo except the erratum.

### Step 6 — `platform/darwin.rs` `[opus]` (`core-implementer`)

**(a) What.** New `tools/processctl/src/platform/darwin.rs` implementing the contract
`platform/mod.rs` declares: `spawn(&SpawnSpec, Option<InheritedInput>) -> Result<(PlatformChild, ProcessIdentity), ProcessError>`,
`PlatformChild::{try_wait, graceful, force, completion_forced_remainder}`,
`observe_process_identity(u32) -> io::Result<ProcessIdentity>`. Wire it in
`platform/mod.rs:1-17` and `lib.rs:7-15`. Delete the now-dead
`not(any(windows, target_os="linux"))` stubs in `process.rs:126-163`.
Add `TMPDIR` to `fleet.rs`'s `BUILD_ENV_ALLOWLIST` (`:8`) and `ENV_ALLOWLIST` (`:16`);
fix the `fleet.rs:166` unused `mut`.

**(b) Why now.** `lock.rs`'s `OwnerNotLive` authority (`:213`, `:657`) calls
`observe_process_identity`, so Step 4 compiles but stays inert until this lands.
Depends on Step 5's decision.

**(c) How.** `observe_process_identity`: `proc_pidpath` (libc apple `:4923`) for the
executable, `proc_pidinfo(PROC_PIDTBSDINFO)` → `pbi_start_tvsec`/`pbi_start_tvusec`
(`:606,627`) for the start marker. **`StartMarker(u64)` is opaque outside the platform
module** — it is `serde`-persisted and never compared across platforms, so darwin may
pack the two tv fields freely. Signalling: plain `kill(pid, sig)` — as safe as
`pidfd_send_signal` because the guardian is held as an unreaped `Child`, and a zombie
pins its pid.

**`completion_forced_remainder` — corrected (review finding).** The first draft said
"return `false`, following `windows.rs:278`". That is a symptom patch and the Windows
precedent is not analogous: Windows returns `false` because Job Object teardown has no
"remainder" concept at all, whereas darwin **can compute half of it**. Of
`guardian.rs:249`'s `forced_group || forced_adopted`, the `forced_group` half is a
plain `kill(-pgid)` probe and works on darwin verbatim; only `forced_adopted`
(`reap_descendants`, the `PR_SET_CHILD_SUBREAPER` half) is unavailable. **Darwin
returns `forced_group`.** Hardcoding `false` would discard the working half and turn
that branch of `[W2]` into a tautology — the exact failure mode this plan exists to
avoid.

**(d) Verify.** `cargo check -p processctl` clean on darwin. Port the 4
`linux_tests.rs` codec tests (`read_handshake`/`read_completion`, guardian-failure
discrimination, raw wait-status fidelity incl. SIGKILL, truncated-frame `UnexpectedEof`)
into a shared unix test module — they are platform-neutral and gated Linux-only by
accident.

### Step 7 — The darwin guardian `[opus]` (`core-implementer`)

**(a) What.** Darwin arms for `tools/processctl/src/guardian.rs`'s four guarantees.
`lib.rs:7` currently gates the whole 401-line module `cfg(target_os="linux")`.

**(b) Why now.** This is the port's centre of gravity — the real containment lives
here, not in `platform/linux.rs` (a ~80-line handle). A port scoped to `platform/`
would miss ~80% of the Linux surface. Depends on Steps 5 and 6.

**(c) How.** Collapse the 3-fd `poll()` loop (`:204-252`) into **one kqueue**:
`EVFILT_READ`+`EV_EOF` on the liveness pipe, `EVFILT_PROC`/`NOTE_EXIT` on the target,
`EVFILT_SIGNAL` (`:3165`) replacing `signalfd` (`:298`) — the existing `sigprocmask`
block (`:283`) stays. `close_unrelated_fds` (`:255`, `/proc/self/fd`) → `/dev/fd`, or
delete it entirely if Step 5 adopts `POSIX_SPAWN_CLOEXEC_DEFAULT`.
`kill_direct_children` (`:383`, `/proc/self/task/<pid>/children`) → `proc_listchildpids`
(`:4901`). The reap-then-kill defect at `:245-250` is **NOT fixed here** — see Step 7b;
a one-line reorder in this step would regress `[W2]` on Linux.
**Record the two degradations** in the module doc, not in a commit message only:
guardian-SIGKILL orphans the target (no `PR_SET_PDEATHSIG`), and a `setsid()` escapee
reparents to launchd unreachable (no `PR_SET_CHILD_SUBREAPER`) — the latter is exactly
the `forced_adopted` half Step 6 drops from `completion_forced_remainder`.

**(d) Verify.** The behaviours with no existing coverage — `linux_tests.rs` is 4 codec
tests and spawns nothing. Write, on darwin: supervisor-death → target dies (liveness
EOF); `kill(-pgid)` kills the whole tree; an unrelated decoy process **survives**;
graceful signal forwards. Prove the reorder fix by asserting the group kill lands
while the target is still unreaped.

### Step 7b — The reap-then-kill defect, fixed at the authority `[opus]` (`core-implementer`)

**(a) What.** `tools/processctl/src/guardian.rs:245-250`. Today:
`let status = target.wait()?;` then
`let forced_group = kill(-target_pid, SIGKILL) == 0;` — the pid is released by the
reap, so the kill is outside zombie pinning. A recycled pid that is also a group
leader would be signalled.

**(b) Why now.** Its own step, after Step 7, because it is a **Linux behaviour change**
that the darwin port merely surfaced — not a "while here" edit. It must not ride along
with a port commit, and it must not land before the darwin guardian exists, or the two
changes cannot be told apart when `[W2]` moves.

**(c) How — and why the obvious fix is wrong.** The obvious reorder (kill the group
*before* the reap) **regresses `[W2]` on every Linux run**, and the review caught this
in the first draft. The target is its own group leader (`guardian.rs:83`,
`setpgid(0, 0)`); a zombie is still a group member and a valid signal target, so
`kill(-pgid)` after an unreaped target returns 0 **unconditionally** → `forced_group`
always true → `Frame::Completion` carries `forced_remainder = true` → `linux.rs:124` →
`process.rs:274` → `ShutdownOutcome::Forced` → `splitproof/src/main.rs:409` scores
`clean = false`. The kill-after-reap is deliberate: the comment at `:246` says it
asks *"is anything LEFT in the group now the target is gone"*.

So the authority in question is **what `forced_remainder` means**, not statement
order. Fix: enumerate group membership *before* the reap (`proc_listchildpids` on
darwin / `/proc/self/task/<pid>/children` on Linux, both already used at `:383`) and
derive `forced_group` from that enumeration minus the target itself, then reap, then
kill. `kill`'s return value stops being the remainder oracle — which is what made the
ordering load-bearing in the first place. Record the semantics change in the commit
and as an erratum here.

**(d) Verify.** A test that pins the branch that was wrong: a target that leaves **no**
descendants must yield `forced_remainder == false` (today's behaviour, and the one a
naive reorder breaks), and a target that leaves a live group member must yield `true`.
Run both on Linux and darwin. Then re-run splitproof `[W2]` on Linux and confirm it
still scores `clean` — this step's whole risk is that it does not.

### Step 8 — devctl darwin `[opus]`

**(a) What.** `tools/devctl/src/supervisor.rs:764` (`control_endpoint`) and `:793`
(`install_signal_handler`): widen `cfg(target_os="linux")` → `cfg(unix)`, bodies
unchanged (`:793` is plain `libc::signal(SIGINT/SIGTERM)`). `tools/devctl/src/control.rs:202`
(`serve`) and `:262` (`request_raw`): replace `SO_PEERCRED`/`libc::ucred` — **both
absent from libc apple**. Narrow the `:517`/`:529` fallbacks.

**(b) Why now.** Depends on Steps 3/6. Until `:793` is widened, `devctl up` dies at
`supervisor.rs:805` *before* acquiring the rollout lock — fail-closed, so it is loud,
not subtle.

**(c) How.** The peer-cred substitution is the only non-mechanical part and is the
trap for pattern-matching Linux: macOS splits what `ucred` bundles, so it takes **two**
`getsockopt` calls at `level = SOL_LOCAL` (not `SOL_SOCKET`) — `LOCAL_PEERCRED` yields
`xucred` (fields `cr_version`, `cr_uid`, `cr_ngroups`, `cr_groups` — **no pid**), and
`LOCAL_PEERPID` yields the `pid_t` separately. Assert `cr_version == XUCRED_VERSION`
(== 0) or the struct is meaningless. `serve` gates on uid only; `request_raw` pins
**both** uid and pid (`:290`) — the anti-reused-pid guard, so it needs both calls.
Verified working on this Mac in both directions (uid=501, pid=45957 observed from each
side of a real UDS).

**(d) Verify.** devctl's `tests.rs` cfgs are **all 8 `cfg(windows)`** — there is no
unix control-endpoint test at all, including the wrong-supervisor rejection
(`:235`). That rejection is precisely the branch this step newly implements, so add a
unix mirror asserting a *wrong* pid/uid is refused. Then `devctl up monolith` on this
Mac, `devctl status`, `devctl down`.

### Step 9 — weles darwin control endpoint `[opus]`

**(a) What.** `weles/src/control.rs`: real darwin `serve` (`:537` linux arm is the
model) and `request_raw` (`:612`), replacing the rotted 4-arg stub at `:896-916`.
`weles/src/supervisor.rs:905` (`control_endpoint_path`) and `:915`: widen to
`cfg(unix)`, drop `"unsupported-control"`. Widen `control_tests.rs:78/87/109/152/189/487`
from `any(windows, target_os="linux")` to include darwin.

**(b) Why now.** Closes the gap `weles-design.md:490` recorded. Independent of
processctl (weles shares no crates), so it can run in parallel with Steps 6-8.

**(c) How.** **Fixing the arity alone is a dead end, twice over**: `supervisor.rs:767`
binds the endpoint *before* boot and treats `Err` as fatal (teardown + return), so a
stub makes `weles up` die before spawning a single service; and with both transport
fns stubbed the whole protocol goes dead-code — 13 lib + 8 test `clippy` warnings,
failing the blocking `-D warnings` stage. The transport itself is portable verbatim:
Linux already uses a **filesystem-path UDS**, not an abstract socket — `serve:548`
does `remove_file` → `UnixListener::bind` → `set_permissions(0o600)`, and
`request_raw:617` validates `symlink_metadata` (not a symlink, `uid == geteuid()`,
mode `0o600`). Only the peer check is Linux-bound: same `LOCAL_PEERCRED` + `LOCAL_PEERPID`
substitution as Step 8, and `weles-design.md:493` already prescribes exactly this
("a native macOS UDS peer-cred implementation, not weakening local auth everywhere").
The missing `fleet_stop: &AtomicBool` is the **stop authority** (`control.rs:69`
names the invariant; `response():303`'s `"down"` arm is its only legal writer) — so
implement that arm, do not just add a parameter. Bound-check the endpoint path:
darwin's `sun_path` is **104** bytes vs Linux's 108, and
`<root>/run/weles/control-<16hex>.sock` is `len(root)+40` (~69 here) — a deeply
nested checkout would fail at `bind`, so fail with a legible error rather than an
opaque one.

**(d) Verify.** `control_tests.rs:189`'s `bind_failure_…never_sets_the_fleet_stop`
must pass on darwin — it pins the exact invariant the rot dropped. Then `weles deploy`
+ `weles up split`, `weles status`, `weles down` on this Mac.

### Step 10 — verifyctl + edgeca darwin `[sonnet]`

**(a) What.** `tools/verifyctl/src/runner.rs:455`: widen `install_interrupt_handler`
to `cfg(unix)` (body is pure POSIX `libc::signal(SIGINT)`); narrow the `:492` bail.
`tools/edgeca/src/lib.rs:73`: widen `atomic_replace` to `cfg(unix)` (the Linux arm is
a plain `std::fs::rename`, atomic on APFS); delete the `:78` Unsupported arm.

**(b) Why now.** `runner.rs:69` calls `install_interrupt_handler()?` unconditionally
at the top of the run, so verifyctl refuses to start on macOS — nothing in Step 14 can
run until this lands. edgeca is the invisible one: it compiles and fails at runtime,
taking the BLOCKING `weles-managed-gateway` stage with it.

**(c) How.** Mechanical, given Step 3's `libc` gate. Both are `linux`→`unix` on bodies
that are already portable.

**(d) Verify.** `cargo check -p verifyctl -p edgeca` on darwin — **compile evidence
only.** Both crates depend on `processctl` (`tools/edgeca/Cargo.toml:14`), so neither
runs until Steps 4-7 land; the first draft's "verifyctl reaches stage one" and "edgeca
mints a CA" were unreachable at this step. That runtime evidence moves to Step 14.

### Step 11 — Close the green-SKIP hole `[opus]`

**(a) What.** `tools/verifyctl/src/model.rs:103-110`: `Skip(SkipReason::NotApplicablePlatform)`
currently scores `=> false` in `failed()` — exempt from failure for **every** stage
class, including BLOCKING, and even under `--strict`. Pinned by
`summary_exit_matrix_preserves_strict_and_platform_rules` (`:163`).
`tools/verifyctl/src/stages/csharp.rs:214`: the `c1`+exit-3 → `NotApplicablePlatform`
mapping.

**(b) Why now.** A macOS port is exactly the pressure that invites new platform
escapes, and this is the one already in the tree: on this Mac the C# stage boots a
whole monolith, hits `QuicConnection.IsSupported == false` (msquic does not ship for
macOS), and reports green having proved nothing. Fixing it *after* the port would mean
the port's own verification ran under a rule that hides platform gaps.

**(c) How — corrected (review finding).** The first draft made
`NotApplicablePlatform` fail under `--strict` for *any* class. That would **red-wall
the Windows dev box permanently**: `fuzz` is also ADVISORY (`stages/mod.rs:124`) and
`fuzz.rs:8` returns `Skip(NotApplicablePlatform)` on `cfg!(windows)` forever, with no
fix available — so `verifyctl --all --strict`, a command CLAUDE.md documents, could
never pass on Windows again. The draft checked the class of the stage it cared about
and missed its sibling in the same list.

The real authority is not the *class*, it is **who decides applicability, and when**.
`fuzz` declares it **statically and up front** — a property of (stage, platform),
auditable in the manifest. `csharp` **derives it at runtime from the exit code of the
thing under test** (`csharp.rs:214`: `label == "c1" && code == Some(3)`), after booting
a whole monolith. That is the defect: a genuine C# client bug that happened to exit 3
would be scored a platform skip. Two changes, both at the authority:

1. **Declare applicability in the STAGE table.** Add the platform-applicability of a
   stage to the `Stage` struct (`stages/mod.rs`) as data — the exemption becomes
   visible in the manifest and greppable, instead of being reachable only by reading a
   stage's `run` body. `fuzz`'s Windows exemption moves there unchanged.
2. **Delete the runtime exit-code→skip mapping** in `csharp.rs:214`. After that, exit 3
   is a FAIL like any other unexpected code. macOS then either declares the stage
   not-applicable *in the table* (visible, auditable, and green only for an ADVISORY
   stage) or the stage FAILs honestly — and either way, a real client bug exiting 3 can
   no longer hide behind the platform reason.

Keep the `model.rs` scoring change scoped to the hole that is actually structural: a
**BLOCKING** stage may never `NotApplicablePlatform`-escape (nothing does today; this
closes the door while a port is the pressure to open it). ADVISORY keeps its current
green — that is fuzz-on-Windows's legitimate use, and after change 2 it is a declared
exemption rather than a runtime discovery. **Sweep the sibling:** `model.rs:105`'s
`Skip(SkipReason::ExplicitNoInstallMissingTool) => false` and its pin at `:170` stay —
CLAUDE.md sanctions it ("only a missing tool with explicit `--no-install` is labeled
SKIP"). Name it in the code as deliberately kept, so the next reader does not
re-litigate it. Record the semantic change in the commit and as an erratum here.

**(d) Verify.** Extend `model.rs`'s matrix test rather than deleting it:
BLOCKING+`NotApplicablePlatform` → `failed()` true (the new rule);
ADVISORY+`NotApplicablePlatform` → false, default **and** under `--strict` (the
Windows-fuzz guarantee — the branch the first draft would have broken);
BLOCKING+`ExplicitNoInstallMissingTool` → false (unchanged, deliberately). Then, on
this Mac, assert the C# stage FAILs on an injected exit 3 that is not a platform
declaration.

### Step 12 — The `supported-targets` tripwire `[opus]`

**(a) What.** New BLOCKING stage `tools/verifyctl/src/stages/supported_targets.rs`
+ `StageId::SupportedTargets` in `model.rs:40` and the STAGE table (`stages/mod.rs`).
A curated `const SUPPORTED_TARGETS: &[&str]` = the three triples; run
`cargo check -p processctl -p weles --target <t>` for each — **two crates, and without
`--all-targets`.** Add `rust-toolchain.toml` pinning the channel and
`targets = [<the three triples>]`.

**(b) Why now.** Without it, darwin rots exactly as the weles fallback did — and with
no CI (`README.md:208`), a verifyctl stage is the *only* mechanism this repo has.
Must land in the same rollout as the port, or the port's own arms start rotting.

**(c) How.** Follow `weles_async_island.rs`'s mold — blocking, cargo-driven, no build,
no Postgres. Four things it must not miss; the first two are review findings that
killed the first draft's version.

**Scope: `processctl` + `weles` only — devctl is excluded, and that is a named gap.**
The draft scoped to "the rollout tools" including `devctl`, while itself naming the
hazard that disqualifies it. `cargo check --target` **runs build scripts**, and
`devctl` pulls `ring` through its normal dependencies (`cargo tree -p devctl -e normal`
→ `ring` via `edge` → `quinn` → `quinn-proto`), whose C/asm needs an Apple
cross-toolchain that a Windows box does not have. `processctl` and `weles` are
ring-free in normal deps (verified: `cargo tree -e normal` → 0 hits each) — and they
are precisely the two crates that rotted.

**No `--all-targets`.** `processctl` is ring-free only in *normal* deps; its
`[dev-dependencies]` (`asyncevents`, `invalidation`, `scheduler` — the anti-drift
mirrors) pull `ring` back in. `cargo check -p processctl --target <t>` without
`--all-targets` does not build dev-deps (verified on this Mac: 0 ring/asyncevents in
the check). The cost, stated rather than hidden: **test code is not typechecked
cross-target**, so rot inside a `cfg`-gated test module (e.g. `control_tests.rs`)
escapes this stage. Named gap, not silence.

**The positive control must be in-stage, not a one-time experiment.** The draft said
"e.g. that removing a darwin `cfg` would fail the check" — that is a mutation test for
12(d), not the house pattern it cites. `weles_async_island.rs:84` is an **always-on**
assertion that runs every invocation and fails loudly when the tool's output shape
drifts. Concrete equivalent here: run with `--message-format=json` and assert each
target produced a compiler artifact for the crate **actually compiled for that target**
(not `"fresh": true` from cache) — so a cfg typo, a silently-ignored `--target`, or a
fully-cached run cannot make the stage pass while checking nothing.

**A missing `rustup target` is a FAIL, never a green SKIP** (the `b78444f` scar).
That is only tenable if the targets are provisioned declaratively — hence
`rust-toolchain.toml` with `targets = [...]`, which rustup installs automatically. The
first draft noticed the repo pins no toolchain and did nothing with it, while building
a blocking gate whose green depended on exactly that unpinned per-machine state.

The payoff: E0061-class rot is caught **from Windows or Linux, without owning a Mac** —
`cargo check --target` typechecks the selected target's cfg arms without linking.

**(d) Verify.** Reintroduce the 4-arg `serve` in a scratch copy → the stage FAILs.
Delete `rust-toolchain.toml`'s `targets` and remove a target via `rustup target remove`
→ the stage FAILs (not SKIPs). Point the stage at a cached-clean tree → the positive
control still asserts a real compile, not a `fresh` hit.

### Step 13 — Install the missing toolchain `[sonnet]`

**(a) What.** `cargo install cargo-audit` (blocking `audit` stage). For `--all`:
`rustup toolchain install nightly`, `cargo install cargo-fuzz cargo-public-api`. For
`--slow`: `cargo install cargo-mutants`. Document the set in `platform-notes.md`.

**(b) Why now.** Step 14 needs them; verifyctl self-installs some, and knowing which
beats discovering it mid-run.

**(c) How.** Note the behaviour change to expect: `fuzz.rs:8` skips only on
`cfg!(windows)`, so **on macOS the fuzz stage actually executes** `cargo +nightly fuzz
run {frame_decode,wire_decode}` in `core/edge` — a code path that has never run in
this repo's history. `public_api.rs:178` shells `rustup toolchain install nightly`.

**(d) Verify.** `cargo audit --version` resolves; `cargo +nightly fuzz --help` works.

### Step 14 — Prove it on this Mac `[inline]`

**(a) What.** One rollout at a time: `cargo test --workspace` → `devctl up monolith`
+ smoke (`/match/report`, `/leaderboard`) → `devctl down` → `cargo run -p verifyctl -- --fast`
→ `cargo run -p verifyctl -- --all --strict`. Record the result in
`docs/reference/platform-notes.md` (replacing the corrected table from Step 1) and a
dated status doc.

**(b) Why now.** Last: it is the only step that can honestly claim "runs on macOS", and
CLAUDE.md forbids a second rollout while one is live.

**(c) How.** Check `pgrep -x cargo; pgrep -x rustc` clear before each. Expect and
record two honest non-greens rather than filing them as failures: `[W2]`'s
`forced_remainder` is weaker on darwin (Step 7's named degradation) and the C# stage
is non-green under `--strict` (Step 11's point). If `--all --strict` is green
*including* csharp, that is a bug in Step 11, not a success.

**(d) Verify.** The PASS/FAIL table itself, pasted into the status doc verbatim —
not paraphrased.

---

## Dispatch summary

| Step | Lane | Rationale |
| --- | --- | --- |
| 1 | `[sonnet]` + `[user approval]` | doc edits; CLAUDE.md/AGENTS.md diffs approved separately |
| 2, 3, 10, 13 | `[sonnet]` | mechanical: a seed script, cfg/dep widening on portable bodies, tool installs |
| 4, 5, 6, 7, 7b | `[opus]` via `core-implementer` | authority-first: the lock's per-fd invariant, the exec handshake, containment guarantees, the `forced_remainder` semantics |
| 8, 9, 11, 12 | `[opus]` | peer-cred substitution, the stop-authority invariant, a scoring-authority change, a new blocking gate |
| 14 | `[inline]` | one live rollout, judged in context |

**Steps 4-7 are one rollout, in that order** — Step 4 cannot compile without Step 6
(`lib.rs:11` gates `mod platform`, which defines `InheritedInput`). Step 7b follows
separately: it is a Linux behaviour change that must be distinguishable from the port.
Steps 8 (devctl) and 9 (weles) are independent of each other — weles imports no
workspace crate and its containment layer already builds on darwin
(`weles/src/platform/mod.rs:16`), so **`weles up` on macOS needs Step 9 only, not
Steps 6-7**. Concurrent `cargo check` serializes on cargo's target-dir lock (slow, not
broken); **test runs never overlap** — one rollout at a time.

## Review record

Reviewed at ultrathink in a separate context; verdict on the first draft was **not
executable as written**. Four findings were correctness blockers and are folded in
above, each verified against the code before acceptance: the Step 7 reorder would have
regressed `[W2]` on **Linux** (Step 7b now fixes the meaning of `forced_remainder`,
not the statement order); Step 4's dependency on Step 6 was **inverted** and its test
evidence unreachable (Steps 4-7 merged into one rollout); Step 12's tripwire included
`devctl`, whose `ring` edge makes it uncross-checkable from Windows (scoped to
processctl+weles, no `--all-targets`, gap named); and Step 11's strict rule would have
**permanently red-walled the Windows dev box** via the ADVISORY `fuzz` stage (rescoped
to who *declares* applicability, and when).

Two further corrections: `completion_forced_remainder` returns `forced_group` on darwin
rather than a hardcoded `false` (which would have made that `[W2]` branch a tautology),
and Step 5 now names what the handshake buys — post-exec **image** identity, not pid
stability — with a written-out plan B, since the Context already establishes that pid
safety comes from zombie pinning and needs no handshake.

The reviewer confirmed as sound: Step 3's prerequisite and its in-repo precedent; Step
9's independence; the deliberate absence of steps for `conformance`/`checkmodules`/
`archcheck` (zero platform cfgs); the 2-gate backend claim; and the parallelism claim.

## The one degradation `[W2]` cannot avoid

Not a research risk — a stated outcome. On darwin `forced_remainder` reports
`forced_group` but never `forced_adopted`, because `PR_SET_CHILD_SUBREAPER` has no
macOS equivalent. `[W2]`'s **primary** assertion (clean drain within 15s, exit 0, no
escalation to force) is unaffected — `process.rs:288`'s graceful-timeout→`force()` path
is platform-independent. What macOS cannot see is a descendant that `setsid()`s out of
the process group and reparents to launchd. Note the baseline: Windows already
hardcodes `completion_forced_remainder → false` (`windows.rs:278`), so darwin with
`forced_group` is **stronger** than the platform `[W2]` has always run on — the honest
framing is "weaker than Linux, stronger than Windows", not "quietly weaker".

## Step 5 erratum

**Decision: Plan A adopted — `posix_spawn` with `POSIX_SPAWN_START_SUSPENDED |
POSIX_SPAWN_SETPGROUP | POSIX_SPAWN_CLOEXEC_DEFAULT`. Plan B not taken.** Proven on
this Mac (macOS 26.5.1, aarch64) with a throwaway libc-only probe; every symbol
verified against the pinned `libc-0.2.186` apple module before use. The probe was
re-run independently by the main context — output reproduces exactly.

**Q1 (exec-boundary handshake) — confirmed.** A suspended `posix_spawn` lets the
parent read `(pid, proc_pidpath, PROC_PIDTBSDINFO start-time)` while the child is
`SSTOP`, *before its image runs* (a marker pipe the child writes on entry returns
`EAGAIN`/errno 35 pre-resume, `"STARTED"` post-`SIGCONT`), then resumes via
`kill(pid, SIGCONT)`. Exact replacement for Linux's `PTRACE_TRACEME` +
`waitpid(WUNTRACED)` SIGTRAP-at-exec (`guardian.rs:100-124,169-189`), and cleaner in
one respect: the guardian can register `EVFILT_PROC`/`NOTE_EXIT` on its kqueue
*before* `SIGCONT`, removing the spawn→watch race.

**Q2 (zombie identity read-back) — NO, and it is a non-issue (parity with Linux).**
On a confirmed unreaped zombie (`waitid(P_PID, WEXITED|WNOWAIT)`), both
`proc_pidinfo(PROC_PIDTBSDINFO)` and `proc_pidpath` fail with `ESRCH` (n=0); libproc
drops task/BSD info at exit, before reaping. Linux's `observe_process_identity`
begins with `read_link(/proc/<pid>/exe)` (`linux.rs:228`), which also fails on a
zombie (mm gone) → `OwnerNotLive`. **Correction to Step 5's original call-site claim
(main-context verify):** `observe_process_identity` has MORE than the two processctl
sites — it is also called in devctl (`supervisor.rs:163/199`, `control.rs:274/457`),
and `devctl/src/tests.rs:407/463/510/558` explicitly `assert!(...is_err())` for a
dead/reused pid. Every one of these reads a process expected *live* and treats
failure as "not live / not the same process" — so darwin's ESRCH→`OwnerNotLive` is
the required behaviour, and the darwin `observe_process_identity` (Step 6) MUST return
`Ok(identity)` for any live pid and `Err` for a dead one, so those devctl tests pass.
No path re-reads an exited target expecting success — the "post-exit re-check on a
zombie" this plan's Step 5(a)/6 anticipated does not exist and could not be built on
Linux either. `StartMarker` may pack `pbi_start_tvsec`/`pbi_start_tvusec` freely
(opaque, serde-persisted, never cross-platform compared).

**Q3 (argv0 vs exec path) — confirmed.** `posix_spawn` takes argv explicitly:
spawning the real binary with `argv[0]="cargo"` yields a child observing
`argv[0]="cargo"`. The rustup-shim basename dispatch (`guardian.rs:51-54`) survives;
darwin form of `command.arg0(original_executable)`.

**Q4 (`CLOEXEC_DEFAULT` vs the fd sweep) — confirmed, with a caveat.** The flag closes
inherited fds across the spawn (extra fd 30 `open` without it, `closed` with it), so
`close_unrelated_fds` (`guardian.rs:255`, `/proc/self/fd`) is **unnecessary on darwin
— delete it, no `/dev/fd` sweep**. Caveat: it also closes 0/1/2, so stdio (and, for
the guardian-spawn, the fd-3 liveness + fd-4 status pipes) must be re-established via
`posix_spawn_file_actions_adddup2`. Free, since Plan A already routes stdio through
file actions.

**Cost Step 6/7 must carry (named, not a guarantee loss):** Plan A replaces
`std::process::Command` wholesale for the guardian's target spawn — stdio + fd 3/4 via
`posix_spawn_file_actions_t`, envp built by hand (`env_clear` semantics), `setpgid`
via `POSIX_SPAWN_SETPGROUP`, argv0 explicit — and the guardian must `waitpid` the raw
pid itself to hold the zombie (the pid-reuse invariant, which needs no handshake).

**Plan B (two-read exec detector) — proven to work but strictly weaker, not adopted.**
A shim that execs a second binary shows `proc_pidpath` changing across the exec while
the start-marker stays constant, so two reads *can* detect an intervening exec — but
the detector has an irreducible race (both reads can land the same side of the exec)
and cannot capture the target before it runs. Superseded by Q1.

**Net:** post-exec image identity is secured on darwin at Linux-equivalent strength;
pid-reuse safety was never at stake here (zombie pinning). No new `[W2]`-class
degradation is introduced by the spawn mechanism.

## Deferred to a Linux run (must re-confirm before the port is "done" on Linux)

Steps 6/7/7b were implemented and verified on macOS; two items are structurally
unverifiable from this Mac and MUST be re-confirmed on a Linux box:

1. **splitproof `[W2]` on Linux.** Step 7b changed the Linux `forced_remainder`
   derivation (enumerate-group-before-reap). Reviewed clean on darwin + Linux
   compiles, but `[W2]` is a Linux runtime assertion — run `cargo run -p verifyctl
   -- --fast` (or splitproof directly) on Linux and confirm `[W2]` still scores
   clean (no spurious force-kill from a `forced_group` that should be false).
2. **A unit test for the Linux `/proc` pgrp parse** (`guardian.rs:495`
   `list_process_group`). The darwin `proc_listpgrppids` variant is tested; the Linux
   `/proc/<pid>/stat` field-5 parse is asserted only by code review today. Add a
   Linux-gated test feeding a synthetic stat line whose comm contains spaces and `)`
   (e.g. `1234 (weird )name) S 1 5678 ...`) and assert the extracted pgrp is `5678` —
   pins the last-`)` + `nth(2)` field index against a one-char regression that would
   compile and only surface on `[W2]`. Do this on the Linux box where it runs, not
   here (refactoring the parse to add the test would trade a review-verified path for
   a compile-only-verified one on a machine that cannot run it).

## Discovered sub-steps (8b, 9b, 9c) — the plan under-scoped test harnesses + weles's own platform

Implementing Steps 8/9 surfaced three darwin gaps the plan did not anticipate, each
fixed as its own committed step (all verified green on darwin, cross-target compiled):

- **Step 8b (`a62274c`)** — devctl's 4 supervised-child integration tests ran in the
  libtest harness, which cannot serve as a processctl guardian on the unix re-exec
  (`current_exe --__processctl-guardian-v1` → libtest exit 101). They never passed on
  ANY unix (only Windows, via Job Objects). Relocated to a `harness = false`
  `tests/supervised.rs` whose `main` dispatches the guardian first — the established
  `processctl/tests/downstream` pattern. Required adding a thin `devctl/src/lib.rs`
  (was binary-only) + widening 7 items to `pub`.
- **Step 9b (`a6d3764`)** — weles's OWN lock/platform layer (zero-shared copy of
  processctl's) had no darwin `observe_process_identity` (returned Unsupported) and
  its `sweep_group` hit EPERM. Probe finding: darwin `waitid(…WNOWAIT)` is FINE
  (non-reaping preserved); the EPERM is `kill(-pgid, SIGKILL)` on a group whose only
  member is the unreaped zombie root returning EPERM where Linux returns ESRCH — both
  mean "nothing signalable left". Fixed by mirroring processctl's darwin observe +
  tolerating EPERM in `sweep_group`. weles `--lib` 151/13 → 164/0.
- **Step 9c (`089728d`)** — weles's `force_kills_the_whole_tree` test called raw
  `force()` (which by design does not reap) then probed liveness via `kill(pid,0)`,
  which reports a zombie as alive on every unix. Fixed the TEST to reap the root via
  `try_wait` (as every production caller does); `force()`/`process_alive` untouched.
  Full weles suite green (lib 164, platform 7, prep 5, main 1).

Lesson for the remaining steps: a crate compiling on darwin does not mean its TEST
suite passes on darwin — Windows-semantics tests (Job Objects, no zombies) surface as
unix failures. Steps 10-14 should budget for the same on verifyctl if it has
process-spawning tests.
