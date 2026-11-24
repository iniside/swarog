# Admin

Last updated: 2026-11-24

## In progress

_None._

## Owner checks

- **The popover swap in a browser:** open a row menu on `/admin/players` in a live fleet and
  check that `admin.js` replaces the popover body; no test runs it in a browser.

## Open

- [ ] **Operator attribution on portal money movements**
  WALLET-07. `admin.action` records the actor and field names only, and
  `adminSubmit` carries no operator identity across the edge to the wallet. Source:
  docs/plans/2026-07-28-2125-wallet-module-plan.md:1060 (2026-07-30).
- [ ] **The entity page seeds the header avatar from the raw path id**
  `core/admin/src/routes.rs:534` takes `id` from the URL path unchanged and
  `routes.rs:625-626` seeds `render::avatar_color(&id)`; the accounts header provider seeds
  from the handle (`modules/accounts/src/admin.rs:560`) and swaps the header in place
  (`primitives.html.tmpl:416`, `hx-swap-oob`), so the first-render colour can differ from the
  swapped one, and a mixed-case id gives another colour (review of 2fdcfd93).

### Known limits (admin-second-pass step 5b)

- [ ] **No table shape check**
  Nothing checks that a `DataTable` row has as many cells as the table has columns:
  `core/admin/src/render/table.rs:234` tolerates the mismatch (`at.min(cells.len())`), so a
  stray extra cell passes every gate, while `controlplane::form_spec_errors`
  (`core/controlplane/src/lib.rs:984`) covers forms only. Intended fix: a `DataTable` shape
  check in controlplane next to `form_spec_errors`, reached from the shared manifest/panel
  test path so every module's tables get it.

### Known limits (admin-control-plane-redesign step 25)

- [ ] **Panel cache serves only on a failed provider call**
  `panels::resolve` (`core/admin/src/engine/panels.rs:290-323`) always calls the provider and
  reads the cached copy only when the call fails; the Step 25d4 plan text implies a fresh cached
  copy is served without a call. Decide which is intended: fix the plan text, or make a fresh
  hit skip the provider call.
- [ ] **An undecodable form spec answers 503 until FORM_TTL**
  Since c2383d34 a claimed form row whose spec no longer decodes releases its claim, but every
  submit of it answers 503 until the row expires (`core/admin/src/engine/forms.rs`); a 400
  telling the operator to reopen the form would end it at once.
- [ ] **The back/forward restore depends on htmx internals**
  `core/admin/src/admin.js:498-499` resets `el["htmx-internal-data"].requestCount`, a private
  htmx 2.0.4 field; an htmx upgrade can silently break the re-enable on a bfcache restore.
- [ ] **The flash check misses a repeated answer that saves another flash**
  The Step 25d2 claim-window tests check that a repeated (htmx) answer saves no flash, but not
  that it saves no other flash than the expected one (review of 7bbcda4f).
- [ ] **panels_tests copies the scripted provider from tests.rs**
  `core/admin/src/engine/panels_tests.rs` defines its own scripted provider instead of sharing
  the one in `core/admin/src/tests.rs` (review of 55a81cb2).
- [ ] **admincheck findings are untyped strings**
  `tools/admincheck` reports findings as plain `String`s, so nothing forces a new finding kind
  to get a fixture. Fix: a typed `Finding` enum and a test table that is exhaustive over it
  (proof audit of 13a3201a/295a41dd).

### Known limits (admin-control-plane-redesign step 26)

