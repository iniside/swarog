# Demoseed

Last updated: 2026-11-01

## In progress

- [ ] **Tests for the group re-run gate**
  DEMOSEED-01. `invite_needs_respond` and the request/invite convergence in `seed_group`
  (tools/demoseed/src/lib.rs, commit 5e58e635) have no tests; test-author step, plus a review of the
  plan errata commit 7a78621e. Blocked while a `devctl up` fleet runs (no second Cargo command).
  Source: demoseed rollout (2026-11-01).

## Open

- [ ] **Monolith logs empty under `devctl up`**
  DEMOSEED-03. `run/devctl/monolith.out.log` / `.err.log` stayed 0 bytes during a full demoseed run
  although the server answered every call; check whether logging is off (`RUST_LOG`) or routed
  elsewhere before any future profiling.
  Source: owner live acceptance (2026-11-01).
  Source: owner live acceptance (2026-11-01).
