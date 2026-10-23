# Groups

Last updated: 2026-10-23

## In progress

_None._

## Open

- [ ] **Push nudge or notification for group invites and membership changes**
  GROUPS-01. Neither `modules/groups` nor `modules/notifications/src` handles
  group events (Grep lower bound: no `nudge`/`pushapi`/`notificationsapi` in
  groups, no group topic in notifications)
  (docs/roadmap/feature-tracker.md:201 @ fd797b3a, 2026-09-10).
- [ ] **Rename and join_policy change after create**
  GROUPS-02. `groupsapi::Groups` has `create` taking name and join_policy and no
  update op (`api/groups/api/src/lib.rs:127`); recorded as a plan non-goal
  (docs/plans/2026-09-06-2230-groups-membership-plan.md:663, 2026-09-06;
  docs/roadmap/feature-tracker.md:201 @ fd797b3a).
- [ ] **group.deleted topic**
  GROUPS-04. A teardown, including a sole admin's account deletion, is N
  `group.member_left` events with a dissolution reason; the comment at
  `api/groups/events/src/lib.rs:35` states there is no `group.deleted` topic
  (docs/roadmap/feature-tracker.md:201 @ fd797b3a and :145, 2026-09-10).
- [ ] **memberships_player_idx does not serve list_mine**
  GROUPS-05. The index is `(player_id, state, created_at DESC, group_id DESC)`
  (`modules/groups/src/lib.rs:80-81`) but `mine_sql` filters only on `player_id`
  and orders by `created_at DESC, group_id DESC` (`store.rs:628-637`), so the
  `state` column in the middle breaks the ordered scan
  (docs/roadmap/feature-tracker.md:201 @ fd797b3a, 2026-09-10; read at HEAD).
- [ ] **accept and reject as exported consts in groupsapi**
  GROUPS-06. `DECISION_ACCEPT`/`DECISION_REJECT` are `pub(crate)` in
  `modules/groups/src/service.rs:38-39`; every other vocabulary word is a
  `pub const` in `api/groups/api/src/lib.rs`
  (docs/roadmap/feature-tracker.md:201 @ fd797b3a, 2026-09-10; read at HEAD).
- [ ] **join resolves the caller's handle before its transaction**
  GROUPS-07. `join` calls `my_handle` (a directory call) before
  `pool.begin()` (`modules/groups/src/service.rs:490-495`), so a caller joining
  a missing group gets 503 instead of 404 when accounts is down
  (docs/roadmap/feature-tracker.md:201 @ fd797b3a, 2026-09-10; read at HEAD).
- [ ] **Proof for the invite directory-outage path**
  GROUPS-08. `conformance_directory_outage` covers only the paged reads'
  `hydrate`; its doc states `invite` and `join` are unreachable without a live
  store (`modules/groups/src/conformance.rs:187-202`), and no groups test names
  `Unavailable`. Party's equivalent is unit-proven by `invite_target_rules`
  (docs/plans/2026-09-28-2212-push-multiplex-invoke-and-party-plan.md:689,
  2026-10-05).
- [ ] **admin_recent_groups counts per row before the LIMIT**
  GROUPS-09. `JOIN LATERAL (COUNTS_LATERAL) ON true` then
  `ORDER BY g.created_at DESC, g.id DESC LIMIT $2`
  (`modules/groups/src/store.rs:564-574`, 2026-09-10; read at HEAD;
  docs/roadmap/feature-tracker.md:266 @ fd797b3a).
