# Admin second pass

Last updated: 2026-11-24

The admin redesign ships mockup elements the backend cannot back as deterministic fakes
(`// FAKE: <why>`, plan R1) and not-yet-possible actions as `implemented: false`, rendered
disabled (R2). This is the one list of them, so a second pass can replace each with a
real feature. Found with Grep over `*.rs` (`// FAKE:`, `implemented: false` and the
`.decl(.., false)` calls), which is a lower bound: a marker written another way is missed.
A port adds its own markers here in its part `a` commit (mockup-implementation skill);
a port that writes a new `contract gap` accept entry adds its item under Accepted contract gaps.
Rule source: docs/plans/2026-10-22-1759-admin-control-plane-redesign-plan.md:45-53.

## In progress

_None._

## Open

### Fake data

- [ ] **Service health, probe interval, down/degraded notes and the wallet-down attention
  item**
  ADMIN2-02. Mockup: Service health panel (state, version, uptime, latency, note), top-bar
  health popover, "wallet-svc unavailable" attention. Fakes: core/admin/src/fakes.rs:14
  (`HEALTH_PROBE_S`, and its `health_note`/`health_probe` strings), :31 (`services()`),
  :97 (`attention_services()`): no health probe exists until Weles exposes its API.
  Replaced by: real per-process probes from the Weles API.
- [ ] **Service health history bar**
  ADMIN2-03. Mockup: the 48-bucket uptime bar per service. Fake:
  core/admin/src/fakes.rs:77 (`history`): no health history is stored. Replaced by: a
  stored probe history (needs ADMIN2-02 first).

### Fake data of the config port

modules/config/src/admin.rs landed in b0e8e377; the line numbers are its committed ones.

- [ ] **Config Edit heading mono key**
  ADMIN2-67. Mockup: the Edit panel title's mono key span (pages-platform.js:96). The
  portal draws "Edit: ns.key" with a separator and no mono span (step 23 checklist H5).
  Closed by: a heading carrier that can mark a code span, drawn by the panel template.

### Fake data and actions of the mail port

modules/mail/src/admin.rs landed in 544dd4e5; the line numbers are its committed ones.

- [ ] **Pause sending**
  ADMIN2-13. Mockup: the outbox "Pause sending" tool. Action `PAUSE_SENDING`,
  `implemented: false`: modules/mail/src/admin.rs:289-300, refused at :568; the drain has
  no pause switch. Replaced by: a persisted pause flag the drain honours.

### Not-implemented actions

- [ ] **Copy payload**
  ADMIN2-16. Mockup: the poison-event "Copy payload" tool. Action `POISON_COPY`,
  `implemented: false`: core/admin/src/events_provider.rs:330-331; copying to the
  clipboard is a client action no backend call performs. Replaced by: a client-side
  copy handler in `admin.js`, or an owner decision to drop the button.

### Fake data and actions of the accounts port

modules/accounts/src/admin.rs, as committed in admin-control-plane-redesign step 13 part a.

- [ ] **Session client and IP**
  ADMIN2-23. Mockup: the Active sessions Client and IP columns. Fakes:
  modules/accounts/src/admin.rs:199 (`fake_client`) and :214 (`fake_ip`), hashes of the
  refresh family id, never the token: a refresh family stores neither. Replaced by: the
  client and address recorded when a family is born.
- [ ] **Suspend account**
  ADMIN2-27. Mockup: the row menu's "Suspend account…" and the player header's "Suspend".
  Actions `SUSPEND` and `SUSPEND_HEADER`, `implemented: false`:
  modules/accounts/src/admin.rs:410 and :413-421; no suspension exists, so every account's
  status is the constant `active`. Replaced by: a suspension state the session checks honour.
- [ ] **Delete account by an operator**
  ADMIN2-28. Mockup: the row menu's "Delete account…". Action `DELETE_ACCOUNT`,
  `implemented: false`: modules/accounts/src/admin.rs:412; deletion is the player's own
  two-step flow. Replaced by: an operator path into the same deletion.
