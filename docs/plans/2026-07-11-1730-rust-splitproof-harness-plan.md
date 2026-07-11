# Plan — Rust split-proof harness (`cargo run -p splitproof`)

## Context

The shell/PowerShell split-proof harnesses are structurally fragile on Windows.
In one session three DISTINCT platform bugs surfaced in a row, each hidden behind
the previous: (1) `winctrl` `Start-Svc` false-throws on a `$null` `$spawn.ExitCode`
(orphans the svc); (2) `split-proof.sh` `[AD2b]`/`[AD2c]` hang on bash `wait` over
native curl.exe children under git-bash/MSYS; (3) Windows PowerShell 5.1 strips
embedded `"` from native args, so every JSON `-d`/playercli payload reaches the
process malformed → 400 / "key must be a string at column 2". All three are the
SAME root cause class: **a shell sits between the harness and the processes**, and
each platform's shell mangles process spawning, job control, or argument quoting
differently.

A Rust harness eliminates the entire class: `std::process::Command` takes env as a
typed map and args as `Vec<String>` (no quoting layer), process lifecycle is a
`Drop` guard (no winctrl, no orphans), parallelism is `tokio` (no `wait` hang), and
QUIC/HTTP/DB use the workspace's own libraries (`edge::PlayerClient`, reqwest, sqlx)
instead of `curl.exe`/`psql.exe`/`playercli.exe` subprocesses. It is cross-platform
by construction (Windows == Linux) and matches the project ethos (config-as-code,
anti-magic, everything-is-Rust). Decision (user, 2026-07-11): **MVP-first**, and the
Rust harness **replaces both scripts** once it reaches assertion parity.

### Why not extend the existing scripts

Already tried — three fixes landed (Start-Svc pid-file gating, `.sh` curl -Z, ps1
Invoke-Curl temp-file + Invoke-PlayerCli escape) and each revealed the next platform
quirk. The shell approach is a whack-a-mole tar pit on Windows; the harness is the
structural fix, not another patch.

### Reuse (Research before planning)

- **Fleet boot matrix** is fully known from `split-proof.ps1:402-591` — every svc's
  env map + ports. Copy verbatim into typed Rust structs. Ports from CLAUDE.md:
  characters 8080/9000, inventory 8081/9001, gateway 8082 + player 9100,
  config 8083/9002, accounts 8084/9003, admin 8085, audit 8086/9004,
  scheduler 8087/9005, match 8088/9006, rating 8089/9007, leaderboard 8090/9008,
  apikeys 8091/9009.
- **QUIC player calls**: `edge::PlayerClient::dial(addr, &trust)` + `.call(method,
  token, api_key, payload_bytes)` — exactly what `tools/playercli/src/main.rs` does.
  Reuse `edge` as a lib (no playercli subprocess).
- **CA mint**: `edgeca.exe --cert --key` (no JSON/quoting → safe to shell out) OR
  `edge::DevCA` if it exposes a generate-and-write fn (prefer lib; confirm at impl).
- **Admin seed**: `adminctl create-user <name> --password-stdin` (password over
  stdin pipe from Rust — no argv quoting).
- **DB assertions**: `sqlx` (already a workspace dep, used by every module).
- **HTTP assertions**: reqwest (confirm/add to workspace at impl).

## Non-goals (MVP)

Full ~50-assertion parity, the W1/W2 graceful-shutdown proof, monolith parity
re-run, and Epic OAuth deferrals — all deferred to follow-up iterations. MVP proves
the harness design end-to-end on a core assertion set, then grows.

## Layout

