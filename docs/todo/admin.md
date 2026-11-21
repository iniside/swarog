# Admin

Last updated: 2026-11-21

## In progress

_None._

## Owner checks

- **The popover swap in a browser:** open a row menu on `/admin/players` in a live fleet and
  check that `admin.js` replaces the popover body; no test runs it in a browser.

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

### Known limits (admin-control-plane-redesign step 13d7c)

- [ ] **Unseeded claim action unchecked**
  `PortalModal::Unseeded` (mail's `cancelMail`, `mail/cancel`): the dev fleet seeds no pending
  or retrying mail, so the crawl checks only that `mail` draws no cancel opener. That `mail/cancel` is the
  portal's action for it stays unchecked until Step 24b3 diffs it.
- [ ] **Absent claim unchecked**
  `PortalModal::Absent` (the Lore `changeRole` and `releaseAll` entries) names no portal
  action, so the crawl has nothing to look for.

### Known limits (admin-control-plane-redesign step 13d3b4)

- [ ] **The mockup harness runs only onclick handlers**
  `tools/admindiff/src/mock.rs` runs each `onclick` body of a rendered page, tab or menu.
  It does not run `onchange`, `oninput` or `onsubmit` handlers, `meta.after` hooks, or
  the onclicks inside a modal or popover it opened. A modal reachable only from one of those
  is not in the inventory.
- [ ] **Modal and menu renders do not drain timers**
  Only the page probe drains queued timers (`MAX_TIMER_ROUNDS`). A modal or menu render
  that defers its drawing to a timer fails loudly instead of being drawn.
- [ ] **`HEAD_ACTIONS` is a hand list in the crawl tests**
  `tools/admindiff/src/crawl_tests.rs:170` names the page head actions of the fake server
  (`mail/test-send`) by hand instead of deriving them from the page map.
- [ ] **`validate_lists` is linked from docs but `pub(crate)`**
  `tools/admindiff/src/pages.rs:512`; the rustdoc link at `pages.rs:178` resolves only
  inside the crate.
- [ ] **Force candidates not yet forced**
  The index wallet KPI, the player page's Personal data (the mockup draws `undefined`) and
  the hard-coded banners have states admindiff never forces, so their other shapes are not
  compared.

### Known limits (admin-control-plane-redesign step 13d2c)

- [ ] **A source missing its admin provider looks healthy to the portal**
  `Snapshot::complete` (core/admin/src/engine/manifests.rs) marks a page or menu root
  `error` only when a source in `snapshot.sources` has never answered. An `Ok([])`
  reply counts as an answer, because every process without admin providers answers it.
  So a service built without its admin provider, or a peer missing from the source
  list, drops its module silently, and `admindiff crawl` passes. Closing it needs a
  per-source declaration of the modules it is expected to serve. Owner, 2026-11-19.
