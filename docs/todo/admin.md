# Admin

Last updated: 2026-11-21

## In progress

_None._

## Owner checks

- **admindiff crawl after step 13e6:** on a dev fleet, run `admindiff crawl` and check that
  `mail` and `players` show no new drift now that their stack-wrapper accept lines are gone.
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
### Known limits (admin-control-plane-redesign step 14)

- [ ] **Menu and modal diffs without an accept section stay red**
  14a1's live diff: `players.menu.user`, `player.menu.user`, `player.characters.menu.user` (11
  lines each, the same user menu) and `events.modal.skip-poison` (3 lines) fail, as no accept
  file covers them. They predate 14a1 except the characters copy of the player menu.
- [ ] **The crawl follows the newest player, who may have no characters**
  The crawl takes the first players row. On a database with leftover test players that row has
  0 characters, and `player.characters` diffs 2 lines (the mockup's table, the portal's empty
  state) that the accept file does not cover. A fresh demoseeded fleet gives every player 1–3.
- [ ] **`counts_by_player` has no test for duplicate or differently spelled refs**
  `modules/characters/src/admin_tests.rs`: the duplicated ref has 0 characters, so a GROUP BY
  double count would pass, and every ref is a lowercase uuid. The engine dedupes refs today
  (`core/admin/src/engine/columns.rs:30-34`). One malformed ref fails the whole batch as Invalid.
- [ ] **The characters provider's titles and refusal paths are unpinned by unit tests**
  The manifest test does not pin the contribution titles, and no unit test covers the wrong
  entity kind or unknown contribution paths (`modules/characters/src/admin.rs:83,243`).

### Known limits (admin-control-plane-redesign step 13e)

- [ ] **The icon-only Link row button has no golden**
  13e1. The icon-only branch for a `Then::Link` row action
  (`core/admin/src/templates/primitives.html.tmpl:134`) is drawn by no golden, and admincheck
  requires an icon on an icon-only action but not `Emphasis::Ghost`.
- [ ] **Confirm-modal impact cells skip the link+label refusal**
  13e3. The `Cell.label` doc says the portal refuses a cell with both `link` and `label`, but
  the confirm modal's impact cells (`render/modal.rs:102`, ADMIN2-43) are not checked.
- [ ] **List-column cells draw a label as its raw value**
  13e3. `render/table.rs:185` does not run the Refs pass on `Cell.label`, so a labelled
  list-column cell shows the raw id.
- [ ] **Panel-form modes are checked only at draw time**
  13e4. The admincheck rule that refused more than one `PanelForm` action per contribution was
  replaced by the mode rule, which only sees modal `Then::Form` specs. A contribution with two
  `PanelForm` actions and no modes passes admincheck and draws an error panel
  (`core/admin/src/routes.rs:836-839`). A `head_actions` entry naming a mode action is also
  dropped from the page silently (`routes.rs:508`).
- [ ] **No admitted mode: opener enabled, read-only panel without seg**
  13e4. An operator admitted to a modal's opener but to none of its modes sees the item enabled
  and gets a 403 toast (`routes.rs:1011-1013`). A read-only panel form whose viewer has no
  admitted mode draws no `div.seg` and labels its button with `FormSpec.submit_label`.
- [ ] **Empty mode labels are not refused; a locked modal's seg stays clickable**
  13e4. Nothing refuses an empty mode `label` or `submit_label` (`core/controlplane/src/lib.rs:913-921`).
  After a 409 the modal's seg buttons stay enabled (`templates/modal.html.tmpl:15`); its
  submit is disabled, so nothing is sent.
- [ ] **A moded form's claim stays on the action it was minted for**
  13e4. The plan's "Mode authority" says the submit claims against the chosen mode's action.
  The portal claims the form row by the action in the post URL (the one it was minted for; for
  a panel form the first admitted mode), bound to the login, then admits and invokes the posted
  mode's action (`core/admin/src/routes.rs` `submit_form`). Both 13e4 reviews found nothing
  bypassable; the plan text and the code still differ. Owner, 2026-11-21: recorded, not changed.
