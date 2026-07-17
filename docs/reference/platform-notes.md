# Platform notes

Everything in `CLAUDE.md` / `AGENTS.md` is written platform-neutrally: the rules
(one rollout at a time, fail-closed dev opt-ins, the fortress rule) are the same
everywhere. This file holds the per-OS specifics — the concrete command spellings
and the places where a platform actually differs in kind, not just in syntax.

## Where the workspace runs

| | Windows | Linux | macOS |
| --- | --- | --- | --- |
| Build the workspace (`cargo build`, `clippy`, unit tests) | yes | yes | yes |
| Fleet rollouts (`devctl`, `verifyctl`, `splitproof`) | yes | yes | yes |
| `weles` supervisor | yes | yes | yes |

**macOS is a first-class rollout + verification platform (as of 2026-07-17).** The
darwin port landed (plan: `docs/plans/2026-07-17-1601-macos-rollout-port-plan.md`);
`cargo run -p verifyctl -- --fast` passes **16/16 blocking stages** on Apple Silicon,
including `split-proof` (the 12-service split fleet) and `weles-managed-gateway`, and
`devctl up monolith` + the smoke flow work. Provision Postgres once
(`scripts/db-provision.sh`) and install `rustup` (the three targets self-install via
`rust-toolchain.toml`); `verifyctl` self-installs `cargo-audit`/`cargo-fuzz`/nightly.

`tools/processctl` — the owned-process containment layer every rollout tool spawns
through — now has three backends: `platform/{linux,windows,darwin}.rs`. The darwin
backend replaces Linux's guardian+pidfd with a kqueue supervisor
(`EVFILT_PROC`/`NOTE_EXIT` + `EVFILT_SIGNAL`) and a suspended `posix_spawn`
(`POSIX_SPAWN_START_SUSPENDED|SETPGROUP|CLOEXEC_DEFAULT`) for race-free identity
capture — parity with the Linux ptrace exec-trap. The PID-reuse invariant transfers
unchanged (it was always POSIX zombie pinning, never pidfd).

**Two containment backstops are structurally weaker on macOS — named, not hidden:**
`PR_SET_PDEATHSIG` has no darwin equivalent, so a SIGKILLed guardian orphans its
target; `PR_SET_CHILD_SUBREAPER` has none either, so a descendant that `setsid()`s
out of the process group escapes `kill(-pgid)`. The primary paths (group kill,
liveness-EOF on supervisor death) keep full strength; `[W2]`'s `forced_remainder`
reports `forced_group` only (stronger than Windows, which hardcodes `false`; weaker
than Linux). See the guardian module doc and the plan's Step 5/7 errata.

**Two items are proven only on Linux hardware, not from a Mac** (recorded in the plan
under "Deferred to a Linux run"): splitproof `[W2]` as a Linux *runtime* assertion,
and a unit test for the Linux `/proc` pgrp parse. Cross-target `cargo check` of
`devctl`/`verifyctl`/`edgeca` also cannot run from a Mac (they pull `ring`, needing a
cross-cc) — the `supported-targets` verify stage scopes its tripwire to the ring-free
`processctl`/`weles` for exactly this reason.

## Command equivalents

The docs quote the Unix spelling; these are the per-OS forms.

**Check for an in-flight Cargo rollout** (the mandatory pre-rollout check):

```
pgrep -x cargo; pgrep -x rustc                                        # Unix
Get-Process | Where-Object { $_.ProcessName -match '^cargo$|^rustc$' } # PowerShell
```

**Inspect/stop a foreground `devctl` fleet** without starting a second Cargo
process — use the already-built binary (under `CARGO_TARGET_DIR` when it differs):

```
target/debug/devctl status | down          # Unix
target\debug\devctl.exe status | down      # Windows
```

**psql** (`DATABASE_URL` default `postgres://gamebackend:gamebackend@localhost:5432/gamebackend?sslmode=disable`):

```
PGPASSWORD=gamebackend psql -U gamebackend -h localhost -d gamebackend
```

The binary is on `PATH` on most Linux installs. Windows (PostgreSQL 18 installer)
and Homebrew macOS keep it out of `PATH`:

