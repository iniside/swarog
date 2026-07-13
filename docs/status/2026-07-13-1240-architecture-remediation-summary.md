# Architecture remediation and Rust tooling — implementation summary

**Plan:** `docs/plans/2026-07-12-1214-architecture-remediation-rust-tooling-plan.md`

**Result:** all 20 planned steps were implemented. The shell/PowerShell process
orchestration was replaced by the Rust `processctl`, `devctl`, and `verifyctl`
tooling; the architecture, lifecycle, consistency, validation, and transport
findings were closed without changing the supported monolith/split topologies.

## What landed

| Steps | Representative commits | Result |
|---|---|---|
| 1–2 | `181c71e`, `619e96d`, `410e2f0` | Exact owned process trees, private state and rollout lease, one canonical fleet shared with splitproof. |
| 3–4 | `0bd4515`, `049a6f5`, `0c7122a` | Foreground `devctl` supervisor with bounded local control, exact cleanup, and frozen/sanitized environments. |
| 5–7 | `d358cfd`, `3106fcc`, `8f911aa`, `1bcfd77` | Typed `verifyctl`, ported verification stages and recoverable blessing, parity cutover, retired orchestration scripts removed. |
| 8–9 | `f7791f1`, `ae55195`, `caef058`, `e7be806`, `56ce6fb` | Shared RPC syntax authority, conformance policy removed from shipping graphs, zero-gap gate, contract metadata and response failure checks. |
| 10 | `62eb611`, `d0bcd93`, `c351a2f`, `971cf2c` | Canonical typed contribution slots and invalidation registration invariants; archcheck also catches imported `Slot::new`. |
| 11 | `47a986c`, `71e3a6a`, `7cf9957`, `8719fd4` | Retention threshold derived once, malformed-value fallback preserved deliberately, aggregate sweep health, checked bounds and focused tests. |
| 12 | `3615b9e`, `0431926` | Deterministic scheduler fairness within the existing budget and complete task reaping after abort. |
| 13–14 | `cfff987`, `b8c7812`, `69126e2`, `6cc0e7c`, `9285e0c` | Atomic account registration/session creation, serialized identity writers, browser-bound Epic OAuth state, split proof. |
| 15 | `ad78878`, `99025bd`, `4c6bd44`, `79cabed`, `e5411bb` | Rendered optimistic-concurrency evidence and all-or-nothing stale-form rejection for admin/config/API-key edits. |
| 16–17 | `bdbd325`, `495a27f`, `aefcb1f`, `002372d`, `f14200c`, `a9211c6` | Bounded limiter state and lifecycle-owned reapers; input byte caps and replay-aware match validation. |
| 18 | `f4b4060`, `c829a21` | Transport failure provenance retained; stream-local failures no longer evict healthy shared QUIC connections. |
| 19 | `738f732`, `fad13ba`, `f874803`, `d0512a6` | Blocking current-docs gate and canonical Rust tooling documentation. |
| 20 | `5168ad1` through `669b685` | Final-proof defects repaired and the complete Windows gate rerun cleanly. |

Commit boundaries were kept smaller where a focused fix verified independently.
The full ordered history remains the authoritative list; nothing was pushed.

## Final proof

After a clean one-rollout preflight:

```text
cargo run -p verifyctl -- --all --strict
```

passed in **298.1 seconds**, run id
`verify-1783938897-80808-4e6c021f`. Build, clippy, workspace tests, audit,
fortress/archcheck, routecheck, codegen freshness, contract golden,
zero-gap conformance, docs-current, splitproof, public API, C# client, and
topiccheck all passed. Fuzz was the expected platform SKIP on Windows. Per the
plan, mutants (`--slow`) were not run as part of this gate.

The final focused checks also passed:

- `cargo test -p verifyctl` — 21 unit and 3 integration tests;
- `cargo clippy -p verifyctl --all-targets -- -D warnings`;
- exact source scans found no broad process-kill commands in active tooling and
  no production dependency on conformance.

No WSL path was attempted. Package-wide formatting was not used as a release
gate and no unrelated pre-existing formatting churn was introduced.

## Defects exposed by the final proof

The end-to-end gate found real integration defects that focused tests had not:

- TOML 1.1 workspace manifests needed document parsing, not single-value parsing
  (`5168ad1`);
- the admin M3b fixture needed HTML-entity decoding of rendered optimistic-state
  JSON, and now asserts the POST redirect plus exact event count (`9bc2a6f`,
  `20fcf9c`);
- Windows executable canonicalization was valid for ownership checks but its
  verbatim `\\?\` spelling was invalid as child `argv[0]` for `dotnet`
  (`28c1727`, `29a273c`);
- the sanitized build environment had to preserve `APPDATA` for the existing
  NuGet configuration, without broadening the service environment (`54dea2e`);
- the C# health probe needed to construct and use reqwest inside its Tokio
  runtime (`c1bd531`);
- the C# stage reused the already-built server (`b777ba7`) and disabled official
  persistent .NET build servers (`669b685`). This removed an exact ten-minute
  owned-process-tree wait: the preceding green strict run took 893.2 seconds,
  while the final run took 298.1 seconds and started the fixture immediately
  after `dotnet build`.

One earlier interrupted rollout left test/DB activity behind. Only the exact
owned process tree was terminated; a standalone `asyncevents` run then passed
45/45 in 5.89 seconds, so no speculative event-plane code change was made.

## Deliberate boundaries

- No production migration/backfill machinery, topology change, event-contract
  mutation, WSL support work, or tooling security product was added.
- Malformed `EVENTS_HOUSEKEEP_INTERVAL` still falls back to the documented
  default; zero and unobservable/overflowing thresholds fail startup.
- Contribution type mismatch remains a loud invariant failure. The practical
  construction path is mechanically restricted, including imported
  `Slot::new`, rather than adding request-path recovery machinery.
- Public/generated surface changes were limited to the reviewed contract and
  optimistic-concurrency additions and passed public-api/codegen gates.
