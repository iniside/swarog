# Friends

Last updated: 2026-10-23

## In progress

_None._

## Owner checks

- [ ] **Friends admin page against a live render**
  FRIENDS-04. Boot a monolith (`devctl up monolith`) and compare `/admin/friends`
  with the mockup; the templates are `include_str!` embedded and no fleet was
  booted while the page was written
  (docs/plans/2026-09-05-1921-friends-module-plan.md:609, 2026-09-05).

## Open

- [ ] **Socket presence and follow**
  FRIENDS-01. Presence is per-gateway-process RAM and the offline transition
  runs in a `Drop` that cannot await; the gateway has no DB. Item-level half of
  the partial tracker row "Presence / online status / follow"
  (docs/roadmap/feature-tracker.md:202, 2026-09-06). Re-checked: no friends
  presence code (read `modules/friends/src`, Grep lower bound).
- [ ] **Friend blocking**
  FRIENDS-02. A future state on the existing edge plus
  `Removed{reason: "blocked"}`; `modules/friends/src` has no block state (Grep
  lower bound; the only `block` hits are `block_on`/`block_in_place`). The
  tracker row "Friends (add/remove/block, states)" is marked shipped and names
  blocking in its title (docs/plans/2026-09-05-1921-friends-module-plan.md:586,
  2026-09-05; docs/roadmap/feature-tracker.md:200).
- [ ] **Handle-existence oracle on request**
  FRIENDS-03. `request` answers an unknown handle with 404
  (`service.rs:381`, `NO_SUCH_PLAYER`) and a known one with 201, so handles can
  be enumerated; it is only rate-limited, not closed
  (docs/roadmap/feature-tracker.md:200, 2026-09-06).
- [ ] **admin_recent ordering on the text-cast id**
  FRIENDS-06. The select list projects `id::text`, so the unqualified
  `ORDER BY created_at DESC, id DESC` tie-break resolves to the output column
  and sorts as text (`modules/friends/src/store.rs:461`, 2026-09-05; read at
  HEAD; docs/roadmap/feature-tracker.md:258).
- [ ] **Test for the raced retry branch of request**
  FRIENDS-07. The branch needs two concurrent sessions interleaved between
  `insert_pending_tx` and `find_pair_tx`, and production exposes no hook to do
  that (`modules/friends/src/tests.rs:1066-1074`, 2026-09-05; comment read at
  HEAD).