```
PGPASSWORD=gamebackend "/c/Program Files/PostgreSQL/18/bin/psql.exe" -U gamebackend -h localhost -d gamebackend   # Windows (git-bash path)
PGPASSWORD=gamebackend /opt/homebrew/opt/postgresql@18/bin/psql -U gamebackend -h localhost -d gamebackend         # macOS (Homebrew)
```

**Paired scripts** — same behaviour, one file per shell: `install.sh` /
`install.ps1` (adminctl user seeding), `scripts/memory-sync.sh` /
`scripts/memory-sync.ps1` (agent-memory mirror).

Neither `.sh` carries the executable bit in git, so on macOS/Linux invoke them
through the interpreter rather than `./`:

```
bash scripts/memory-sync.sh push | pull | path
```

## Platform-shaped behaviour in the backend

These are real per-OS differences in what the code does, not spelling:

- **Graceful shutdown** — a cooperative stop is `CTRL_BREAK_EVENT` to the process
  group on Windows and `SIGTERM` to the group on Unix. Split-proof's `[W2]`
  assertion drives whichever is native and requires a clean drain with no
  force-kill (`tools/splitproof/src/main.rs`). `weles`'s `OwnedProc::shutdown`
  makes the same distinction, and deliberately degrades to a forced stop *visibly*
  when the graceful signal cannot be delivered (a console-less Windows process
  cannot send `CTRL_BREAK` at all).
- **Containment unit** — a Windows Job Object vs a Unix process group. The kill
  paths themselves DO use PIDs: Linux's `pidfd_open` (`platform/linux.rs:163`)
  takes a pid, and the guardian signals the target's process group with
  `kill(-target_pid, SIGKILL)` (`guardian.rs:242`/`248`) — a pgid is a pid. Safety
  comes from POSIX zombie pinning, not handle-only addressing: the guardian is
  held as an unreaped `std::process::Child` (`platform/linux.rs:15`) and, inside
  the guardian, the target is held as an unreaped `std::process::Child`
  (`guardian.rs:112`) until `target.wait()` (`guardian.rs:245`) — an unreaped
  child's pid cannot be recycled by the kernel. `process.rs` only reaps via
  `try_wait`, and both `shutdown` (`process.rs:261`) and `Drop` (`process.rs:368`)
  bail out once a status is latched, so the kill paths become unreachable once a
  reap has happened. pidfd is belt over an already-load-bearing pinning
  guarantee, not the source of the guarantee. On darwin the same containment is a
  kqueue supervisor; the reused-pid safety is identical (zombie pinning, no pidfd).
  The old reap-then-kill window at `guardian.rs:245-250` was fixed (Step 7b):
  `forced_group` is now derived from enumerating the group *before* the reap, and
  the group kill happens while the target's pid is still pinned — on both platforms.
- **`run/rollout.lock`** — `weles` participates bit-compatibly with devctl/verifyctl
  **on Windows**: both implementations take a 1-byte `LockFileEx` lock at offset
  `1 << 63`, with an owner-only DACL on creation (`weles/src/lock.rs`,
  `tools/processctl/src/lock.rs:952`). The offset is a Windows-only device —
  `LockFileEx` is a mandatory byte-range lock and the metadata JSON lives at offset
  0 (`write_metadata`/`read_metadata` seek to 0), so the lock byte is parked out of
  the way at the top of the address space. On Linux AND darwin the lock is
  whole-file `flock(LOCK_EX | LOCK_NB)` (`tools/processctl/src/lock.rs:857`, now
  `cfg(unix)`) — there is no offset. "Bit-compatible" is a per-platform pact (both
  tools use the same primitive on the same OS), not a cross-OS wire format.
- **Local control endpoints** — a Windows named pipe with an owner-only DACL and
  server-pid peer validation; on Unix, a filesystem-path Unix domain socket
  (`weles/src/control.rs:548`: `remove_file` then `UnixListener::bind`, mode
  `0o600`), with peer identity checked from the socket. Linux uses `SO_PEERCRED`
  (one `ucred` struct carrying pid+uid); **darwin** uses two `getsockopt` calls at
  `SOL_LOCAL` — `LOCAL_PEERCRED` → `xucred` (uid, no pid field) plus `LOCAL_PEERPID`
  → the pid separately (`xucred` does not bundle the pid the way `ucred` does).
  Both `weles` and `devctl` carry their own copy of this peer-cred helper (separate
  fortresses — mirror, not shared). All transports are bounded so partial input
  cannot hang a rollout, per the trusted-local-operator model in the dev-tooling
  scope rule.

