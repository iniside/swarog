# Admin ports 14–20: contract gaps against the current control-plane contract (2026-11-20)

**Completed by a second pass 2026-11-20.** The first pass stopped on a context budget; the
second pass read the files listed under its Not read, re-checked every item, and rewrote the
tables. Items it changed are marked *(2nd pass)*.

**Question (owner, 2026-11-20).** Before the admin ports (Steps 14–20 of
`docs/plans/2026-10-22-1759-admin-control-plane-redesign-plan.md`, plan:2090-2149) start, which
mockup elements can the current control-plane contract plus renderer NOT draw in their exact
shape (tags, classes, nesting)? Goal: every contract gap found now, so they all land in one
contract step.

**Method.** Targeted reads only; rust-analyzer was not used and nothing was run (no cargo, no
admindiff). Pass 1: `pages-core.js` (whole), `registry.js` (whole), `pp` 1-30 and 156-186,
`bundle.js` (whole), `cp` 351-406, 497-800, 900-1185, 1280-1531, `tmpl` (whole), plan
1880-2340, and the prior sweep `docs/research/2026-11-17-ports-contract-gaps.md` (K1–K21).
Pass 2: `core/admin/src/render/{table,cell,action,shell}.rs` (whole), `render/form.rs` 1-60,
`shell.html.tmpl` and `modal.html.tmpl` (whole), `tmpl` 60-462 again, `engine/labels.rs`
15-88, `routes.rs` 592-612, 740-762, 830-858, `pp` 1-186 (whole), `pc` 118-193, `bjs`
85-225 and 276-284, `cp` 366-392, 560-610, 636-770, 970-1060,
`tools/admindiff/src/skeleton.rs` 1-70, `tools/admincheck/src/checks.rs` 60-120, the
accept files in `tools/admindiff/accept/` **[grep]**, and `pages.rs` **[grep]** only.
Matching is on tag, class and nesting, which is what `admindiff diff` compares
(skeleton.rs:1-8); attributes are never compared (skeleton.rs:32-33) and text only with
`--text`, so text and attributes are a hand-check. admindiff unwraps only `.frag` on the
mockup side and `.slot` on the portal side (skeleton.rs:25-34): every other portal wrapper
(`div.stack`, `form`) is a diff line.

Abbreviations: `pc` = `UILayout/redesign/assets/pages-core.js`, `pp` = `…/pages-platform.js`,
`reg` = `…/registry.js`, `bjs` = `UILayout/design-system/components/bundle.js`,
`cp` = `core/controlplane/src/lib.rs`, `tmpl` = `core/admin/src/templates/primitives.html.tmpl`,
`shell` = `…/templates/shell.html.tmpl`, `table.rs` etc. = `core/admin/src/render/*.rs`.

## Resolved questions (pass 1 "Not verified")

| Item | Answer | Anchor |
|---|---|---|
| V1 pager foot / toolbar | Foot draws when `next_cursor` is set or `total` is `Some` with rows; `pager:false` ⇔ `PageInfo{total: None, next_cursor: None}`. Toolbar draws when filters or a search placeholder exist (mockup: when `filters` exist, bjs:155). Foot text is portal-composed: `"N items · showing 1–M"` / `"N of M"`, and the page label has no thousands separator (`"1 / 98112"`): text-only differences against "37 items", "7 of 2.4 M transactions", "1 / 98,112" | table.rs:82-91, 199-209, 261, 266 |
| V2 shell | Tab error dot `span.dot.err`, lock `span.muted > svg`, count via slot → `span.count` all match pc:167; tab tiles row `div.grid.g-tiles` matches pc:128; header actions `button.btn` match pc:166 | shell:65-68, tmpl:406 |
| V3 label pass | Reads **any** cell kind whose `link` is `Link::Entity(e)` and `value == e.id`; an unlinked cell is never labelled | labels.rs:15-20, 66 |
| V4 `Layout::Rows` | Flattens to one row per sub-layout: a stack row is `div.stack`, a grid row is `div.grid.<g> > div.stack` per column | shell.rs:185-193, shell:55-58 |
| V5 nav `down` | `a.nav-item.down` with title, count hidden: matches reg:140 | shell:29 |
| V6 `Tone::Accent` | → `accent` → `span.ico.accent` | style.rs:62, tiles.rs:146, tmpl:250 |
| Row menu decision | `menu_url` is set when the row entity's kind has any `RowAction`; `href` (clickable) when it has an `EntityHeader` contribution | table.rs:219, 229-233; routes.rs:743-759 |
| G5 tab panels diffed | Not yet for 14–20: `EXCLUDED_TABS` lists `player.characters` … `player.inbox` as "not ported yet" **[grep]**; each port removes its line and adds a tab entry | pages.rs:285-294 |
| Modal shapes | The mockup has **no** modal for Grant/Revoke currency, Grant item, Notify or Send notification: they toast (reg:90, 102-106) or have no handler (pc:130, 142). The portal draws the generic form modal (modal.html.tmpl:5-25); nothing to diff, but each portal modal opener on a compared tab needs its pages.rs modal bookkeeping entry | pages.rs:308 **[grep]** |

