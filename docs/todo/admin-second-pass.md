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

- [ ] **Deployment name, version and process count**
  ADMIN2-01. Mockup: top-bar deployment and the dashboard subtitle ("emberline-prod ·
  Swarog 0.42.1 · 20 processes (split topology)"). Fake: core/admin/src/fakes.rs:8,11
  (`DEPLOYMENT`, `DASHBOARD_SUBTITLE`): the portal has no deployment name and no
  version or process-count source. Replaced by: a deployment name in config and the
  fleet's version and process count from Weles.
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

- [ ] **Config revision writers**
  ADMIN2-65. Mockup: the "by <user>" actor of each Revisions entry. Fake:
  modules/config/src/admin.rs:183 (`fake_by`): config.settings stores no writer.
  Replaced by: the writer recorded with each revision.
- [ ] **Config prior revisions**
  ADMIN2-66. Mockup: the Revisions entries before the current one (rev N-1, N-2 and
  their old values). Fake: modules/config/src/admin.rs:191 (`fake_prior`), :351
  (`revisions_body`): no history is kept, only each key's current value. Replaced by:
  a stored per-key revision history.
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
- [ ] **Retry now**
  ADMIN2-14. Mockup: the per-row "Retry". Action `RETRY_NOW`, `implemented: false`:
  modules/mail/src/admin.rs:302-303; the drain owns every retry time. Replaced by: a
  per-row "due now" operation on the outbox.
- [ ] **Park and cancel parked**
  ADMIN2-15. Mockup: "Park: keep but stop retrying" and the parked row's "Cancel".
  Actions `PARK` and `CANCEL_PARKED`, `implemented: false`: modules/mail/src/admin.rs:304-306
  and :313; no operator park exists (resume parked is real) and `cancel` refuses a row
  that is not pending. Replaced by: an operator park transition and a cancel for parked rows.

### Not-implemented actions

- [ ] **Scheduler Fire now**
  ADMIN2-64. Mockup: the Schedules row button's "Fire now". Action `schedule.fire`,
  `implemented: false` and `Then::Invoke`: modules/scheduler/src/admin.rs (`manifest`);
  the scheduler has no write path. Replaced by: fire now as `Then::Invoke` (an immediate
  fire under the advisory lock), with `writes: true` and a `cp.invoke scheduler`
  conformance case.
- [ ] **Friends remove friend**
  The player's Friends tab row-menu "Remove friend…" on a `friendship` row is
  `implemented: false`; tracked as FRIENDS-08 in `docs/todo/friends.md`.
- [ ] **Inventory grant and revoke**
  The player's Inventory tab "Grant item" tool, the row-menu "Grant item…" and the holding
  "Revoke item…" are `implemented: false`; tracked as INVENTORY-05 in `docs/todo/inventory.md`.
- [ ] **Copy payload**
  ADMIN2-16. Mockup: the poison-event "Copy payload" tool. Action `POISON_COPY`,
  `implemented: false`: core/admin/src/events_provider.rs:330-331; copying to the
  clipboard is a client action no backend call performs. Replaced by: a client-side
  copy handler in `admin.js`, or an owner decision to drop the button.

### Fake data and actions of the accounts port

modules/accounts/src/admin.rs, as committed in admin-control-plane-redesign step 13 part a.

- [ ] **Identity last used**
  ADMIN2-22. Mockup: the Linked identities Last used column. Fake:
  modules/accounts/src/admin.rs:193 (`fake_last_used`): no identity records its last use.
  Replaced by: a last-used time written on each login through the identity.
- [ ] **Session client and IP**
  ADMIN2-23. Mockup: the Active sessions Client and IP columns. Fakes:
  modules/accounts/src/admin.rs:199 (`fake_client`) and :214 (`fake_ip`), hashes of the
  refresh family id, never the token: a refresh family stores neither. Replaced by: the
  client and address recorded when a family is born.
- [ ] **Export CSV**
  ADMIN2-25. Mockup: the Users & players "Export CSV" head action. Action `EXPORT_CSV`,
  `implemented: false`: modules/accounts/src/admin.rs:406; no export exists. Replaced by:
  an export of the filtered list.
- [ ] **Create account**
  ADMIN2-26. Mockup: the "Create account" head action. Action `CREATE_ACCOUNT`,
  `implemented: false`: modules/accounts/src/admin.rs:408; an account is created only by
  the player's own registration. Replaced by: an operator create path.
- [ ] **Suspend account**
  ADMIN2-27. Mockup: the row menu's "Suspend account…" and the player header's "Suspend".
  Actions `SUSPEND` and `SUSPEND_HEADER`, `implemented: false`:
  modules/accounts/src/admin.rs:410 and :413-421; no suspension exists, so every account's
  status is the constant `active`. Replaced by: a suspension state the session checks honour.
- [ ] **Delete account by an operator**
  ADMIN2-28. Mockup: the row menu's "Delete account…". Action `DELETE_ACCOUNT`,
  `implemented: false`: modules/accounts/src/admin.rs:412; deletion is the player's own
  two-step flow. Replaced by: an operator path into the same deletion.
- [ ] **Edit profile**
  ADMIN2-29. Mockup: the Profile panel's "Edit". Action `EDIT_PROFILE`,
  `implemented: false`: modules/accounts/src/admin.rs:423; the display name is immutable
  (ACCOUNTS-09). Replaced by: editable profile fields.
- [ ] **Link and unlink identity**
  ADMIN2-30. Mockup: the Linked identities "Link" tool and each row's "Unlink". Actions
  `LINK_IDENTITY` and `UNLINK_IDENTITY`, `implemented: false`:
  modules/accounts/src/admin.rs:425 and :427; linking is the player's own flow and no
  unlink exists. Replaced by: operator link and unlink, refusing the last identity.
- [ ] **Revoke sessions**
  ADMIN2-31. Mockup: the Active sessions "Revoke all" tool and each row's "Revoke".
  Actions `REVOKE_ALL` and `REVOKE_SESSION`, `implemented: false`:
  modules/accounts/src/admin.rs:429-430; no operator revocation exists. Replaced by: an
  operator call into the family revocation (`Store::kill_family_tx`).
- [ ] **Players total at scale**
  ADMIN2-32. The players page total and the nav counter run an exact `count(*)` over
  `accounts.players` on every load (`Store::player_count`, modules/accounts/src/store.rs),
  and each list page sorts every player, with no index on `created_at` (plan R1). Owner
  decision 2026-11-17 (step 13 G6): exact for now. Replaced by: a `pg_class.reltuples`
  estimate past a size threshold, and an index if the keyset paging needs one.

### Fake data of the characters port

modules/characters/src/admin.rs, as committed in admin-control-plane-redesign step 14a1.

- [ ] **Character level**
  ADMIN2-48. Mockup: the Characters tab's Level column. Fake: modules/characters/src/admin.rs:113
  (`fake_level`, 1–60 from `controlplane::fake_hash` of the character id): characters stores
  no level. Replaced by: a level the game server reports for each character.
- [ ] **Character region**
  ADMIN2-49. Mockup: the Characters tab's Region column. Fake: modules/characters/src/admin.rs:118
  (`fake_region`, a pick from the mockup's region shapes): characters stores no region.
  Replaced by: the region a character last played in.
- [ ] **Character last played**
  ADMIN2-50. Mockup: the Characters tab's Last played column ("4 min ago"). The portal draws
  "—" (modules/characters/src/admin.rs:144, owner 2026-11-20: not faked): characters stores no
  last-played time. Replaced by: a last-played time the game server reports, drawn as an age.

### Fake data and actions of the storage port

modules/storage/src/admin.rs, as committed in admin-control-plane-redesign step 19 part a2.
The four downloads and Delete are drawn disabled; storage's manifest declares `writes: false`.

- [ ] **Cloud saves used sparkline**
  ADMIN2-52. Mockup: the "Cloud saves used" tile's sparkline (UILayout/redesign/assets/registry.js:49).
  Fake: modules/storage/src/admin.rs:241 (`fake_spark`, seven points from
  `controlplane::fake_hash("storage.tile.spark", "dashboard")`): storage keeps no history of
  its byte total. Replaced by: a stored-bytes series from a metric store.
- [ ] **Download all cloud saves**
  ADMIN2-53. Mockup: the Cloud saves tab's "Download all" tool. Action `DOWNLOAD_ALL`,
  `implemented: false`: modules/storage/src/admin.rs:318; no carrier serves bytes to the
  portal (`docs/todo/storage.md` "Admin download carrier for cloud saves"). Replaced by: that
  carrier.
- [ ] **Download one cloud save**
  ADMIN2-54. Mockup: the icon-only download button on each Cloud saves row. Action
  `DOWNLOAD`, `implemented: false`: modules/storage/src/admin.rs:323; same missing carrier as
  ADMIN2-53.
- [ ] **Download cloud saves from the player row menu**
  ADMIN2-55. Mockup: the player row menu's "Download cloud saves" with the hint "read-only"
  (UILayout/redesign/assets/registry.js:111). Action `DOWNLOAD_PLAYER`, `implemented: false`:
  modules/storage/src/admin.rs:326; drawn disabled with "Not implemented yet" instead of
  enabled with the hint. Same missing carrier as ADMIN2-53.
- [ ] **Delete cloud saves (by design: no operator delete)**
  ADMIN2-56. Mockup: the Cloud saves tab's "Delete" tool, which the mockup itself draws
  disabled. Action `DELETE`, `implemented: false`, `danger`: modules/storage/src/admin.rs:319.
  By design: storage exposes saves for inspection only (the tab's read-only banner), so no
  operator delete exists. Replaced by: nothing unless an operator delete path is planned.

### Fake data and actions of the apikeys port

modules/apikeys/src/admin.rs, as committed in admin-control-plane-redesign step 21 part a2.

- [ ] **API key created by**
  ADMIN2-58. Mockup: the Keys panel's "Created by" operator login. Fake:
  modules/apikeys/src/admin.rs:414 (`fake_creator`, a login from `FAKE_CREATORS` picked by
  `controlplane::fake_hash("apikeys.created_by", name)`): keys record no creator. Replaced by:
  a creator column written from the admin session on create.
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

- [ ] **Players list column chooser ("Columns: 9 of 12")**
  ADMIN2-33. Accept entry: the `contract gap` line for the toolbar's "Columns:" label and
  "9 of 12" button in tools/admindiff/accept/players.txt (written by step 13 part b).
  Mockup: pages-core.js:100. No column chooser exists and `DataTable` has no toolbar-right
  carrier (owner decision 2026-11-17, step 13 G5). Closed by: a column chooser in
  `core/controlplane` and the portal; then delete the accept line.
- [ ] **Personal data panel**
  ADMIN2-34. Accept entry: the `contract gap` mock-only Personal data panel in
  tools/admindiff/accept/player.txt (written by step 13 part b). Mockup: pages-core.js:120,
  a panel denied by the permission `accounts.pii.read`. No PII store exists, and the owner
  declined a `Forbidden` → denied panel mapping (2026-11-17, step 13 G9), so accounts
  contributes no such panel. Closed by: a PII store and a permission-denied panel state;
  then delete the accept line.
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
