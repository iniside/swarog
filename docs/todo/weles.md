# Weles

Last updated: 2026-10-23

## In progress

_None._

## Open

### Supervisor and control plane

- [ ] **Readiness probe off the monitor thread**
  WELES-01. `observe` calls `health::probe` inline per service while the phase is
  `WaitingHealthy`, so the probe runs synchronously on the monitor thread
  (weles/src/supervisor.rs:1326, 2026-07-21; re-checked at HEAD by targeted read).
  Source: docs/status/2026-07-16-1342-weles-m1-readiness-review-2-status.md B1,
  docs/status/2026-07-16-0810-weles-m1-readiness-review-status.md P7.
- [ ] **`DOWN_TIMEOUT` derived from fleet size**
  WELES-02. `DOWN_TIMEOUT` is a fixed 130s (weles/src/main.rs:17, 2026-07-15) while worst-case
  teardown is N x (`STOP_GRACE` + `STOP_FORCE`) = N x 10s (weles/src/supervisor.rs:72-73).
  weles/fleet.split.toml declares 19 `[[service]]` entries, so 19 x 10s exceeds 130s.
  Source: docs/status/2026-07-16-1342-weles-m1-readiness-review-2-status.md B2.
- [ ] **Control endpoint bound after prepare hooks (M2)**
  WELES-03. The `[[prepare]]` hooks run before `ControlServer::bind` (weles/src/supervisor.rs:807
  and :866-873, 2026-07-16), so `weles down` in the prep window gets "very early
  startup". Source: docs/reference/weles-design.md:905,
  docs/plans/2026-07-16-0830-weles-m1-hardening-plan.md:259,
  docs/status/2026-07-16-1342-weles-m1-readiness-review-2-status.md.
- [ ] **Mid-run state-checkpoint failure (M3)**
  WELES-05. Only the initial pin checkpoint is critical (`checkpoint_critical`,
  weles/src/supervisor.rs:607); later `Reporter::checkpoint` calls log and continue
  (:585-592), so a state dir that breaks mid-run degrades silently.
  Source: docs/plans/2026-07-16-0830-weles-m1-hardening-plan.md:264 (2026-07-16).
- [ ] **Control serve-thread panic not reported by `dead()`**
  WELES-06. `ControlServer::bind` stores `thread_dead` only inside the `Err` arm
  (weles/src/control.rs:112), so a panic leaves `dead()` false forever. The agent
  endpoint was fixed with `DeathFlag`; control was not. KNOWN GAP comment at
  weles/src/agentapi.rs:237 (2026-07-17).
- [ ] **Start-time check for Linux `supervisor_alive`**
  WELES-07. Linux/other liveness is pid-only; Windows has the asymmetric creation-time check
  (weles/src/control.rs:415-440, KNOWN GAP, 2026-07-16). Failure mode is benign
  over-protection of a stale state file.
- [ ] **fsync in `flip_current`**
  WELES-04. `flip_current` (weles/src/prep.rs:942, 2026-07-16) writes `current` without
  `sync_all` (no `sync_all` in prep.rs by Grep, a lower bound). Optional;
  `verify_generation` sha-verify-on-read catches the symptom.
  Source: docs/status/2026-07-16-1342-weles-m1-readiness-review-2-status.md B3'.

### Replicas and resolve

- [ ] **Live replica test through minting and resolve**
  WELES-20. Nothing exercises `replicas: 2` -> mint -> `weles up` -> `/resolve` returns two
  addresses -> client spreads across both. tools/verifyctl/src/stages/weles_managed_gateway.rs
  (:760-830) proves set consumption with a stage-owned fake agent, not weles minting.
  Source: docs/reference/weles-design.md:840 (2026-07-28).
- [ ] **Replica spread unobserved in the live fleet**
  WELES-21. Needs a per-instance edge counter; `core/metrics` exposes only
  `http_requests_total` (core/metrics/src/lib.rs:80). Spread is proven at unit level only.
  Source: docs/plans/2026-07-30-2149-weles-store-doc-replicas-remediation-plan.md:584 (2026-07-30).
- [ ] **Cross-process store rejection test**
  WELES-22. weles/master/src/store_tests.rs proves the reject class with two handles in one
  process only.
  Source: docs/plans/2026-07-30-2149-weles-store-doc-replicas-remediation-plan.md:589 (2026-07-30).
- [ ] **M2 liveness for resolve**
  WELES-23. The `alive` flag is a placeholder (weles/master/src/store.rs:88-89) and
  `200 {"addrs":[]}` is never produced (weles/src/agentapi.rs:34-35).
  Source: docs/reference/weles-design.md:877-888,
  docs/plans/2026-07-30-2149-weles-store-doc-replicas-remediation-plan.md:581 (2026-07-30).

### Agent

- [ ] **Agent as a system service**
  WELES-27. The design relies on the OS service manager restarting the agent and states the
  agent cannot be installed as one today; the root/privilege requirement is unstated.
  Source: docs/reference/weles-design.md:120 and :468-472 (2026-07-21).
