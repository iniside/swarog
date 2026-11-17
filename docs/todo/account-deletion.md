# Account deletion

Last updated: 2026-11-17

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
- [ ] **Purge sweeps race the writers that guard on the tombstone**
  ACCOUNT-DELETION-04. Suspected, not exercised by any test: a WALLET-01-class race
  (fixed in wallet by one per-player advisory lock shared by writer and sweep, 39e2f802).
  A writer reads "no tombstone" and inserts; the tombstone and the sweep commit before the
  writer does; the sweep's DELETE never sees the writer's uncommitted row, so the row
  survives erasure. Read at HEAD on 2026-11-17, per module (writer, then sweep):
  - **characters** — `create` takes `player_lock_key` and then checks the tombstone
    (`modules/characters/src/service.rs:136-148`); the sweep
    (`modules/characters/src/projection.rs:67-130`) takes no lock before its
    `DELETE … RETURNING`. The lock is not shared.
  - **friends** — `request` takes the requester lock and then checks the tombstone
    (`modules/friends/src/service.rs:397-410`, `store.rs:49-85`); the sweep
    (`modules/friends/src/projection.rs:67-133`, `purge_edges_tx`) takes no lock.
  - **notifications** — `deliver_on` checks the tombstone and inserts with no lock
    (`modules/notifications/src/service.rs:329-359`); the sweep
    (`modules/notifications/src/projection.rs:415-474`) deletes with no lock.
  - **inventory** — only the player-owned path is in question: the dev-gated HTTP `grant`
    checks the tombstone on the pool and then `grant_pool` in a separate autocommit
    (`modules/inventory/src/service.rs:108,125-138`); the sweep
    (`modules/inventory/src/projection.rs:253-290`) states it takes no lock. The
    per-character `lock_character` guards the character-owned path, a different tombstone.
  - **groups** — `join` and the sweep share the per-GROUP lock (`service.rs:496-508`,
    `projection.rs:436-445`), so `join` is not affected. `create` checks the tombstone and
    inserts a new group and membership with no lock (`service.rs:364-378`), and the sweep
    finds a player's groups from committed rows (`projection.rs:382-389`), so `create` is
    in question.
  No module reviewed shares a per-player lock between its tombstone-guarded writer and its
  sweep, so none was dropped. Verify each with a staged-interleaving test like wallet's
  before fixing; the fix pattern is `modules/wallet/src/store.rs` `lock_player_tx`.
