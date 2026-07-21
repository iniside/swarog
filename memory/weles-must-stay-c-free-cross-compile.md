---
name: weles-must-stay-c-free-cross-compile
description: weles must stay C-dependency-free — bundled-C deps break the supported-targets cross-compile gate on macOS
metadata: 
  node_type: memory
  type: project
  originSessionId: a306e79f-f692-451e-9363-85eaa3151eae
  modified: 2026-07-21T17:05:05.502Z
---

`weles`/`weles-master` MUST stay free of C-linking dependencies (no `*-sys` crate
with a `cc` build script, no `bundled` C amalgamation). The `supported-targets`
verify stage does `cargo check -p processctl -p weles --target x86_64-unknown-linux-gnu`
and `--target x86_64-pc-windows-gnu` FROM the macOS dev box; a bundled-C dep needs a
cross C compiler (`x86_64-linux-gnu-gcc`, mingw) that the box lacks → hard FAIL
(`error occurred in cc-rs: failed to find tool "x86_64-linux-gnu-gcc"`).

**Evidence (2026-07-21):** Weles M1 step A3 added `rusqlite { bundled }` for the
master state store; `--fast` passed everything EXCEPT `supported-targets` (linux +
windows). Fix: swapped to **`redb`** (pure-Rust embedded store, deps `redb → libc`
only, no C build) — commits `56e0e8e`/`b830058`. The requirement was
write-concurrency, NOT SQLite specifically; redb serializes write txns in-process
(same "both writers commit, no loss" for the design's actual N-writer driver, which
is all in-process in the agent). Recorded as a dated errata in
[[mini-orchestrator-native-no-containers]]'s design doc (`docs/reference/weles-design.md`,
"## State: SQLite, runtime only").

**Accepted trade (redb vs SQLite):** redb's `Database::create` takes an EXCLUSIVE
file lock, so a concurrent second-PROCESS open (`weles deploy` writing
`deploy_history` while a live `weles up` holds it during its boot mint-pass) is
REJECTED (`DatabaseAlreadyOpen`), not blocked-and-committed like SQLite busy_timeout.
Non-fatal — both store writes are log-and-continue provenance, and `up` opens the
store only briefly (mint pass), not for the fleet lifetime. Tolerable because the
lost row is provenance, not correctness.

**Rule:** before adding ANY dependency to weles, check it (and its transitive graph)
for a C build script — `sqlx-sqlite`, `rusqlite bundled`, `openssl-sys`, `ring`
(has asm/C), etc. are all cross-compile hazards here. Prefer pure-Rust. The main
game backend (`core/`, `modules/`) does NOT have this constraint — only the two
cross-checked dev-tooling crates (`processctl`, `weles`) do.