## Known items, re-checked

| Item | Status at HEAD | Anchor |
|---|---|---|
| G1 / K1 `span.mono.muted` | **closed**: `CellKind::MonoMuted` | cp:986-987, tmpl:78 |
| G4 / K21 refs bound | **closed**: `MAX_PANEL_REFS = 100` | cp:208 |
| ADMIN2-40 seg (K16) | **still a GAP** (W1) | cp:727-766, tmpl:278-283 |
| X8 `request_id` → `idempotency_key` | the portal's request id is the minted form-instance id; see W2 | routes.rs:1146 **[grep]**, cp:451 |
| groups promote (Q1) | not a contract gap: `TableRow` bound to `Row.item`; extra `th`/`td` (tmpl:218, 224) needs a D6 accept line | cp:684-687, 1038-1041 |

## Per port

Port traps (no gap, but the obvious kind is wrong): a plain relative time in the mockup
("4 min ago", "2025-03-14") is `CellKind::Text`, not `Time` (`Time` draws `span.dim`,
tmpl:84) — characters Last played (pc:127), friends Since (pc:135), groups Joined (pc:137),
inventory Acquired (pc:132), storage Updated (pc:141). Every table with a search box carries the
existing `span.input-icon` / `form.input-icon` accept pair (tmpl:202; accept/mail.txt:11-12).
Every single-panel tab is drawn inside `div.stack` (shell:69) where the mockup puts the panel
bare (pc:169): a template-choice accept pair per tab, as accept/mail.txt:13-14 does for pages.

### Step 14 — characters

| Element | Mockup | Carrier | Proposal |
|---|---|---|---|
| Id `span.mono.muted` | pc:127 | `MonoMuted` tmpl:78 | none |
| Region `span.tag` | pc:127 | `Ref` tmpl:81 | none |
| No pager | pc:127 | `PageInfo{total: None, next_cursor: None}` table.rs:266 | none *(2nd pass: V1 closed)* |
| Tab count | reg:68 | `EntityTabCount` → `span.count` tmpl:406 | none |

**No contract gap.** Data decisions G2, G3, G7, G8, G10, G11 stay with the port.

### Step 15 — wallet

