# Account deletion

Last updated: 2026-10-23

## In progress

_None._

## Open

- [ ] **Redact `asyncevents.events` and `audit.log` after `player.deleted`**
  ACCOUNT-DELETION-01. The player id and, for `player.deleted`, the handle survive
  the 90/30-day retention. Mirrored in docs/reference/game-backend-feature-gaps.md:306
  and README.md:140. Source: docs/roadmap/feature-tracker.md:145 (2026-09-10).
- [ ] **Purge a deleted player from leaderboard, rating and match**
  ACCOUNT-DELETION-02. None of the three subscribes to `player.deleted` (Grep of
  modules/, lower bound). `match.finished` carries opaque contestant strings, so
  they cannot be mapped to a player. Same root as NOTIFICATIONS-01. Source:
  docs/roadmap/feature-tracker.md:145 (2026-09-10); README.md:140-141;
  .agents/shared/gamebackend.md:245.
- [ ] **Purge a deleted player from mail**
  ACCOUNT-DELETION-03. `mail.outbox` has no `player_id`, and mail does not
  subscribe to `player.deleted`. Source: docs/roadmap/feature-tracker.md:145
  (2026-09-10); README.md:140-141.
