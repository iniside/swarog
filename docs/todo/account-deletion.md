# Account deletion

Last updated: 2026-11-22

## In progress

_None._

## Open

- [ ] **Redact `asyncevents.events` and `audit.log` after `player.deleted`**
  ACCOUNT-DELETION-01. The player id and, for `player.deleted`, the handle survive
  the 90/30-day retention. Mirrored in docs/reference/game-backend-feature-gaps.md:306
  and README.md:140 @ fd797b3a. Source: docs/roadmap/feature-tracker.md:145 @ fd797b3a (2026-09-10).
- [ ] **Purge a deleted player from leaderboard, rating and match**
  ACCOUNT-DELETION-02. None of the three subscribes to `player.deleted` (Grep of
  modules/, lower bound). `match.finished` carries opaque contestant strings, so
  they cannot be mapped to a player. Same root as NOTIFICATIONS-01. Source:
  docs/roadmap/feature-tracker.md:145 @ fd797b3a (2026-09-10); README.md:140-141 @ fd797b3a;
  .agents/shared/gamebackend.md:245 @ fd797b3a.
- [ ] **Purge a deleted player from mail**
  ACCOUNT-DELETION-03. `mail.outbox` has no `player_id`, and mail does not
  subscribe to `player.deleted`. Source: docs/roadmap/feature-tracker.md:145 @ fd797b3a
  (2026-09-10); README.md:140-141 @ fd797b3a.
- [ ] **`character.created` is kept forever, past `player.deleted`**
  ACCOUNT-DELETION-04. Since admin-control-plane-redesign step 16a1b (9e21e950)
  `character.created` is `HistoryPolicy::KeepForever`
  (`api/characters/events/src/lib.rs:46-47`), so its payload (`player_id`, `name`, `class`)
  stays in `asyncevents.events` for every character ever created, also after the player
  is deleted; ACCOUNT-DELETION-01's "90/30-day retention" does not cover it. KeepForever is
  there only because inventory's `inventory.character-owner.v1` replays from `Genesis` to
  fill its own `inventory.character_owners` table (16a1, ccf29df3). Two ways out, owner
  to decide: redact kept-forever payloads after `player.deleted` (widens ACCOUNT-DELETION-01),
  or have inventory ask characters for a player's characters instead of keeping its own copy,
  which drops the Genesis replay and lets `character.created` return to a retention
  (owner, 2026-11-22: maybe query instead of storing independently).