| Element | Mockup | Carrier | Proposal |
|---|---|---|---|
| **W1** `div.seg > button.on + button` above `div.form` in the panel body, switching the submit | pp:172 | **GAP (confirmed)**. `FormSpec` has no modes (cp:727-735), `panel_form` draws none (tmpl:278-283) | O-1; `FormSpec.modes` (C4) |
| **W2** "Request id" readonly mono field holding the id the submit sends | pp:176 | **GAP (confirmed, sharpened)**. The portal's `request_id` is the form-instance id it mints when it draws the panel (routes.rs:833-834, 1146 **[grep]**); the provider builds `Primitive::Form` before that and cannot know it. A provider-minted default in a readonly field (form.rs:59) draws the shape but is a **second** key, not the one sent | O-2: (a) drop as a declared deviation; (b) portal-filled `FieldKind::RequestId` (C5); (c) provider-minted field used as the idempotency key, against X8 |
| Panel `form` wrapper and hidden inputs | pp:172 (none) | `panel_form` wraps `div.form` in `form` + `input[hidden]` (tmpl:280) | **new (2nd pass), not contract**: template-choice accept lines; no Form panel is diffed yet, so this is the first |
| Cap / Reason `span.dim` | pp:169, 181 | **near-gap**: only `Time` draws `span.dim` (tmpl:84); `Muted` is `span.muted` (tmpl:77) | S1 |
| Currencies `g-21` beside Grant, Ledger full width below | pp:183 | `Layout::Rows([Grid21, Stack])` draws `div.grid.g-21 > div.stack > panel` ×2, then `div.stack > panel`; the mockup has bare panels (shell:56-57) | **not contract (2nd pass)**: template-choice accept lines, precedent accept/players.txt:11-12 |
| Grant banner `div.banner.err` while wallet-svc is down, form disabled | pp:171, 177 | `body.banner` (tmpl:437/439) draws the shape; but a module that does not answer gives the panel the error state, so the mockup's "ok + banner" state is unreachable | **state deviation (2nd pass)**, owner hand-check, no contract change |
| Stale Currencies / Ledger with custom text | pp:167-168 | stale banner tmpl:434 (fixed text "…-svc is lagging") | text only |
| Kind `badge.plain[.accent]`, Amount `span.mono` coloured, Code/Currency `span.tag`, Tx `span.mono.muted` | pp:169, 181 | `Label`+tone tmpl:76, `Mono`+tone tmpl:83, `Ref`, `MonoMuted` | none |
| Player `a > span.user-cell` labelled from accounts | pp:181, bjs:102-106 | `User` + `Link::Entity(player)`, value = id, `Row.entity = None` → `a[style] > span.user-cell` (tmpl:90) and label (labels.rs:15-20) | none *(2nd pass: V3 closed)* |
| Ledger foot "7 of 2.4 M transactions", "1 / 98,112" | pp:182 | table.rs:199-209, 87 | text only |
| Grant form fields: user ref, money, reason, mono | pp:173-176, bjs:212-214 | Ref tmpl:263, Money tmpl:264, `Field.mono` | none |
| Tab tiles (one per currency), error state | pc:128 | `EntityTabTile` + `Kpis` (tmpl:381), tile row shell:68 | none |
| Tab error dot; row-menu group "unavailable" | reg:69, 101-104 | shell:66; tmpl:150-151 | none |
| Nav `a.nav-item.down` | reg:25, 140 | shell:29 | none *(2nd pass: V5 closed)* |
| Wallet search provider | reg:41 | `SearchDecl` | scope gap, O-5 |

### Step 16 — inventory

| Element | Mockup | Carrier | Proposal |
|---|---|---|---|
| **I1** `td.act` more button on a row that is not `tr.clickable` | pc:133 | **dropped as a contract gap (2nd pass)**. `menu_url` needs only a `RowAction` on the row entity's kind; `href` needs an `EntityHeader` (table.rs:219, 229-233; routes.rs:743-759). Inventory declares a kind (e.g. `item`, admincheck requires it declared, checks.rs:106-112) with `RowAction`s and no `EntityHeader`; rows carry `Row.entity = item` | O-6 (pattern decision, not contract) |
| **I2** Owner `span.mono` "character:Yuki" — another module's label, unlinked | pc:132 | **near-gap, new (2nd pass)**. The label pass labels only a cell linked to the entity (labels.rs:15-20); a linked `Mono` draws `a.mono` (tmpl:89), not `span.mono`. The label replaces the value, so the `character:` prefix is lost | O-7: (a) linked label (`a.mono`, one accept line), (b) unlabelled raw id text, (c) `Cell.label: Option<EntityRef>` (C3) |
| Source `span.mono.muted`; Type `span.tag` | pc:132 | `MonoMuted`, `Ref` | none |
| Toolbar search + chips; foot "37 items", "1 / 2" | pc:131-133 | tmpl:200-211 | none (text only, input-icon accept pair) |
| Grant item tool / row-menu item | pc:130, reg:105 | `PanelTool`, `RowAction`, `implemented:false` → disabled | attribute only |

