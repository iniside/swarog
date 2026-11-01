# Demoseed

Last updated: 2026-11-01

- [x] **Tests for the group re-run gate**
  DEMOSEED-01. `invite_needs_respond` (tools/demoseed/src/lib.rs, commit 5e58e635) pinned by three
  tests in 85c9ce17 (invited → respond, member → skip, anything else → error); errata 7a78621e
  reviewed. The async request/invite sequence in `seed_group` and `find_mine` matching stay without
  unit tests (would need a fake gateway); the owner's live run exercised them.
  Source: demoseed rollout (2026-11-01).
- [x] **First seed run takes ~2 minutes** — measured, owner decided to leave it
  DEMOSEED-02. Curl timings against the `devctl up` monolith (debug build), 2026-11-01:
  `/healthz` ~1 ms; authed reads (`/leaderboard`, `/accounts/me`, `/characters`) 3-7 ms; writes with
  a durable event (`POST /characters`, `/inventory/me/grant`) 4-6 ms; `match.report` 8-13 ms;
  `POST /accounts/register` and `/accounts/login` ~860 ms each (argon2, debug build). So no general
  monolith latency problem: ~60 hashes (~52 s) plus the gateway limiter (~480 calls at 20 rps, ~25 s)
  plus 429 backoff explain the ~2 minutes. Owner: register/login are rare, keep argon2 as is; keep
  demoseed as is (reusing the register `Session` and pacing to the limiter would cut it to ~30-40 s,
  not wanted now).
  Source: owner decision (2026-11-01).
