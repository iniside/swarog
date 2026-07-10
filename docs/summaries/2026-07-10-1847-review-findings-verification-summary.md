# Review-findings verification — 2026-07-10

Adversarial verification of the 19-item external review (5 HIGH, 9 medium, 5
checker gaps). Six parallel read-only agents, each instructed to REFUTE its
findings against the current code. Outcome: **17 CONFIRMED, 2 PARTIAL, 0
REFUTED.** No fixes applied yet — this doc is the verdict record.

## HIGH findings

| # | Finding | Verdict | Evidence |
|---|---------|---------|----------|
| 1 | Inventory create/delete reorder → orphaned holdings | **CONFIRMED** | `modules/inventory/src/lib.rs:595-623` — two independent subscriptions (`inventory.character-created.v1`, `inventory.character-deleted.v1`), separate cursors, no mutual ordering. `grant_starter` (`lib.rs:268-274`) has no character-existence check; `grant_exec` (`lib.rs:132-137`) is a bare upsert with no FK/tombstone. Wipe-before-grant leaves orphaned rows. |
| 2 | `eventctl skip` can rewind the checkpoint | **CONFIRMED** | `tools/eventctl/src/lib.rs:264-278` — final UPDATE `WHERE subscription_id = $1` only; no CAS on the cursor read at `:230-232`, no tx, no `FOR UPDATE`. Skip is allowed on an `active` subscription with 1–19 failures; a worker (backoff min 1 s) can deliver + advance between the read and the UPDATE, and skip rewinds it. Workers serialize via `FOR UPDATE SKIP LOCKED` (`worker.rs:95-108`) but eventctl never takes that lock. Paused targets are safe — the race is scoped to active-with-failures. |
| 3 | Split-mode Epic OAuth loses the token | **CONFIRMED** | `modules/gateway/src/proxy.rs:67` — `reqwest::Client::new()`, default policy follows up to 10 redirects. Callback returns `Redirect::to("/#token=…")` (`modules/accounts/src/epic_oauth.rs:255`); the fragment is client-side only, so the proxy follows the 302 to accounts-svc `/` and the browser never sees the token. `relay_response` (`proxy.rs:143-156`) would forward a 302 faithfully — reqwest eats it first. No proxy test covers redirects; split-proof never drives the Epic flow. |
| 4 | SIGTERM bypasses graceful shutdown | **CONFIRMED** (script sub-claim PARTIAL) | `core/app/src/lib.rs:651-657` — sole signal wait is `tokio::signal::ctrl_c()`; no unix SIGTERM branch, no Windows close/shutdown events. `ordered_teardown` (`:541-550`) runs only after that future resolves. `.sh` scripts use `kill` (SIGTERM) → bypass confirmed; `.ps1` scripts use `Stop-Process -Force` (`TerminateProcess`) — a hard kill no handler could catch, so on this Windows box the graceful path was never script-reachable at all. Only interactive Ctrl-C ever drains. |
| 5 | topiccheck indexes contracts by topic, not (topic, version) | **CONFIRMED** (latent) | `tools/topiccheck/src/main.rs:177-178` — `BTreeMap<&str, u32>` keyed by topic; duplicate versions overwrite. Same topic-only flaw at `:196` (first-match `find`) and `:254-261`/`:311`. Contradicts CLAUDE.md's v1+v2-coexistence model. Latent today: all six topics are single-version v1. |

## Medium findings

