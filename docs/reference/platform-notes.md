# Platform notes

Everything in `CLAUDE.md` / `AGENTS.md` is written platform-neutrally: the rules
(one rollout at a time, fail-closed dev opt-ins, the fortress rule) are the same
everywhere. This file holds the per-OS specifics — the concrete command spellings
and the places where a platform actually differs in kind, not just in syntax.

## Where the workspace runs

| | Windows | Linux | macOS |
| --- | --- | --- | --- |
| Build the workspace (`cargo build`, `clippy`, unit tests) | yes | yes | yes, except the crates below |
| Fleet rollouts (`devctl`, `verifyctl`, `splitproof`) | yes | yes | **no** |
| `weles` supervisor | yes | yes | **no** (compile error — see Errata) |

`tools/processctl` — the owned-process containment layer every rollout tool spawns
through — has exactly two backends: `tools/processctl/src/platform/linux.rs` and
`.../windows.rs`, selected by `#[cfg(target_os = "linux")]` / `#[cfg(windows)]` in
`platform/mod.rs`. There is no Darwin backend, so on macOS `spawn` and
`PlatformChild` simply do not exist and the crate fails to compile (14 errors, all
in `lock.rs`). `devctl`, `verifyctl`, `splitproof`, and `edgeca` have no errors of
their own but fail to build on darwin regardless, because each depends on
`processctl` (e.g. `tools/edgeca/Cargo.toml:14`). `weles` fails separately, with its
own single error unrelated to processctl — see Errata below. **Six crates do not
build on darwin: `processctl`, `weles`, `devctl`, `verifyctl`, `splitproof`,
`edgeca`.**

**Consequence: macOS is a read/edit/review machine, not a verification machine.**
Do not claim a rollout result from a Mac — run it on Windows or Linux.

Adding a Darwin backend to `processctl` (kqueue/`EVFILT_PROC` in place of Linux
pidfd, process groups as the containment unit) is the single change that would
unblock macOS rollouts. It is not currently planned — record it as a gap, don't
half-build it.

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
- **Containment unit** — a Windows Job Object vs a Unix process group. Ownership
  is always the platform handle itself; no path ever falls back to a PID or name
  lookup, so a reused PID can never be signalled by mistake.
- **`run/rollout.lock`** — `weles` participates bit-compatibly with devctl/verifyctl
  **on Windows**: both implementations take a 1-byte `LockFileEx` lock at offset
  `1 << 63`, with an owner-only DACL on creation (`weles/src/lock.rs`,
  `tools/processctl/src/lock.rs:952`). The offset is a Windows-only device —
  `LockFileEx` is a mandatory byte-range lock and the metadata JSON lives at offset
  0 (`write_metadata`/`read_metadata` seek to 0), so the lock byte is parked out of
  the way at the top of the address space. On Linux (and would-be darwin) the lock
  is whole-file `flock(LOCK_EX | LOCK_NB)` (`tools/processctl/src/lock.rs:857`) —
  there is no offset. "Bit-compatible" is a per-platform pact (both tools use the
  same primitive on the same OS), not a cross-OS wire format.
- **Local control endpoints** — a Windows named pipe with an owner-only DACL and
  server-pid peer validation; a loopback socket elsewhere. Both are bounded so
  partial input cannot hang a rollout, per the trusted-local-operator model in
  the dev-tooling scope rule.

## Errata

**2026-07-17** — two claims in this file were written from reading `cfg` gates, not
from a compiler, and were false:

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

Lesson: a claim about compilation requires a compiler, not a reading of `cfg`
gates.
