# Admin

Last updated: 2026-10-23

## In progress

_None._

## Open

- [ ] **Keep `?player=` after a successful wallet grant**
  WALLET-06. `render_after_submit` redirects to the bare `/admin/{slug}`
  (modules/admin/src/lib.rs:1401,1411), dropping the drill-down query. Source:
  docs/plans/2026-07-28-2125-wallet-module-plan.md:1055 (2026-07-30).
- [ ] **Operator attribution on portal money movements**
  WALLET-07. `admin.action` records the actor and field names only, and
  `adminSubmit` carries no operator identity across the edge to the wallet. Source:
  docs/plans/2026-07-28-2125-wallet-module-plan.md:1060 (2026-07-30).
- [ ] **Fetch remote `admin_data` once and concurrently**
  ADMIN-01. `resolve_items` (modules/admin/src/lib.rs:1495) awaits every remote
  `admin_data` in sequence on every `/admin` request. Source:
  docs/roadmap/feature-tracker.md:257 @ fd797b3a (2026-09-17); also
  docs/research/2026-10-22-1707-admin-redesign-gap-analysis.md:38-43.
- [ ] **Local-render pages run `block_in_place` that the request timeout does not cancel**
  ADMIN-02. Affects apikeys, friends, groups, mail, notifications and wallet admin
  pages (wallet: modules/wallet/src/admin.rs:389). `HTTP_REQUEST_TIMEOUT_MS` does not
  cancel the blocked section. Source: docs/roadmap/feature-tracker.md:257 @ fd797b3a (2026-09-17).
- [ ] **Seed the avatar palette from the canonical lowercase id**
  ADMIN-03. The friends drill-down seeds it from the raw URL param; the same pattern
  is in wallet and notifications. Source: docs/roadmap/feature-tracker.md:257 @ fd797b3a
  (2026-09-17).
- [ ] **Derive `PARAM_PLAYER` and `PLAYER_REF_PREFIX` instead of redeclaring them**
  ADMIN-04. Local literals in modules/{friends,notifications,storage,wallet}/src/admin.rs
  (Grep, lower bound); they should derive from
  `accountsapi::admin::PLAYERS_ROW_MENU`. Source: docs/roadmap/feature-tracker.md:257 @ fd797b3a
  (2026-09-17).
- [ ] **A remote-fetch error collapses an item's section and label to its id**
  ADMIN-05. modules/admin/src/lib.rs:1504-1509 sets section and label to `it.id`,
  which breaks the page slug on a DB blip. Companion to AUDIT-02 and
  NOTIFICATIONS-05. Source: docs/plans/2026-09-05-1921-friends-module-plan.md:596
  (2026-09-05).
- [ ] **Pin `PLAYERS_ROW_MENU` ordering across topologies**
  ADMIN-06. characters, friends, notifications and wallet use priority 0 in their
  src/admin.rs; inventory uses 10. Source:
  docs/plans/2026-09-05-1921-friends-module-plan.md:601 (2026-09-05).