### Step 17 — friends

| Element | Mockup | Carrier | Proposal |
|---|---|---|---|
| **F1** linked user cell `a > span.user-cell` **and** `td.act` more button, row not clickable | pc:135 | **dropped as a contract gap (2nd pass), with a trap**. With `Row.entity = player` the row is clickable (accounts declares `EntityHeader`) and the user cell's link moves inside it (`span.user-cell > … > a`, table.rs:159-163, tmpl:82) — a nesting diff. Same pattern as I1: friends declares a row kind (e.g. `friendship`) with `RowAction`s and no header; the user cell links `Link::Entity(player)` itself and is labelled | O-6 |
| State badge info/ok; foot "12 friends", "1 / 1" | pc:135 | `Badge`+tone tmpl:75; `total = Some(12)` → foot, label "1 / 1" (table.rs:88) | none |

### Step 18 — groups

| Element | Mockup | Carrier | Proposal |
|---|---|---|---|
| Group name `a[href]` | pc:137 | `Text` + link → `a` (tmpl:90); `Link` is `Page` or `Entity` only (cp:372-375) | O-4 destination |
| Role `badge.plain`; no pager | pc:137 | `Label`; `PageInfo` none | none |
| Promote (extra) | — | `TableRow` | D6 accept line |

### Step 19 — storage

| Element | Mockup | Carrier | Proposal |
|---|---|---|---|
| **St1** Download all, per-row download, row-menu "Download cloud saves" | pc:140-141, reg:111 | **GAP (confirmed)**. `Then` is Invoke/Form/Confirm/Link (cp:708-713), `Link` Page/Entity (cp:372-375); nothing serves bytes | O-3 |
| **St2** Enabled menu item with `span.hint` "read-only" | reg:111, bjs:282 | **GAP (confirmed)**: `menu_item` draws `span.hint` only when disabled (tmpl:133-138); `ActionDecl` has no hint (cp:636-656) | S3 / C1 |
| **St3** Icon-only row button | pc:141 | **near-gap, visual only (2nd pass)**: tag shape matches (`td > button.btn.sm.ghost > svg`, tmpl:124-129, 218, 224); the extra label is text, invisible to the diff | S3 / C1 |
| Read-only chip, custom banner, tab lock; Delete disabled | pc:138-140, reg:73 | `ContributionDecl.readonly` cp:601-603, tmpl:423, 435; shell:66; tmpl:116 | none |
| Size plain text in `td.num` | pc:141 | `Bytes` has no template branch → plain value (tmpl:86) | none |
| Dashboard tile "Cloud saves used" | reg:49 | `KpiTile`, `Palette::Chart2` | none |

### Step 20 — notifications

| Element | Mockup | Carrier | Proposal |
|---|---|---|---|
| Inbox timeline: `div.tl-day`, `ul.tl > li > span.when + span.ico.accent + div.body > div + div.meta`, bold title | pc:143-148, bjs:192-200 | tmpl:247-253; `Timeline.days`; tone style.rs:62 | none *(2nd pass: V6 closed)* |
| Tool "Send notification"; header "Notify"; row "Send notification…" | pc:142, reg:90, 106 | `PanelTool`, `HeaderAction`, `RowAction` | none (form fields: plan:2148-2149) |

## Shared gaps

- **S1. Dim text cell** (`span.dim`): wallet Cap and Reason (pp:169, 181). `CellKind::Dim`, or
  bless `Time` as the dim carrier (no contract change). Confirmed.
- **S2. Row menu without row click** — *dropped as a contract gap (2nd pass)*. Expressible today
  by a module-owned row kind with `RowAction`s and no `EntityHeader` (inventory I1, friends F1).
  Remains an owner decision on the pattern (O-6), because K4 (plan:2251-2252) assumed rows of
  header kinds.
- **S3. Action display fields** `ActionDecl.hint` (enabled-item hint, St2) and
  `ActionDecl.icon_only` (St3, visual only). Confirmed.