- [ ] **Mode submit paths proven by reading only**
  13e4. No test asserts that the `admin.action` audit names the chosen mode, that a role
  removed between mint and submit is refused, that a form id posted to another mode's URL is
  refused, or the "not `Then::Invoke`" half of `form_mode_error`. The code handles each.

- [ ] **A request-id field accepts `required`, `readonly`, `help` and a duplicate key**
  13e5. `form_spec_errors` (`core/controlplane/src/lib.rs:936`) refuses only a default on a
  `FieldKind::RequestId` field. `required: true` draws a meaningless red `*`, `help` and
  `placeholder` are dropped, and a second field with the same key is not refused (that key
  would then post through the other field). The `disabled` branch of the field
  (`primitives.html.tmpl:273`) is drawn by no golden; no read-only panel has an instance.
- [ ] **The mockup's unavailable wallet form draws the request id; the portal leaves it out**
  13e5. `pages-platform.js:176` draws the field disabled with an id while wallet-svc is down;
  the portal draws the field only on a minted instance (plan 13e5). Step 15a2 hand-checks it.

- [ ] **The `panels` style argument and the bare-row CSS margin run in no golden**
  13e6. No golden has a non-last row with two or more panels, so the `style` argument of
  `panels` (`core/admin/src/templates/primitives.html.tmpl:207-209`) is unpinned. The 12px
  margin under a bare non-last row panel (`core/admin/src/portal.css:4`) and the `min-width: 0` of a
  grid slot's child (`portal.css:5`) are CSS rules that no golden or admindiff check sees. No
  golden draws the dashboard's no-attention branch (`shell.html.tmpl:49-50`) or a `#health` with
  two or more wide panels.

### Known limits (admin-control-plane-redesign step 13d7d)

- [ ] **One opener call may append more than one element**
  A3 of step 13d7d, dropped when the owner froze the mock harness (2026-11-21). In
  `tools/admindiff/src/mock.rs` the menu and modal renders take the first element their
  opener appends; later elements under the same `UI.openMenu` or modal opener call are
  neither compared nor failed as loose. The renders also read the production
  `EXCLUDED_MOCK_APPENDS` (`production_appenders`) while the probe reads the lists it is
  given, and `hasClass` has no caller. No real mockup opener appends a second element
  today.
- [ ] **State switchers in `onchange`/`oninput` are not probed**
  S7 of step 13d7d, dropped with the harness freeze. The probe runs only `onclick`, so
  `PAGES.auditTopic` (`UILayout/redesign/assets/pages-platform.js:121`) and
  `PAGES.effUser`/`effRepo` (`pages-lore.js:158`) fall under no PAGE_MAP /
  `NON_TAB_KEYS` rule. Matters once the audit page is mapped (it is `Route::Pending`).
- [ ] **The golden refusal check sees only titled `.btn` and `.menu-item` controls**
  `core/admin/src/golden_tests/refusals.rs` skips a disabled control with no title and
  any refused control that is not a `button.btn` or `.menu-item.disabled` (admindiff's
  `refusals_of` takes any `button[disabled]`). It checks that `data-refused-action` and
  `data-refusal` are well formed, not that they match the `ActionItem`; exact values are
  pinned only by the golden bytes. No golden declares a `ReadOnly(Some(..))` hint, so the
  recorded-hint branch never decides on a real case. Every refusal macro draws its title today.
- [ ] **A palette list outside the palette is not caught**
  `admin.js:276` reads `[data-palette-list] a.palette-item` across the whole document,
  but the crawl counts the input and list only inside the palette scrim.
- [ ] **Most controls of the crawl tests' fake portal are still hand-written**
  The shell, the palette and the disabled item and button come from `core/admin` goldens;
  the modal and menu openers, table rows, slots, nav items and tabs in
  `tools/admindiff/src/crawl_tests.rs` do not (the shell golden draws an empty `<nav>`).

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

### Deferred

- [ ] **The crawl never runs an action**
  Owner, 2026-11-21. `admindiff crawl` only GETs pages, menus and modals (its one POST is
  the login, `tools/admindiff/src/crawl.rs:313`), so it proves a control and its modal are
  drawn, not that the action reaches its module. Idea: on the dev fleet, submit the safe
  actions' forms and check the outcome. Today the delivery is covered by the `core/admin`
  tests and splitproof only.