| # | Finding | Verdict | Evidence |
|---|---------|---------|----------|
| 6 | Verifier flattens accounts outage into 401 | **CONFIRMED** | Contract distinguishes `Ok(None)` from `Err` (`api/accounts/api/src/lib.rs:58`); `modules/gateway/src/verifier.rs:29,94-100` collapses `Err` → log + `None`; both fronts map `None` → 401 (`lib.rs:808-810`, `:408-410`). Outage ⇒ mass 401, no 502/503 path. |
| 7 | HTTP drain unbounded | **CONFIRMED** | `core/app/src/lib.rs:525-533` — `axum::serve(...).with_graceful_shutdown(...)` awaited with no timeout; a hung in-flight connection blocks the bounded QUIC drain + module stop forever. |
| 8 | Module migrations unserialized across replicas | **CONFIRMED** | `core/lifecycle/src/app.rs:67-75` — plain sequential `m.migrate()`, no advisory lock; modules run bare `sqlx::raw_sql(SCHEMA_DDL)`. asyncevents wraps its DDL in `pg_advisory_xact_lock` (`core/asyncevents/src/store.rs:33-52`) and its own comment documents the exact hazard ("tuple concurrently updated"). |
| 9 | Worker failure-write stale-state race | **CONFIRMED — timeout arm only** | Error arm is safe: `record_failure` commits on the same connection still holding `FOR UPDATE` (`worker.rs:186-197`). Timeout arm (`worker.rs:199-221`): `pg_terminate_backend` releases the row lock BEFORE `record_failure` runs on a fresh connection with `WHERE subscription_id` only (`:243`) — a replica can deliver + reset in between, then the stale `failures + 1` re-imposes backoff or (at the ≥20 threshold) pauses a healthy subscription. Cursor untouched — no event loss; self-heals on next success. |
| 10 | `history_contracts` cache not tx-aware | **PARTIAL** (LOW–MEDIUM) | Mechanism real: `transport.rs:104-116` marks seeded before the producer tx commits; rollback leaves a stale RAM entry. But impact is fail-safe: no-row topics are SKIPPED by GC (`retention.rs:104-106` — "unknown promise = keep"), never deleted early; "wrong policy" impossible (`ensure_history_contract` is insert-or-verify, raises on drift). Self-heals via subscriber-side reconcile at plane start (`catalog.rs:41-46`) or process restart. Residual bug: emit-only topic + first-emit rollback in a long-lived producer ⇒ retention unenforced until restart. |
| 11 | Expired sessions never deleted | **CONFIRMED** | `modules/accounts/src/lib.rs:65-71` — no `expires_at` index; `store.rs` INSERT-only (`:173-178`), read filters TTL (`:190-192`), no DELETE/prune job anywhere. Same gap existed in the Go original — not a port regression. |
| 12 | Contributions silently drop wrong-typed values | **CONFIRMED** | `core/contrib/src/lib.rs:17,43-48` — string-keyed slots, `filter_map(downcast_ref)` discards mismatches with no log/assert. Wrong `T` for a slot name ⇒ silently missing wiring. |
| 13 | split-proof polls /healthz not /readyz | **CONFIRMED** | `/readyz` genuinely checks DB + contributed worker/invalidation probes (`core/app/src/lib.rs:454-475,600-614`); scripts poll `/healthz` only (`split-proof.sh:233`, `.ps1:164`). Practical failure mode is flake/retry rather than false-pass, but the weaker gate is real. |
| 14 | split-proof SQL helper ignores psql exit code | **CONFIRMED** | `split-proof.sh:171-173` and `.ps1:154-156` — stderr → null, no exit-code check, no `ON_ERROR_STOP`, no `set -e`. Cleanup DELETEs fire unchecked; later `count(*)` assertions can count a previous run's rows. |

## Checker gaps

| # | Gap | Verdict | Evidence |
|---|-----|---------|----------|
| G1 | archcheck doesn't enforce core/* → never api|modules | **CONFIRMED** | `tools/archcheck/src/main.rs:184` — only `Kind::Module` consumers are constrained; core crates classify `Other`, never checked. Only core rule is bus→sqlx (`:288-303`). |
| G2 | svc-hosts-module check = Cargo dep, not registration | **CONFIRMED** | `main.rs:392-417` — `boots_its_module` scans `dependencies` only; a svc that depends but never adds the module to `modules()` passes. |
| G3 | Fleet lists hand-duplicated | **CONFIRMED** | Manual: `checkmodules/src/lib.rs:35-51` (12-svc vec), `verify.sh:146` (13-crate build list), `split-proof.ps1:77-98,225-409` + `.sh` mirror (ports + fleet). No cross-check against the derived module set. |
| G4 | admin-svc not modeled as planeless | **CONFIRMED** | `tools/topiccheck/src/main.rs:74` — `PLANELESS_PROCESSES = ["gateway-svc"]`; admin-svc runs `without_db` (`cmd/admin-svc/src/main.rs:47`) but topiccheck hands every process an identical lazy pool + recording transport (`:271-281`). Latent. |
| G5 | Stale outbox/inbox vocabulary in shipping docs | **CONFIRMED** | Doc comments in scheduler (`lib.rs:6,172` — "bus → outbox → sink"), characters, match, accounts, inventory ("inbox-dedup"), audit, leaderboard still describe the removed push model; `asyncevents/README.md:47` says "no inbox, no dedup table". archcheck's retired-token list doesn't ban the words. |

## Notes for the fix plan

- Findings 1, 2, 9 are all asyncevents-adjacent ordering/CAS issues — a fix plan
  should treat them together (cursor CAS / claim-lock for eventctl, existence
  guard or single-subscription redesign for inventory, re-claim before
  `record_failure` in the timeout arm).
- 4 + 7 are one shutdown story (signal coverage + bounded HTTP drain), and the
  `.ps1` scripts need a graceful-stop mechanism before any of it is testable on
  Windows.
- 13 + 14 are split-proof hardening and cheap.
- 10 can be downgraded or fixed trivially (move the cache insert after commit /
  drop the cache and rely on `ON CONFLICT DO NOTHING`).