- [ ] **Players total at scale**
  ADMIN2-32. The players page total and the nav counter run an exact `count(*)` over
  `accounts.players` on every load (`Store::player_count`, modules/accounts/src/store.rs),
  and each list page sorts every player, with no index on `created_at` (plan R1). Owner
  decision 2026-11-17 (step 13 G6): exact for now. Replaced by: a `pg_class.reltuples`
  estimate past a size threshold, and an index if the keyset paging needs one.

### Fake data and actions of the apikeys port

modules/apikeys/src/admin.rs, as committed in admin-control-plane-redesign step 21 part a2.

- [ ] **Revoked key's row menu offers Revoke**
  ADMIN2-61. Mockup: a revoked key's row menu holds one disabled "Already revoked" item
  ("revoked keys cannot be restored", UILayout/redesign/assets/pages-platform.js:11). The
  portal draws the same enabled "Revoke…" on every row (modules/apikeys/src/admin.rs:861) and
  answers a revoked key with a no-op "already revoked" (:190), contract gap P2-2: a row
  action cannot be disabled or replaced per entity. Replaced by: a per-entity refusal on a
  row action, drawn as the mockup's disabled item.
- [ ] **Create form role help follows the chosen role**
  ADMIN2-63. Mockup: the create-key form's Role help, the chosen role's scopes as tags
  (UILayout/redesign/assets/pages-platform.js:26). The portal draws the first role's policy
  as plain text (modules/apikeys/src/admin.rs:649), contract gap P2-5: a form field's help is
  static text, neither tags nor bound to the selected option. Replaced by: a per-option
  help on an Enum field.

### Fake data and actions of the audit port

modules/audit/src/admin.rs, as committed in admin-control-plane-redesign step 24 part a1b.

- [ ] **Audit log inline styles**
  ADMIN2-71. Mockup: the search box is 260 px wide (pages-platform.js:120), the Action cell
  has `font-weight:500` and the Target cell `font-size:11.5px` (pages-platform.js:123). The
  portal draws the search box at 220 px (core/admin/src/templates/primitives.html.tmpl:216,
  see ADMIN2-35) and Mono cells without either style; checklist
  docs/plans/2026-11-24-step24-audit-checklist.md H4/H6. Replaced by: the ADMIN2-35 search
  width field set to 260 by audit, and a cell emphasis or size the template draws, or an
  owner decision to keep the shared Mono style.

### Pages without a mockup

