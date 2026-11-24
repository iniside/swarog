# Friends

Last updated: 2026-11-24

## In progress

- [ ] **Admin removal of a friendship**
  FRIENDS-08. The player's Friends tab draws the friendship row-menu "Remove friend…"
  disabled (`implemented: false`, `modules/friends/src/admin.rs:243-257`): `api/friends`
  has no admin write, only the identity-bound `Player::remove`. Needs an operator removal
  of an edge by its id inside friends that emits `friend.removed` as the player path does.

## Open

- [ ] **Socket presence and follow**
  FRIENDS-01. Presence is per-gateway-process RAM and the offline transition
  runs in a `Drop` that cannot await; the gateway has no DB. Item-level half of
  the partial tracker row "Presence / online status / follow"
  (docs/roadmap/feature-tracker.md:202 @ fd797b3a, 2026-09-06). Re-checked: no friends
  presence code (read `modules/friends/src`, Grep lower bound).
- [ ] **Friend blocking**
  FRIENDS-02. A future state on the existing edge plus
  `Removed{reason: "blocked"}`; `modules/friends/src` has no block state (Grep
  lower bound; the only `block` hits are `block_on`/`block_in_place`). The
  tracker row "Friends (add/remove/block, states)" is marked shipped and names
  blocking in its title (docs/plans/2026-09-05-1921-friends-module-plan.md:586,
  2026-09-05; docs/roadmap/feature-tracker.md:200 @ fd797b3a).
- [ ] **Handle-existence oracle on request and party invite**
  FRIENDS-03. `request` answers an unknown handle with 404
  (`service.rs:381`, `NO_SUCH_PLAYER`) and a known one with 201, so handles can
  be enumerated; it is only rate-limited, not closed
  (docs/roadmap/feature-tracker.md:200 @ fd797b3a, 2026-09-06). `party.invite` has the
  same oracle for a party leader: an unknown handle is `NotFound`
  (`api/party/api/src/lib.rs:134-139`, `modules/party/src/service.rs:175-181`), a known
  one 201. Closing it changes the player-API contract of both modules and needs its
  own plan; closing friends alone buys nothing.
- [ ] **Test for the raced retry branch of request**
  FRIENDS-07. The branch needs two concurrent sessions interleaved between
  `insert_pending_tx` and `find_pair_tx`, and production exposes no hook to do
  that (`modules/friends/src/tests.rs:1066-1074`, 2026-09-05; comment read at
  HEAD).
### Known limits (admin-control-plane-redesign step 17)

- [ ] **The Friends tab pager's cursor filters rows instead of bounding the index range**
  Within one state the cursor predicate (`modules/friends/src/store.rs:483-484`, in
  `admin_page`) is a row filter, so a later page scans that state's earlier rows. Each side
  branch keeps its own `LIMIT` (`store.rs:569-579`), so the merge sort stays bounded at
  2 x limit. A true range bound needs four branches (two states x two sides). Owner
  deferred it, 2026-11-22.