## Errata

**2026-07-17 (port landed)** — the entries below documented the *pre-port* state
(macOS could not build the rollout tools / could not verify). That state is now
history: the darwin port shipped and `verifyctl --fast` passes 16/16 on this M4 Max,
so the "Where the workspace runs" table and the removed "read/edit/review machine"
consequence have been rewritten to reflect it. The `weles` `E0061` and the six
non-building crates below were all real *at the time* and were fixed by the port; the
older corrections are kept for the record (a claim about compilation still requires a
compiler, not a reading of `cfg` gates — the lesson that stands). One product bug the
port surfaced along the way: an `AUTH_DDL` lock-order inversion in `modules/admin`
that could deadlock a concurrent `migrate` against the login path — exposed by this
machine's high test parallelism, not caused by it, and fixed.

**2026-07-17 (pre-port corrections)** — four claims in this file were overstated or
false. The first two were written from reading `cfg` gates, not from a compiler:

- Earlier revisions of the table above and the "Consequence" line claimed `weles`
  "builds and supervises natively on macOS" because
  `weles/src/platform/mod.rs:16` gates its containment layer on `#[cfg(unix)]`.
  That layer alone is darwin-ready, but `weles/src/control.rs` (the operator
  control endpoint) is not: `control.rs:103` calls `serve` with 5 arguments, while
  the darwin fallback arm at `control.rs:897` (under
  `#[cfg(not(any(windows, target_os = "linux")))]`, in a block literally commented
  "Unsupported-target fallbacks") declares only 4 parameters — a real `E0061`
  compile error, confirmed with `cargo check -p weles` on this machine. The
  `fleet_stop` parameter was added to the real (Windows/Linux) arms and the
  never-compiled fallback rotted out of sync. Even setting the compile error
  aside, `weles/src/supervisor.rs:767` treats a control-bind failure as fatal, so
  `weles up` could not run on macOS regardless.
- The `run/rollout.lock` bullet claimed the `1 << 63` offset "is shared by both
  implementations on both platforms — it is a wire detail of the lock, not a
  Windows-only quirk." It is Windows-only: `tools/processctl/src/lock.rs:952`
  (`lock_overlapped`, `#[cfg(windows)]`) is the only place the offset appears,
  needed because `LockFileEx` is a mandatory byte-range lock and the metadata
  JSON occupies offset 0. The Linux (and would-be darwin) arm is whole-file
  `flock(LOCK_EX | LOCK_NB)` (`lock.rs:857`) with no offset at all.
  `CLAUDE.md` already scoped this correctly with a parenthetical
  "(Windows: 1-byte lock at offset `1<<63`, owner-only DACL on creation)"; this
  file lost that precision when it generalized the claim to "both platforms".

Two further claims were overstated in a follow-up pass the same day:

- The "Containment unit" bullet claimed a reused PID "can never be signalled by
  mistake" via handle-only addressing. The kill paths themselves DO take PIDs
  (`pidfd_open(pid)`, `kill(-target_pid, …)`); the actual safety mechanism is
  that the target and guardian are held unreaped (so their pids cannot be
  recycled) until the code paths that would kill them are no longer reachable.
  The bullet now states that mechanism and names the one place it is NOT true
  today: `guardian.rs:245-250` waits (releasing the pid) and only then signals
  the now-stale pid — a real, narrow bug, tracked as Step 7b in the macOS
  rollout-port plan, not fixed here.
- The "Local control endpoints" bullet said "a loopback socket elsewhere",
  implying macOS coverage and misnaming the Linux transport (a filesystem-path
  Unix domain socket with `SO_PEERCRED` peer validation, not a loopback TCP/UDP
  socket). Corrected, and made explicit that macOS has no control-endpoint
  transport in either `weles` or `devctl` today — the same darwin gap the rest
  of this file already documents, not a new one.

Lesson: a claim about compilation requires a compiler, not a reading of `cfg`
gates — and a claim about a safety mechanism requires reading the mechanism, not
inferring it from the absence of an obviously wrong one.