- [ ] **Dead step prose in topiccheck and cmd/*-svc**
  `tools/topiccheck/src/main.rs:96` still describes the old admin wiring in prose, and
  `cmd/*-svc` headers keep "Step N" changelog prose; rewrite them as invariants.

### Known limits (admin-control-plane-redesign step 24)

- [ ] **admincheck passes a self-dial**
  If admin-svc dialled peer "admin", `dialled_peers` (`tools/admincheck/src/checks.rs`) finds
  admin-svc itself, which hosts providers, so no finding fires; the portal would dial an edge to
  providers it serves locally. Fix: a finding for `peer == peer_of(PORTAL_PROCESS)` plus a fixture
  (proof audit of 9f691d5a).
- [ ] **admincheck drops a duplicate dial**
  The dial list is collected into a `BTreeSet` (`tools/admincheck/src/checks.rs` `dialled_peers`),
  so two `peer_face` lines for one peer in `cmd/admin-svc/src/lib.rs` give no finding
  (proof audit of 9f691d5a).
- [ ] **drawn_orders is not part of admincheck --strict**
  `tools/admincheck/src/drawn_orders.rs` is `#[cfg(test)]` only (`main.rs:4-5`), so the verifyctl
  admincheck stage never runs it.
- [ ] **No core test hides an empty Timeline**
  `core/admin/src/engine/panels.rs:214` treats a Timeline with no entries as empty and
  `core/admin/src/render/panel.rs:160` draws it hidden; no core test runs an empty Timeline
  through to the hidden state (only the audit provider tests reach it).

### Known limits (admin-control-plane-redesign step 23)

- [ ] **admindiff tests red on the new redesign screens**
  `cargo test -p admindiff --lib` fails 5 tests (`the_real_mockup_draws_only_declared_menus`,
  `_modals`, `_tabs`, `the_real_mockup_defines_only_declared_menus`,
  `a_real_mockup_tab_entry_needs_its_menu_entries`) and `admindiff crawl` stops on page-map
  drift: the redesign mockup (2026-11-23) draws a `character` row menu on the player
  Characters tab and a `group` row menu on the Groups tab, both compared, and the portal draws
  neither; `validate_menus` (`tools/admindiff/src/pages.rs`) refuses to exclude a drawn
  menu by design. The new screens themselves are declared Pending (fdc97507). Options: port
  the two row menus, or let `EXCLUDED_MOCK_MENUS` cover a drawn menu. Owner 2026-11-24:
  admindiff is left alone for now.
- [ ] **Two change-note inputs with different behaviour**
  The confirm dialog's note (`core/admin/src/templates/modal.html.tmpl:19`, gated by
  `data-reason-required` in admin.js) and the form note (`reason_field`,
  `primitives.html.tmpl:285`, browser `required`) differ in markup, error style and how
  they block submit (6856ea04). Fix: one shared note macro for both.
- [ ] **Macro-coverage gate checks one signature per macro, not each branch**
  `golden_cases_cover_every_macro_variant_and_state` (`core/admin/src/golden_tests/macros.rs`)
  passes when any case renders a macro's signature, so `{% if %}` branches can stay
  unrendered: the disabled `reason_field` of a read-only panel form has no golden. Fix:
  read each macro's branches from the template source and require a case per branch, or
  rename the test to what it checks; then add a read-only `panel_form` case with a note.
- [ ] **Change-note test and golden weaknesses**
  The Optional-note test's `!raw.contains(r#"autocomplete="off" required"#)`
  (`core/admin/src/tests.rs`) depends on attribute order; assert no `required` inside the
  `_reason` input tag instead. The `panel_form_reason` golden builds on `moded_form()`,
  which already has a field labelled "Reason", so it shows two "Reason" labels; give the
  fixture a `reason_label`.
- [ ] **Panel form has no banner for unmatched errors**
  A provider `FieldErrors` on a key the panel form does not draw is not shown on the panel
  re-render; only the modal has an error banner (`core/admin/src/render/modal.rs:114-121`).
- [ ] **Confirm actions accept a change note they do not draw**
  `check_form_keys` (`core/admin/src/engine/actions.rs`, ~389) lets `_reason` through for
  every Confirm action, even one with `ReasonReq::None` whose dialog draws no note, so a
  hand-posted note reaches the audit row. 9e8577eb closed this for Form only. Fix: the same
  rule for Confirm, plus a twin of `a_panel_form_without_a_change_note_refuses_a_posted_reason`.

### Known limits (admin-control-plane-redesign step 22)

- [ ] **SH2 tells admission's refusal from the provider's only by letter case**
  `tools/splitproof/src/scheduler_admin_checks.rs:151` looks for "Not implemented yet" (the
  portal's admission) against the provider's "not implemented yet"
  (`modules/scheduler/src/admin.rs:279`); `tools/splitproof/src/storage_admin_checks.rs:135`
  uses the same idiom. Fix: a distinct provider message, or assert that no `admin.action`
  audit row was written.
- [ ] **Splitproof skips the Scheduler page's disabled banner and its schedules foot, and Recent fires has no cap note**
  The page draws `DISABLED_BANNER` (`modules/scheduler/src/admin.rs:39`) and the foot "first
  100 schedules" when more than `MAX_ROWS` exist (`admin.rs:146-147`); no splitproof check
  covers either. Recent fires is read with `MAX_FIRES` = 100 (`admin.rs:34`, `:58`) and the
  page does not say it is capped.
- [ ] **The F1 ceiling bound is only loosely pinned**
  `TIMESTAMP_MAX_EPOCH_SECONDS` (`modules/scheduler/src/lib.rs:141`, used in
  `store.rs:43` of `SCHEDULES_SQL`) has no test at its edge. Add an anti-drift row at
  `TIMESTAMP_MAX_EPOCH_SECONDS` minus the ceiling interval.
- [ ] **`invoke_refuses_fire_now_and_knows_no_other_action` needs a DB pool it never uses**
  `modules/scheduler/src/admin_tests.rs` skips the test without a pool and builds
  `SchedulerAdmin::new(pool, true)`, but `invoke` and `panel("nope")` do not read the DB.
- [ ] **Comment errors**
  `modules/scheduler/src/lib.rs:137-140` (the `TIMESTAMP_MAX_EPOCH_SECONDS` doc);
  `INTERVAL_SECONDS` doc in `scheduler_admin_checks.rs:11-15` carries the `fetch`/`patient`
  paragraph that belongs to the pass, not the constant; rustdoc warns of an unclosed `<X>` at
  `core/controlplane/src/lib.rs:390`; `tools/splitproof/src/apikeys_admin_checks.rs:39-52`
  hop and break-run wording, and the long line at `:41`; `core/admin/src/golden_tests/view_tests.rs:98`
  "its filter at `any`".
- [ ] **The admindiff accept reasons give the wrong pairing**
  `tools/admindiff/accept/scheduler.txt:6-7` blame the Lore/Economy pairing, but the cause is
  the unported Platform Config & flags and Audit log. The `#ecbb9a` reason in 15 accept files
  names a pairing that admindiff's LCS does not make. Admindiff ignores reasons, so the gate
  result is the same.
- [ ] **A 409 on deleting a role shows the generic portal banner, not the provider's message**
  `core/admin/src/routes.rs:1326` maps `Invoked::Failed` with `Status::Conflict` to
  `CONFLICT_BANNER` through the `conflict` closure (`:1303`), dropping `error`'s text.
- [ ] **The filter chip "any" looks the same for a real value "any" and for no filter**
  `core/admin/src/templates/primitives.html.tmpl:218` draws `"any"` when the value is none;
  `routes.rs:681` treats a real "any" as a value.

### Known limits (admin-control-plane-redesign step 20)

- [ ] **The admindiff tab follower picks the newest player, so tab accept entries pin one seed**
  `tools/admindiff/src/crawl.rs:221-244` follows the first qualifying row of the players
  list, ordered `created_at DESC` (`modules/accounts/src/store.rs:1077`), and demoseed
  creates players in parallel (`tools/demoseed/src/lib.rs:513-517`), so every fresh seed
  follows another player. The `player.friends` entry `tr#dd6dc9` and the four
  `player.inbox` data-difference entries describe one seed and go STALE on the next (the
  split crawl of 4572f506 already holds 3 inbox rows against the monolith's 1); their reasons
  ("only accepted friendships", "one friend.accepted message per player") are false. Fix:
  follow a fixed seeded player, then rewrite those entries (proof audit of 4572f506).
  Owner 2026-11-23: deferred.
- [ ] **The Inbox tab qualifier never matches**
  `lists_rows` (`tools/admindiff/src/crawl.rs:497-499`) looks for `tbody > tr`; the Inbox
  timeline draws none, so the inbox follower always falls back to the first row. The
  `player.inbox` entry `b` is also a template fact (every message has a title,
  `modules/notifications/src/admin.rs:240-252`), not a data difference.
- [ ] **EXCLUDED_MODALS rows of an unfetched host are skipped silently**
  `tools/admindiff/src/crawl.rs:657-663` drops the rows whose host was not crawled with no
  UNCHECKED line, unlike the mock-modal list (proof audit of 232adec5).
- [ ] **The tab-fallback test is vacuous**
  `tools/admindiff/src/crawl_tests.rs:1418` asserts with `assert_failed`, which passes
  whatever the follower picks (proof audit of 232adec5).
- [ ] **A key-reused 409 shows the generic conflict banner**
  The portal draws its fixed `CONFLICT_BANNER` for every conflict, so the Send notification
  form's "key reused with another body" text never reaches the operator; the wallet forms
  have the same gap.

### Known limits (admin-control-plane-redesign step 13f)

- [ ] **Nothing refuses a literal `order` on a shared point**
  Step 13f moved every shared-point order into `core/controlplane/src/order.rs`, but no
  gate fails when a module's `admin.rs` writes a number instead of the constant; the 13f2
  snapshot catches a wrong value, not a literal of the right value. Owner 2026-11-22: each
  port's review checks it; a source scan or a typed order argument is deferred.
- [ ] **Two player tab orders tie across modules**
  `player_tab::ACCOUNTS_IDENTITIES` = `WALLET` = 30 and `ACCOUNTS_SESSIONS` = `INVENTORY` =
  40, so the order between them comes from the module-name tie-break, not a decision.
  Distinct values move goldens and accept files; owner 2026-11-22: do it in a port that
  already moves them.

### Known limits (admin-control-plane-redesign step 16)

- [ ] **A tab or page can declare a typed key as a param and never receive it**
  `declared_params` (`core/admin/src/routes.rs:643-657`) forwards only keys the portal does
  not read itself; a key such as `q`, `cursor`, `before`, `state`, `range` or `entity`
  listed in `EntityTab`/`PageDecl` `params` never reaches `params`, and admincheck has no
  check that rejects the declaration (review of c88143c0).
- [ ] **Entity deep links drop a tab's filter keys**
  The entity-page links built at `core/admin/src/routes.rs:179-181,600` carry the entity and
  tab but not the tab's declared filter keys, so a link to a filtered tab opens unfiltered
  (review of c88143c0).

### Known limits (admin-control-plane-redesign step 15)

- [ ] **The admindiff modal test site draws every exclusion it checks**
  `tools/admindiff/src/crawl_tests.rs:1497-1499` builds its fake site from `EXCLUDED_MODALS`
  itself, so an exclusion the real seeded fleet never draws (a `when_empty` tool) passes offline
  and fails only in the live crawl.
- [ ] **The Money JSON has one decoder but two encoders**
  `controlplane::decode_money` (`core/controlplane/src/lib.rs`) is the one parser, but both
  `encode_field` there and `posted_raw` (`core/admin/src/engine/actions.rs:452`) build the
  `{"amount","currency"}` object with their own `json!`; the round-trip test covers only
  `encode_field`, so a Money shape change can leave `posted_raw` behind. `posted_raw` calling
  `encode_field` with `FieldValue::Money` ends it.
- [ ] **Tiles whose labels differ are never compared inside**
  `diff_pages` (`tools/admindiff/src/skeleton.rs:471`) pairs a mock tile with a portal
  tile only by kind and key (the `kpi-label` text). A wallet tile named after a portal
  currency and the mockup's tile under another name come out as one `mock-only tile` and
  one `portal-only tile` line; their value, sub-line and markup are never diffed, so an
  interior drift inside such a tile is invisible behind the accept line. Owner, 2026-11-21:
  accepted as a known limit.

- [ ] **The wallet no-opener reason is repeated per user-menu host**
  `EXCLUDED_MODALS` (`tools/admindiff/src/pages.rs`) repeats the wallet row-menu reason for
  each user-menu host, and each new player tab needs 2 more rows. The crawl demands them
  loudly.
- [ ] **A landed module's `Absent` entries and the wallet outage group go unchecked**
  An `EXCLUDED_MOCK_MODALS` `PortalModal::Absent(module)` entry is never reported once that
  module lands, and the portal's down-state wallet group is never compared against the
  mockup's outage drawing.
- [ ] **Only one user-menu host runs the wallet claims in a unit test**
  `the_wallet_claims_hold_on_the_drawn_user_menu` (`tools/admindiff/src/tests.rs:874-918`)
  checks `players.menu.user` only; `player.menu.user`, `player.characters.menu.user` and
  `player.wallet.menu.user` carry the same lists and rest on the live crawl.
- [ ] **The stand-in site draws a per-host menu URL on tab entries**
  `Site::with_map` (`tools/admindiff/src/crawl_tests.rs`, `menu_buttons_in`) draws
  `/admin/_menu/{kind}/{page}` for tab entries, while the portal opens one
  `/admin/_menu/{kind}/{entity id}` on every tab (`core/admin/src/routes.rs:627`), so tab
  tests built from a page map never see two menu targets sharing one URL.
- [ ] **The sidebar diff pairs nav sections by shape, so one nav accept line hides Lore entries**
  `diff_pages` pairs leftover nav sections by similarity (`tools/admindiff/src/skeleton.rs:471-644`).
  The mockup's Economy entry is `down: true` (`UILayout/redesign/assets/registry.js:25`), so it
  never matches exactly and the mockup's Lore section pairs with the portal's Economy section
  (on the economy page only by position). The Lore lines in the 9 sidebar accept files blame
  "no Lore module", not the `.down` demo state. Which section a leftover entry counts in
  depends on that pairing. A counted line keeps each section's count (`mock-only ×{4,3}
  a.nav-item#ecbb9a` on the economy page) as the set of distinct per-parent counts
  (`tools/admindiff/src/skeleton.rs:416-422`), so it changes only when that set changes. A
  change that leaves the set as it was is hidden and its accept entry does not see it: with two
  parents (1,1) to (1,0) and (2,2) to (2,1), (4,3,3) to (4,4,3), and a parent dropping out,
  (4,3,3) to (4,3). The `mock-only a.nav-item#848c49` line has two parents on every page but
  mail, each giving 1 (Lore's Locks, Platform's Mail outbox), so it is a live instance of the
  limit: with parents (1,1), losing Locks or Mail outbox leaves the set {1} and the line
  byte-identical, and its accept entry stays green. The limit is stated in the `admindiff`
  usage text.
- [ ] **The Economy Ledger table is never compared with the mockup**
  A fresh fleet has no wallet movements (the starter grant is off and demoseed grants none), so
  `tools/admindiff/accept/economy.txt` accepts the empty-state body and the diff never sees the
  Ledger header or cells; a query that always returns no rows also passes. The columns rest on
  the hand-typed list in `modules/wallet/src/admin_tests.rs`. Enabling `wallet/starter_currency`
  and `wallet/starter_amount` in the dev fleet config fills the table and drops those entries.

### Known limits (admin-control-plane-redesign step 14a3)

- [ ] **Declaring the character kind opens an empty entity page**
  `EntityKindDecl{character}` (630ffe45) makes `/admin/.../character/<any id>` render an entity
  page with no header, summary or tabs instead of the "Unknown entity kind" 404
  (`core/admin/src/routes.rs:542`). Nothing links there today (`character_row` has
  `entity: None`). One declaration both makes a module the label owner and creates a page;
  the plan decides whether the kind gets a header or "owns labels" splits from "has a page".
- [ ] **Character labels drop uuid spellings Postgres accepts**
  `is_uuid` (`modules/characters/src/store.rs:36-42`) rejects braced or unhyphenated ids, so
  they draw the plain id. The portal sends only canonical ids.
- [ ] **The batch-reader uuid check is written twice**
  `is_uuid` in characters equals accounts' `is_canonical_uuid`
  (`modules/accounts/src/store.rs:98-104`), whose name is wrong (it accepts uppercase). With
  more batch label readers it becomes one helper in a foundation crate.
- [ ] **The character kind's icon is unchecked against the mockup**
  `swords` (also the summary fact's icon); nothing draws the kind's icon yet.
- [ ] **Purges-pending date and count follow the session's formatting**
  `controlplane::purge_backlog_body` (`core/controlplane/src/lib.rs:395`, drawing characters'
  and wallet's item) takes the first 10 chars of `min(created_at)::text` (DateStyle and time-zone dependent) and prints the count without
  thousands separators.
- [ ] **No database test reads the purges-pending item back**
  Nothing plants a `characters.purged_players` row and reads the item; such a test must be
  serialised with the purge tests sharing that table.

### Known limits (admin-control-plane-redesign step 14)

- [ ] **An admindiff tab-follower probe error is silent**
  `probe_page` (`tools/admindiff/src/crawl.rs:228`) drops a failed probe without printing it, and
  no test covers a probe error.
- [ ] **The tab follower checks the whole page for rows, not the tab panel**
  `lists_rows` (`tools/admindiff/src/crawl.rs:489`).
- [ ] **Two crawl comments still say a FirstRow host always lists a row**
  `tools/admindiff/src/crawl.rs:274` and `:569`; with the fallback (no row's tab lists a row)
  the followed tab can be empty.
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
