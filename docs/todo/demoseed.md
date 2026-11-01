# Demoseed

Last updated: 2026-11-01

## In progress

- [ ] **Tests for the group re-run gate**
  DEMOSEED-01. `invite_needs_respond` and the request/invite convergence in `seed_group`
  (tools/demoseed/src/lib.rs, commit 5e58e635) have no tests; test-author step, plus a review of the
  plan errata commit 7a78621e. Blocked while a `devctl up` fleet runs (no second Cargo command).
  Source: demoseed rollout (2026-11-01).

## Open

- [ ] **First seed run takes ~2 minutes; profile it**
  DEMOSEED-02. Owner's live run on 2026-11-01 (default 30 players / 100 matches, monolith from
  `devctl up`, debug build) took about 2 minutes for ~540 gateway calls; the gateway limiter alone
  (`RATE_LIMIT_RPS`/`RATE_LIMIT_BURST`, core/app/src/lib.rs:1063-1069) accounts for ~30 s. Unmeasured
  hypotheses: argon2 hashing on register + login in a debug build (demoseed logs in after every
  register even though register returns a `Session`), and 429 backoff in `Gateway::call`. Owner:
  suspicious, profile later.
  Source: owner live acceptance (2026-11-01).
