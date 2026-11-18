# Account deletion

Last updated: 2026-11-18

## In progress

- [ ] **Orphan-row race between the tombstone guard and the purge sweep**
  ACCOUNT-DELETION-05. Owner reversed the accepted decision on 2026-11-17 ("wyscigow nie
  zakceptujemy"); closed per module by docs/plans/2026-11-17-1900-purge-race-locks-plan.md.
  - Decision: docs/plans/2026-09-10-1917-account-deletion-purge-plan.md:437-460 ("Known
    gap ... Not taken"). A write that read "no tombstone" before the plant, and commits
    after the player's sweep stamped `swept_at`, leaves one row no later sweep revisits.
    Closing it needs one shared per-player advisory lock on every guarded write path.
  - Recoverability from that analysis: the orphan is unreachable for characters,
    inventory and notifications, because every read path is keyed on the deleted identity.
    In friends and groups it is visible to, and removable by, the counterparty or
    co-members. The one unrecoverable shape is `groups.create`: the deleted player is the
    group's sole admin, so nobody can act on that group.
  - WALLET-01 reversed the decision for wallet only (39e2f802: `Store::lock_player_tx`,
    shared by `apply_on` and the sweep).
  - Per-module state, read at HEAD on 2026-11-17 (writer, then sweep). None shares a
    per-player lock between its guarded writer and its sweep, and none is exercised by a
    test:
    - characters: `create` takes `player_lock_key`, then checks the tombstone
      (`modules/characters/src/service.rs:136-148`); the sweep
      (`modules/characters/src/projection.rs:67-130`) takes no lock.
    - friends: `request` takes the requester lock, then checks the tombstone
      (`modules/friends/src/service.rs:397-410`, `store.rs:49-85`); the sweep
      (`modules/friends/src/projection.rs:67-133`) takes no lock.
    - notifications: `deliver_on` checks the tombstone and inserts with no lock
      (`modules/notifications/src/service.rs:329-359`); the sweep
      (`modules/notifications/src/projection.rs:415-474`) deletes with no lock.
    - inventory, player-owned path only: the dev-gated HTTP `grant` checks the tombstone on
      the pool, then `grant_pool` in a separate autocommit
      (`modules/inventory/src/service.rs:108,125-138`); the sweep
      (`modules/inventory/src/projection.rs:253-290`) takes no lock. The per-character
      `lock_character` covers the character-owned path, a different tombstone.
    - groups: the lock is per group, and the sweep locks only the groups
      `player_groups_tx` finds, from committed memberships
      (`modules/groups/src/store.rs:152-160`, `projection.rs:382-389,436-445`). A `join`
      in flight holds the group lock with its membership uncommitted
      (lock and guard `service.rs:496-508`, insert `:548-552`), so the sweep never locks that group and the membership
      survives. `create` guards with no lock (`service.rs:364-378`).

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