- [ ] **Group entity page**
  ADMIN2-51. `/admin/e/group/{id}` (the Groups tab's name link) draws only groups'
  `EntityHeader` (name, join policy and day, member and pending counts) and no tab; no
  mockup page exists for it (checklist docs/plans/2026-11-22-step18-groups-checklist.md
  G7 b). Closed by: a mockup for the group page and its port.

### Mockup pages without a portal page

- [ ] **Characters list and character page**
  The mockup has `UILayout/redesign/characters.html` and `character.html` (drawn by
  `pages-core.js`, with `charLink` and the "character" row menu at `:208`, `:221`, `:262`),
  but the portal has no character page: `modules/characters` declares the `character` kind
  with a label only, and inventory draws a character owner as a label. The player page no
  longer links to them (admin-second-pass steps 8a and 6). Owner 2026-11-24: port these pages
  after the admin-second-pass leftovers are fixed.
  The mockup character page's Items tab carries the inventory grant and revoke
  (`pages-core.js:313`, admin-second-pass step 14), so the port brings them too.

### Guard gaps

- [ ] **Unlisted page-head action renders nowhere**
  ADMIN2-20. The page head draws only the ids in `PageDecl.head_actions`
  (core/admin/src/routes.rs:487-493), so a `PageHead(p)` action that page `p` does not
  list renders nowhere. admincheck checks each listed id (tools/admincheck/src/checks.rs:135-152)
  but does not refuse an unlisted `PageHead` action. Closed by: an admincheck finding for
  it. Source: the 593010bf hand-off.

- [ ] **A row links to an entity header the operator's role cannot see** (low priority, owner 2026-11-18)
  ADMIN2-41. A table row is clickable when its kind has any `EntityHeader`
  contribution (K4, the `header_kinds` set built in core/admin/src/routes.rs), without
  checking the header's `min_role` against the operator. A Viewer gets a clickable row that
  opens an entity page drawing "Requires role …". Not a leak: the entity page enforces
  `min_role`. Closed by: building `header_kinds` only from headers the operator admits.
  Source: Step 13c.1a proof audit.

### Contract gaps admindiff cannot see

- [ ] **"Today" day label on timelines**
  ADMIN2-57. Mockup: the Inbox tab labels the current day "Today"
  (UILayout/redesign/assets/pages-core.js:144-145). The portal labels every day "Mon D"
  (`day_and_time`, core/admin/src/render/tiles.rs:128), so the Inbox tab of
  modules/notifications/src/admin.rs (`days: true`) draws "Nov 23" there. admindiff compares
  no text, so no accept entry covers it; checklist
  docs/plans/2026-11-22-step20-notifications-checklist.md N4/H4. Closed by: a "Today" label
  for the reader's current UTC day in the portal's timeline renderer.

- [ ] **Per-page toolbar search width** (low priority, owner 2026-11-17)
  ADMIN2-35. Mockup: `searchWidth: 280` for players (UILayout/redesign/assets/pages-core.js:98),
  260 for the mail outbox (pages-platform.js:120) and the audit log (ADMIN2-71), 220 elsewhere. The portal's `table_toolbar`
  hard-codes `style="width:220px"` (core/admin/src/templates/primitives.html.tmpl:216), so the
  players box is 60 px and the outbox box 40 px narrower than the mockup. admindiff compares
  no attributes, so no accept entry covers it. Found in step 13 part b. Closed by: a
  `DataTable` search-width field in `core/controlplane` that the template reads, set by
  accounts and mail.

### Known limits (admin-control-plane-redesign step 13c.2b2)

- [ ] **A page can declare a key the portal owns** (owner, 2026-11-19)
  ADMIN2-46. Nothing stops a `PageDecl.params` entry naming `q`, `state`, `cursor`,
  `before`, `range` or `entity`; the route routes or drops it silently. No page does
  today. Closed by: an admincheck finding for a declared portal-owned key. Source: 13c.2b2
  review.
- [ ] **Two filter kinds carry options: `Enum` (chip) and `Select`** (owner, 2026-11-19: design call)
  ADMIN2-47. `FilterKind::Enum` draws a chip and `FilterKind::Select` a select, both with an
  option list. Closed by: an owner ruling whether `Enum` becomes the select (one kind) or
  both stay, with the reason. Source: 13c.2b2 hand-off.

### Known limits (admin-control-plane-redesign step 13c.2a2b)

- [ ] **Modal impact and panel form cells draw no entity label** (owner, 2026-11-19)
  ADMIN2-43. A modal's impact cells (core/admin/src/render/modal.rs:102) draw with no labels,
  and `panel_form_result` (core/admin/src/render/form.rs:191) renders with an empty labels
  map: neither is a slot response, so no label pass runs for them. A modal impact naming an
  entity draws its raw id. Closed by: a label pass over the modal's impact cells (and the
  form result's body) before they are drawn. Source: 13c.2a2b proof audit.
- [ ] **Label refs are decided twice, in the label pass and in the renderer** (owner, 2026-11-19: after the panel is final)
  ADMIN2-44. `labels::refs` (core/admin/src/engine/labels.rs) walks a slot body to pick the
  ids that need a label, and the draw sites (render/cell.rs, render/tiles.rs, render/queue.rs)
  decide again where a label replaces the id. A new draw site added to the renderer but not to
  `labels::refs` draws a raw id. Today only
  `every_golden_draw_asks_a_label_for_exactly_the_refs_its_body_collects` keeps the two in
  step, and only for the drawn goldens. Closed by: two render passes per slot: the first
  records the ids the renderer asks a label for, the label pass fetches exactly those, the
  second draws with labels; `labels::refs` and the equality test die. Needs a render free
  of side effects. Source: 13c.2a2b review.

### Known limits (admin-control-plane-redesign step 28)

- [ ] **The enum gate counts a variant once per enum, not once per draw site** (owner, 2026-11-18: after the panel is final)
  ADMIN2-42. `render::seen` records (enum, variant), so a variant converted at one site
  counts as covered at every site. Example: `Tone::Accent` is drawn by `style::tone`,
  `delta_class` and `bar_tone`; a wrong `bar_tone` arm would pass the gate if no golden drew an
  accent bar. Closed by: recording (enum, variant, site) and requiring a golden per site, or
  per-site goldens where risk shows. Source: Step 13c.1g proof audit.
- [ ] **Confirm `InvokeOutcome` stays declared undrawn** (owner, 2026-11-18)
  ADMIN2-36. `InvokeOutcome` is `variants! { undrawn; .. }` (core/controlplane/src/lib.rs),
  as the plan names "outcome" never converted. `routes.rs:1196-1211` turns each outcome into
  a toast, reveal, redirect or conflict response, so the enum gate does not see a new
  outcome variant. Closed by: an owner ruling; if drawn, a `seen::record` at that site and
  golden cases for every outcome. Source: Step 28 round-2 proof audit.
- [ ] **Declaration-form scan misses some enum spellings**
  ADMIN2-37. `every_controlplane_enum_is_declared_drawn_or_undrawn`
  (core/admin/src/golden_tests/declared.rs) matches only lines that start with `pub enum`, so an
  attribute on the same line escapes it, and its naive brace count lets an unbalanced `{` in
  a string or doc comment inside a `variants!` block hide a bare enum after it. Nothing in
  controlplane uses either form today. Closed by: reading the declared set from
  `docs/reference/public-api-baseline/controlplane.txt` (`pub enum` lines against `impl
  controlplane::Variants for` lines) instead of the source. Source: Step 28 round-2 review.
- [ ] **Screen goldens assemble their views by hand**
  ADMIN2-38. The dashboard, page and entity goldens build `DashboardView`, `PageView` and
  `EntityView` in the test from production helpers (`routes::slot`, `action_item`,
  `render::screen_page`, `layout_rows`), because the route builders are async handlers
  needing a `Portal`, auth and a snapshot. Enum coverage is exact, but a route that puts a
  slot in the wrong field is not caught. Closed by: pure view builders lifted out of the
  `page`/`dashboard`/`entity` handlers and called from the goldens. Source: Step 28
  round-1 and round-2 proof audits.

## Accepted contract gaps

One item per `contract gap` entry in `tools/admindiff/accept/*.txt`; remaining mockup
gaps go to the next iteration.

- [ ] **Config revision restore link**
  ADMIN2-39. Accept entry: the `contract gap` line for the Revisions timeline's per-entry
  "restore" link, written by the config port (Step 23) in its page's accept file, since no
  accept file maps the config page yet. Mockup: pages-platform.js:104-105. A `TimelineEntry`
  carries no action (core/controlplane/src/lib.rs, `TimelineEntry`), so the portal draws no
  link (admin-control-plane-redesign step 13c.1b, K10). It also covers the entries'
  `span.mono` value meta (pages-platform.js:103-105): `TimelineEntry.detail` is plain
  text (step 23 checklist P2-6). Closed by: a per-entry action and a code-marked detail on
  `TimelineEntry` drawn by the timeline macro; then delete the accept line.
- [ ] **Config pending-change preview**
  ADMIN2-68. Accept entry: the `contract gap` line for the Edit panel's muted "Pending
  change vs rev N" preamble and its `div.code` diff block, written by the config port
  (Step 23) in its page's accept file. Mockup: pages-platform.js:101-102. No carrier
  draws an unsaved change against the stored revision (step 23 checklist D6). Closed by:
  a diff-preview carrier on the form or timeline; then delete the accept line.

## Mail nav counter with only parked rows

With only parked rows the mail nav counter shows a warn badge reading "0". That matches
the Step 11 checklist (the count is queued pending plus retrying; the warn tone comes
from retrying plus parked), but it reads oddly. Option: show the parked count when
nothing is queued. Owner design decision. Source: b5eb40d5.

## Data a dev fleet never produces

`data difference` accept entries in `tools/admindiff/accept/*.txt` that are real data
the fleet lacks, not fakes, for the second pass to decide on (seed, or accept for good).
At the time of writing no entry in `index.txt` or `events.txt` has that kind: they are
`not ported yet`, `template choice`, `contract gap` or a declared deviation, and no mail
accept file exists yet. A port's `data difference` entries are listed here in its part
`b` or `a` commit (plan Step 24b is where seeding is decided).

_None._