New crate `tools/splitproof` (bin). `cargo run -p splitproof`. Not in `modules/`
(it's a test tool, not a fortress). Registered in the fleet-drift preflight the same
way the scripts are (compare its svc list to `cmd/*-svc` on disk).

## Steps

### Step 1 — Crate skeleton + fleet model + process lifecycle `[inline]`

**What:** `tools/splitproof/{Cargo.toml,src/main.rs}` + `src/fleet.rs`.
**How:** `Svc { key: char, name: &str, http_port: u16, edge_port: Option<u16>,
env: Vec<(String,String)> }`. A `Running` guard wrapping `std::process::Child` with
`impl Drop { fn drop(&mut self){ let _ = self.0.kill(); let _ = self.0.wait(); } }`
so a panic/early-return kills the whole fleet — no orphans, ever. `spawn(svc)` sets
env via `Command::envs`, redirects stdout/stderr to `run/logs/<name>.out/err`, and
returns the guard. Binaries resolved from `target/debug/<name>.exe` (or no `.exe` on
unix — `std::env::consts::EXE_SUFFIX`). Pre-boot: `cargo build -p <each>` (via
`Command`), clear stragglers (best-effort kill by name), mint CA, seed admin users.

### Step 2 — wait_healthy + ordered boot `[inline]`

**What:** `src/fleet.rs` boot sequence.
**How:** `wait_healthy(port)` = reqwest GET `http://127.0.0.1:<port>/readyz` polled
until 200 or a ~30s deadline (typed timeout, not sleep-magic). Boot ORDER mirrors
the script (deps first): accounts → apikeys → audit → scheduler → rating →
leaderboard → match → characters → config → inventory → gateway → admin, each with
its exact env map from `split-proof.ps1:402-591` and `wait_healthy` before the next.
Fleet-drift preflight: assert the harness svc list == `cmd/*-svc` dirs on disk (fail
loud before boot, per the "didn't-forget scripts self-check" rule).

### Step 3 — Assertion framework + MVP assertions `[inline]`

**What:** `src/assert.rs` + `src/checks.rs`.
**How:** a tiny recorder — `struct Proof { pass: u32, fail: Vec<String> }` with
`check(name, bool)` / `check_eq(name, a, b)`; prints `PASS`/`FAIL` per line, a final
tally, and the process exits non-zero iff any fail. MVP assertion set (each a real
cross-process flow):
- **AUTH**: POST `/accounts/register` → 201 + player_id; `/accounts/login` → 200 +
  bearer; `/accounts/me` (Bearer) → 200 (auth-once over the edge). reqwest JSON —
  no quoting.
- **[K5]** key-verifier: N concurrent distinct bogus `X-Api-Key` → every response
  401/403/429, never a 5xx (the guaranteed observable; 503-shed is unit-tested).
- **[C4]** config large-value: write a >8 KB value via sqlx, assert no abort +
  revision bump + a downstream svc reflects it after reload (Step 6 fix).
- **[P1]** QUIC: `edge::PlayerClient` characters.create through gateway :9100 →
  domain status "Ok" (reuses edge lib).
- **cross-process event**: create a character → inventory starter-grant appears
  (durable `character.created` → inventory), asserted via sqlx.
- **config live-reload**: change a config value → a consuming svc's behavior flips
  (CachedConfig invalidation).
- **admin lockout**: 12 concurrent wrong logins via reqwest (tokio join) → user
  locks at exactly 5, one `admin.action` login-locked (sqlx) — the exact flow that
  hung the bash harness, now deadlock-free in tokio.

### Step 4 — Wire into verify + retire the scripts (follow-up, NOT MVP) `[inline]`

**What:** `verify.sh`/`verify.ps1` fortress/split-proof stage → `cargo run -p
splitproof`; delete `split-proof.sh`/`.ps1` + `tools/winctrl` once assertion parity
is reached. Deferred until the Rust harness covers the full named-assertion set.

## Verification

Run `cargo run -p splitproof` on this Windows box — it must boot 12/12, pass the MVP
assertions, and tear down with no orphaned `-svc.exe` (check Task Manager: zero
after exit, because the `Drop` guard kills them). This is the proof the design holds
where the shell harnesses failed. `cargo build -p splitproof` + clippy clean.
One-test-at-a-time still applies (it drives the shared Postgres).

## Dispatch

`[inline]` throughout: the harness must be built by iterating against the live
fleet (spawn → observe → fix), which is exactly the mid-edit judgment loop that
doesn't hand off cleanly. Commit per step.