- **S4. Download carrier** (storage only). O-3.
- **S5. Label without link** — *new (2nd pass)*. The label pass keys on `Link::Entity`, so a
  labelled cell is always a link (I2). Only inventory Owner among 14–20.
- **R1. Renderer wrappers, not contract** — *new (2nd pass)*: `div.stack` around single-panel
  tabs (shell:69) and around each `Layout` column (shell:56-57), and the panel `form` wrapper
  (tmpl:280). admindiff does not unwrap them (skeleton.rs:34); each port carries template-choice
  accept lines (precedent accept/players.txt:11-12, accept/mail.txt:13-14). Changing the
  renderer instead is optional and does not touch the contract.

## Proposed single contract step (dependency order)

Each item gets one golden case in `core/admin/src/golden_tests/`; each new variant or field is
caught by the public-api baseline, and a new enum variant by Step 28's variant-coverage gate
(plan:3173).

1. **C1** `ActionDecl.hint: Option<String>` drawn on enabled menu items, and
   `ActionDecl.icon_only: bool` (label moved to `title`, as `queue_action` does, tmpl:291-298).
   Golden: an enabled menu item with `span.hint`; an icon-only `TableRow` button.
2. **C2** `CellKind::Dim` → `span.dim` (S1), if chosen over blessing `Time`. Golden `cell_dim`.
3. **C3** `Cell.label: Option<EntityRef>` — a ref the label pass labels without linking (S5),
   only if O-7 (c). Golden: an unlinked `span.mono` holding another kind's label; the label
   pass test that it is asked.
4. **C4** `FormSpec.modes` — `div.seg` before `div.form`, one `PanelForm` action per mode (W1),
   after O-1. Golden: seg + form, the on-mode's submit label. Closes ADMIN2-40.
5. **C5** portal-filled request-id field (W2), only if O-2 (b). Golden: readonly mono field; a
   test that the drawn value equals the `InvokeCall.request_id` the submit sends.
6. **C6** download carrier (S4), only if O-3 (b): `Then::Download`, a portal route over a new
   `cp` call, a `tools/conformance` policy row and a splitproof assertion. Probably its own step.

S2 needs no contract item; if O-6 rejects the row-kind pattern, add `Row.menu: Option<EntityRef>`
between C2 and C3 (golden: a non-clickable row with `td.act` and an outer-linked user cell).

## Data gaps (not contract)

Global ledger (FAKE, R1); Embers currency choice (plan:2126); characters Level/Region/Last
played (FAKE); storage version/checksum/quota (FAKE or DERIV); inventory count meaning and owner
text; friends pending direction and the row-kind id; notifications title/body mapping; wallet
ok-state fact wording; foot texts (table.rs:199-209) and the page label's missing thousands
separator (table.rs:87).

## Owner decisions

- O-1 seg: build `FormSpec.modes` (C4) or keep the ADMIN2-40 accept.
- O-2 request-id field: deviation, portal-filled field (C5), or provider key against X8.
- O-3 storage downloads: `implemented:false` or a download carrier (C6).
- O-4 group link destination.
- O-5 wallet search provider: in Step 15 or not.
- O-6 row menu without click: module-owned row kinds without a header (no contract change) or
  `Row.menu`.
- O-7 inventory Owner: linked label, raw id text, or `Cell.label` (C3).
- O-8 dim cell: `CellKind::Dim` (C2) or `Time` as the dim carrier.
- O-9 renderer wrappers (R1): accept lines per port, or drop the `div.stack` wrappers in
  `shell.html.tmpl`.

## Not verified

- Whether `panel.rs` sources `p.meta` ("37 items · owner user:…") from the body for a tab panel
  (only tmpl:423 read).
- Whether `admincheck` has another check that rejects an entity kind with `RowAction`s but no
  `EntityHeader` (checks.rs 60-120 read only), and what `/admin/e/item/…` draws for such a kind.
- Whether a notification-popover entry (reg:115-121) has a contribution point; the shell draws
  `shell.notifications` (shell:79-81), its producer not read.
- Wallet dashboard tile and attention row (reg:52, 58) against the Step 15 bullets.
