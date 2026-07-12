# Remediation Round 4 — execution summary

**Plan:** `docs/plans/2026-07-11-2249-remediation-round4-plan.md` (all 16 steps
executed 2026-07-11→12, one commit per step, trailer audit clean: 3× Fable 5,
5× Opus 4.8, 7× Sonnet 4.6, exactly matching the plan's lanes).

## What landed (finding → commit)

| Step | Commit | Fix |
|------|--------|-----|
| 1 | `82c9d3d` | `edge::Server::handle/handle_identity` panic on a duplicate wire method (the ONE silent-overwrite seam, now on the registry/lifecycle loud-boot convention); `edge::Client` gains `max_idle_timeout` (30s) + `DIAL_DEADLINE` (5s). routecheck now applies EdgeRegs only for processes that actually host an internal edge (the monolith never does — the old aggregation manufactured a fake `admin.adminData` collision). |
| 2 | `5be0d0f` | Gateway `remote_caller`: per-provider dial singleflight (keys.rs flight shape); the remotes cache is a sync mutex never held across an await — a dead svc stalls only its own routes. |
| 3 | `9af4380` | Whole-request inbound HTTP timeout in `core/app` (`HTTP_REQUEST_TIMEOUT_MS`, default 30s, `0` disables; deliberate 408) — closes the slow-upload DoS on typed ops AND the proxy inbound leg for every process. |
| 4 | `addc824` | Scheduler: dedicated per-fire connection (`connect_with(pool.connect_options())` — abort can't strand the advisory lock), bounded acquire (5s), one 30s budget per tick with skip-on-exhaustion, `stop()` grace(4s)-then-abort. |
| 5 | `ace9e96` | `core/remote`: definitive-answer guard on the SECOND attempt too — a NotFound replay no longer resets a healthy connection. |
| 6 | `7ca0b51` | Retention staleness clock (`retention_stalled`, mirrors `delivery_stalled`) + `asyncevents_retention_sweep_errors_total`; readyz flips on a live-but-ineffective sweep (3× housekeep interval). |
| 7 | `6840bdc` | rpc-macro: compile-time validation of `path_args`/`body_names` keys and path `{placeholder}` bijection; trybuild harness (1 pass + 3 compile-fail fixtures). No latent bugs found in existing contracts. |
| 8 | `37ec8d3` | routecheck header tells the truth: uniqueness is enforced at the authority; GATES is a hand-curated allowlist (new gate env var ⇒ GATES entry). |
| 9 | `5f21179` | Wire-only `#[retry_safe]` surfaced as a golden value: `opsapi::WireOp` + generated `wire_ops()`; +18 `wire` lines in contract-golden (5 previously-invisible wire-only methods); 9 api baselines re-blessed (additive). |
| 10 | `945e9d0` | JWKS cache TTL (600s) on the HIT path — a rotated-out/compromised kid expires; degrade-open for freshness under the fetch cooldown, closed for unknown kids. |
| 11 | `571dee8` | Argon2 dummy-hash prewarmed in `start()` via spawn_blocking in accounts AND admin; cross-module param-parity test in cmd/server. |
| 12 | `710c0a3` | `apikeysapi::MAX_KEY_BYTES` — one key-length contract: gateway check, store/admin creation-time rejection (bytes), `CHECK (octet_length(key) <= 256)` in DDL. |
| 13 | `1970677` | `event_id` folded into audit's CREATE TABLE — the sole `ALTER TABLE` in modules/ deleted (wipe-over-migrations restored uniformly). |
| 14 | `b78444f` | verify.sh/.ps1: failed cargo-audit install → FAIL (env defect); `--no-install` SKIP stays but any blocking SKIP is named on the final line (`VERIFY: OK (blocking stage(s) SKIPPED: …)`). |
| 15 | `982ec8d` | C# generator bails on a cross-domain DTO short-name collision (names both files); add-game-module skill gained the GATES checklist line. |
| 16 | (this commit) | CLAUDE.md + AGENTS.md updated (edge uniqueness, HTTP_REQUEST_TIMEOUT_MS, retention STALE, scheduler semantics); full `./verify.sh --all` gate; trailer audit. |

## Accepted gaps (deliberate, with rationale — from the plan)

- Delivery progress-vs-backlog probe: poison backoff is per-subscription by
  design (eventctl is the operator surface); flipping process readiness on it
  would be wrong. Unhealthy-pass STALE already covers connection-error loops.
- start-phase `require()` drift: requirecheck observes init only; documented
  convention is "requires resolve in init".
- sh/ps1 twins stay hand-maintained byte-parallel (no config layer).
- routecheck GATES stays a curated allowlist; derivation from source rejected
  as disproportionate — the skill checklist + header now carry the duty.

## Verification

Per-step minimal stages green throughout (each step's commit body/report);
splitproof 67/67 after Steps 2 and 3; final gate: `./verify.sh --all` — see the
run log under `run/verify/`.
