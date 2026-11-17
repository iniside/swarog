# Step 13 (accounts): checklist

Read-only extraction per `.agents/skills/mockup-implementation/SKILL.md` §1. Mockup pages:
`UILayout/redesign/players.html` (`PAGES.boot("players")`) and `player.html`
(`PAGES.boot("player")`). Builders: `P.players` at `pages-core.js:71-112` (columns `:72-78`,
rows `:83-96`, table `:97-103`, head `:104`, panel and banner `:105-106`, th wrap `:109-112`)
and `P.player` at `:156-171` with `tabRender.profile` at `:118-125`. Rows: `data.js:3-18`
(`D.users`). Registry: `registry.js:9,18` (nav), `:38` (search), `:46` (tile), `:66-76`
(tabs), `:79-85` (facts), `:88-92` (header actions), `:95-112` (row actions).

Abbreviations: `pc` = `UILayout/redesign/assets/pages-core.js`, `reg` =
`UILayout/redesign/assets/registry.js`, `bjs` = `UILayout/design-system/components/bundle.js`,
`cp` = `core/controlplane/src/lib.rs`, `tmpl` = `core/admin/src/templates/primitives.html.tmpl`,
`shell` = `core/admin/src/templates/shell.html.tmpl`, `rr` = `core/admin/src/render.rs`, `rt` =
`core/admin/src/routes.rs`, `pn` = `core/admin/src/engine/panels.rs`, `al` =
`modules/accounts/src/lib.rs`, `st` = `modules/accounts/src/store.rs`, `ds` =
`docs/research/2026-10-22-1830-admin-redesign-data-sources.md`, `plan` =
`docs/plans/2026-10-22-1759-admin-control-plane-redesign-plan.md`.

Comparison scope (as in the mail checklist):
- Tags, classes and nesting only; text needs `--text`, attributes are never compared
  (`tools/admindiff/src/skeleton.rs:1-8`). Labels, totals and crumbs are R3 rows the review
  checks by hand.
- The mock's `.frag` wrappers are unwrapped and `.frag-label` removed (`skeleton.rs:25-30`).
  So a contributed list-column header is `th > span(label)` (`pc:111`), a fact is
  `span > …` (`pc:165`), a tab is a bare `button.tab` (`pc:167`).
- Only `tbody>tr`, `ul>li`, `.svc`, `.pop-row` and `.uptime>i` collapse (`skeleton.rs:126-137`).
  `thead` `th`s, `div.tabs` buttons and `div.entity-facts` spans compare by position.
- A panel with no `.panel-title` is keyed `#<position>` (`skeleton.rs:274-276`). The players
  table panel and the entity header have none (`pc:105`, `pc:168`).
- The row menu, palette and modals sit outside `.app` and are checked by hand.
- Crawl operator: devctl mints `admin` with role `admin` (`tools/devctl/src/supervisor.rs:416`).

What accounts holds (verified): `accounts.players(id, display_name, discriminator,
created_at)` (`al:121-126`); `identities(provider, subject, player_id, secret_hash,
created_at)` (`al:163-170`); access `sessions(token PK, player_id, created_at, expires_at,
family_id)`, 60-minute TTL (`al:172-181`, `st:10-12`); `refresh_tokens(…, family_id,
issued_at, expires_at, used_at)`, 30-day family (`al:184-192`, `st:14-16`). Providers: `guest`,
`dev`, `epic`, `google` (`api/accounts/events/src/lib.rs:99-102`); a `dev` subject is the
email (`st:539-552`), so subject is PII. No status, kind, country, PII store or
last-seen column. Reusable reads: `stats()` (`st:972-981`), `list_players(limit)`
(`st:984-1013`, unused at HEAD), `player_by_handle` (`st:935-952`), `players_by_id`
(`st:897-920`), `identities_of` (`st:954-970`, no `created_at`). Indexes: `al:1264-1296`
(none on `players.created_at` or `refresh_tokens.issued_at`).

---

## 1. Element table: Users & players (`players.html`)

### Shell frame (each row needs an entry in `players.txt`)

| Element | Mockup line | Carrier | Status |
|---|---|---|---|
| Sign out form | `reg:143` (none) | `shell:31` | template choice. Copy `index.txt:5` |
| Theme toggle | `reg:158` (devbar) | `shell:39` | template choice. Copy `index.txt:11` |
| Design-system section | `reg:13` | none | D3. Copy `index.txt:9` |
| Lore section | `reg:10,19-24` | none | no lore module exists. Copy `index.txt:7` |
| Economy section | `reg:11,25` | wallet port | not ported yet (Step 15). Copy `index.txt:8` |
| API keys / Scheduler / Config / Audit entries | `reg:26,28-30` | their ports | not ported yet. Copy `index.txt:6` |
| Players section, entry "Users & players", `users` icon, active, `span.count` "214,903" (no tone) | `reg:9,18,140` | fixed section `players` (`cp:59-64`); `NavEntry{section:"players", order:10, counter}`; `Point::NavCounter` → `Cell{Badge, tone: None}` → `span.count` (`tmpl:370-373`) | ok. REAL `stats().0` (G6 on cost). No count while 0 players |
| Breadcrumbs "Players / Users & players" | `pc:80` | `section_crumb` + page (`rt:101-105`) | ok |
| Page wrapper `div.stack` around the panel | `pc:104-105` | `Layout::Stack` (`shell:53`) | template choice, as `mail.txt:14-15` |

### Page head

| Element | Mockup line | Carrier | Status |
|---|---|---|---|
| H1 "Users & players" | `pc:104` | `PageDecl.title` | ok |
| Sub "Staff and players share one account space; Lore access is a role on the account." | `pc:104` | `PageDecl.subtitle` | ok (text; "Lore access" names a module that does not exist, kept verbatim per R3) |
| `btn` "Export CSV" (`download`) | `pc:104` | `PageHead`, `Default`, `implemented: false` | ok, NEW (`ds:65`) |
| `btn primary` "Create account" (`plus`) | `pc:104` | `PageHead`, `Primary`, `implemented: false` | ok, NEW (`ds:65`) |

### Users table panel (DataTable, `list_columns_for: Some("player")`)

| Element | Mockup line | Carrier | Status |
|---|---|---|---|
| Panel with no head (no `title` passed) | `pc:105`, `bjs:144` | the portal always draws `.panel-head` (`tmpl:155,385`) | **contract gap G2** |
| `section.panel.players-table` class | `pc:105` | none | **G2** (template choice) |
| Banner warn "Embers column unavailable — wallet module did not answer. Other columns are live." + `btn sm` Retry | `pc:106` | none yet | **engine G1**; not ported yet (Step 15) |
| Toolbar search "Handle, name, email or user id" | `pc:98` | `DataTable.search_placeholder` (`tmpl:165`) | ok; `form`/`span` pair as `mail.txt:12-13`. Data G13 |
| Filter chips Kind / Status / Provider "any", "+ Filter" | `pc:99`, `bjs:155` | `DataTable.filters` (`tmpl:166-167`) | ok. Data G13 |
| Toolbar right: `span.muted` "Columns:" + `btn sm` `sliders` "9 of 12" | `pc:100` | none (`tmpl:167` ends at `span.grow`) | **contract gap G5** |
| Select column `th.cb`/`td.cb` | `pc:98` | `DataTable.selectable` (`tmpl:178,183`) | ok |
| Headers User, Kind, Status, Identities | `pc:73` | `Column` | ok |
| Header "Characters" `th.num > span` | `pc:74,111` | fan-out (G1) | not ported yet (Step 14) |
| Header "Lore role" `th > span` | `pc:75,111` | fan-out (G1) | no lore module exists |
| Header "Embers" `th.num > span` | `pc:76,111` | fan-out (G1) | not ported yet (Step 15) |
| Header "Last seen" `th.sorted` + `span` "↓" | `pc:77`, `bjs:157` | `Column.sortable` only; `tmpl:179` draws no sort | **contract gap G4** |
| Header "Created" | `pc:77` | `Column` | ok |
| Row menu `th.act` / `td.act > button.btn.sm.icon.ghost` | `pc:101`, `bjs:167` | `Row.entity` + any `RowAction("player")` (`rt:676-684`, `tmpl:185`) | ok |
| `tr.clickable` | `pc:84` | `Row.entity` (`rr:915`) | ok; kind `player`, not the mock's `user` (`plan:62`) |
| User cell `span.user-cell > span.avatar + span > a(handle)` [+ ` span.muted` name for staff] | `pc:86` | `CellKind::User` draws `a > span.user-cell > … > span` (`tmpl:75,82`) | **contract gap G3** (link inside, muted suffix). REAL `Name#1234` (`st:127-129`) |
| Kind `span.badge.plain` (`accent` for staff) | `pc:87`, `bjs:97` | `Badge` draws `span.badge <tone>` only (`tmpl:73`) | **G3** (plain badge). Data **G12** (constant `"player"`) |
| Status `span.badge <ok/warn/err>` | `pc:88`, `bjs:98-99` | `Badge`, tone from value (`rr:160-168` has active/suspended/banned) | ok carrier. Data **G12** |
| Identities: one `span.tag` per provider, space separated | `pc:89` | one `Ref` cell = one `span.tag` (`tmpl:74`) | **G3** (several elements). REAL (`st:984-1013`) |
| Characters `td.num` number | `pc:90` | fan-out (G1) | not ported yet (Step 14) |
| Lore role `span.badge.plain` / `span.muted` "—" | `pc:91` | fan-out (G1) | no lore module exists |
| Embers `td.num > span.muted[title="wallet module unavailable"] > svg alert` | `pc:92` | fan-out failure cell (G1) | not ported yet (Step 15) |
| Last seen `span.dim` "4 min ago" | `pc:93` | `CellKind::Time` (`tmpl:77`) | ok carrier. Data **G12** |
| Created `span.muted` "2025-03-11" | `pc:94` | none: no muted carrier | **G3**. REAL `created_at` |
| Foot "214,903 accounts · showing 1–14", pager "1 / 15,351" | `pc:102`, `bjs:163` | `PageInfo` (`tmpl:171-173`); text from `rr:925-928` ("N of M") | ok shape; **text G6** |

### Row menu (popover, not diffed)

| Element | Mockup line | Carrier | Status |
|---|---|---|---|
| accounts "Open detail" (`external`) | `reg:97` | `RowAction` + `Then::Link`: the link is static, `bind` sets only url/vals (`rr:302-315`) | **G15**: not built, hand-checked |
| "Suspend account…" (`ban`), "Delete account…" (`trash`, danger) | `reg:98-99` | `RowAction("player")`, `implemented: false` | ok, NEW (`ds:68`) |
| wallet, inventory, notifications, lore, storage groups | `reg:101-111` | their ports | not ported yet / no lore module (hand-checked) |

### Dashboard and palette

| Element | Mockup line | Carrier | Status |
|---|---|---|---|
| Tile "Active players (24 h)" value "38,412" | `reg:46` | `DashboardTile`, order 10 (`plan:1820`) | DERIV `count(DISTINCT player_id)` over `refresh_tokens.issued_at` (`ds:35`, `al:188`); no index on `issued_at` (G12) |
| Delta `span.up` "▲ 6.1%" + " vs prev. day"; spark `--chart-1` | `reg:46` | `Delta{tone: Some(Ok)}`, `Palette::Chart1` | **data: FAKE** (`ds:35`) |
| Tile link | `reg:46` | `Link::Page{accounts, players}` | ok |
| Search provider: group "Users & players", icon `user`, hit title handle, sub "`kind` · `id`", right status | `reg:38`, `reg:220` | `SearchDecl{kind:"player", label, icon:"user"}`; `SearchHit{title, subtitle, badge}` (`cp:300-306`, `tmpl:415`) | ok carrier; scope chip reads `player`, not `user` (`plan:62`). Data **G13** |

---

## 2. Element table: player detail (`player.html?u=aiko.tanaka`)

### Entity header (`section.panel`, keyed `#1`)

| Element | Mockup line | Carrier | Status |
|---|---|---|---|
| `span.avatar.lg` | `pc:168` | `shell:58`, OOB swap `tmpl:357` | ok |
| H1 display name; `span.muted` "@handle" | `pc:168` | `EntityHeader{title, subtitle}` (`cp:980-985`, `tmpl:319`) | ok. REAL `display_name`, `Name#1234` |
| Status `span.badge.ok` | `pc:168` | `EntityHeader.status` | ok. Data G12 |
| Kind `span.badge.plain.accent` | `pc:168` | none (one `status` only) | **contract gap G10** |
| Fact `svg hash` + `span.mono` id | `reg:80` | `Point::EntitySummary` → `span > {cell}` (`tmpl:360-364`) draws no icon | **G10** (fact icon). REAL |
| Fact `svg clock` + "joined 2025-03-11" | `reg:81` | as above | **G10**. REAL `created_at` |
| Facts characters / Lore / wallet-unavailable | `reg:82-84` | their ports | not ported yet (Steps 14, 15) / no lore module |
| Header actions Change Lore role, Notify | `reg:89-90` | their ports | no lore module / not ported yet (Step 20) |
| `btn danger-ghost` "Suspend" (`ban`) | `reg:91`, `pc:166` | `HeaderAction("player")`, danger, `implemented: false` | ok, NEW |
| `button.btn.icon` more | `pc:168` | `menu_url` when any `RowAction("player")` (`rt:578-582`, `shell:61`) | ok |
| Tab "Profile & identities", `on` | `reg:67`, `pc:167` | `Point::EntityTab{kind:"player", tab:"profile", …}` (`shell:62`); first tab by order (`rt:566-570`) | ok |
| Tabs Characters…Lore with counts, err dot, lock | `reg:68-75` | their ports | not ported yet (Steps 14-20) / no lore module |
| Crumbs "Players / Users & players / aiko.tanaka" | `pc:164` | last crumb is the URL id, a uuid (`rt:589`) | **G10** (text only) |

### Profile tab body

| Element | Mockup line | Carrier | Status |
|---|---|---|---|
| `div.grid.g-2` holding the four panels (three contributed, G9) | `pc:118` | the tab body is always `div.stack` (`shell:64`) | **contract gap G11** |
| Panel "Profile", tool `btn sm` "Edit" (`edit`) | `pc:119` | PropertyList + `PanelTool`, `implemented: false` | ok, NEW (`ds:81`); needs **G8** |
| User id: `span.mono` id + `span.tag` "user:id" | `pc:119`, `bjs:109` | one `Mono` cell | **G3** (two elements) |
| Handle, Display name | `pc:119` | `Text` | ok, REAL |
| Kind badge plain; Status badge | `pc:119` | as list | **G3**, **G12** |
| Country | `pc:119` | `Text` | **data: FAKE** (`ds:81`) |
| Created; Last seen | `pc:119` | `Text` | REAL; Last seen **G12** |
| Panel "Personal data", denied: "Requires `span.tag` accounts.pii.read …" | `pc:120`, `bjs:125` | none: not contributed (G9), mock-only panel | **G9** |
| Panel "Linked identities", meta count, tool `btn sm` "Link" (`plus`) | `pc:121` | DataTable, `PanelBody.meta`, `PanelTool`, `implemented: false` | ok, NEW; **G8** |
| Columns Provider (`span.tag`), Subject (`span.mono`), Linked, Last used, "" | `pc:122` | `Ref`, `Mono`, `Text`, `Text`, `Column{label:""}` | ok carrier. Subject masking **G12**; Linked needs `identities.created_at` (`al:168`, not selected by `st:954-970`); Last used **FAKE** |
| Row `btn sm ghost danger-ghost` "Unlink", no icon, disabled on the last identity | `pc:122` | none: a DataTable has no per-row buttons | **contract gap G7**; NEW |
| Panel "Active sessions", meta "2", tool `btn sm danger-ghost` "Revoke all", no icon | `pc:123` | `PanelTool`, danger, `implemented: false`; `action_button` always draws an svg (`tmpl:96`) | **G7** (icon-less); NEW (`plan:60`) |
| Columns Client, IP, Started, Expires, "" | `pc:116,124` | `Text` | Client, IP **FAKE**; Started, Expires **G12** |
| Row `btn sm ghost` "Revoke", no icon | `pc:124` | none | **G7**; NEW |
| No pager on either table | `pc:122,124` (`pager:false`) | `PageInfo{total: None, next_cursor: None}` → no foot (`rr:939`) | ok |

---

## 3. States per panel

| Panel | ok | empty | readonly | stale | error | denied / narrowed to zero |
|---|---|---|---|---|---|---|
| Users table | 14 rows, Embers failing (`pc:83-106`) | fresh fleet before demoseed: toolbar + "Nothing here yet" + foot (`tmpl:329-331`) | unreachable under G8 | `cache_ttl_s` (recommend none: a list) | portal error block (`tmpl:403`) | narrowed: "No results" state (`tmpl:330`); golden `table_empty_search` exists per the skill. The fan-out makes no call for 0 rows |
| Profile | 8 props, Edit tool | `NotFound` → empty (`pn:271-274`) for a deleted id | G8 | none | error block | — |
| Personal data | not contributed (G9) | — | — | — | — | — |
| Linked identities | rows | 0 identities: unreachable (a player is created with one, `st:289`) | G8 | none | error block | — |
| Active sessions | rows | 0 live: "Nothing here yet"; the mock has no empty design | G8 | none | error block | — |
| Header | title, badges | `NotFound` → fallback h1 (`tmpl:358`) | — | — | `dot err` (`tmpl:358`) | — |
| Tile | `reg:46` | never (`pn:216`) | — | `cache_ttl_s` (recommend 60, as mail) | error tile | — |
| Nav counter | count | `Primitive::Empty` at 0 → no count | — | — | no count | — |

---

## 4. `writes` and every ActionDecl

No accounts action is REAL in this step: every mockup action is NEW (`ds:65,68,81-84`; revoke
sessions excluded at `plan:60`). **`writes: false`** with G8 (decided). No conformance
writer probe and no `cp.invoke` CapCase then apply (`tools/conformance/src/checks.rs:239-243`),
and no `request_id` double-apply risk exists. All `min_role: Operator`, `multi: false`,
`then: Invoke` unless noted.

| id | point | label / icon | Emphasis | danger | implemented | order |
|---|---|---|---|---|---|---|
| `export-csv` | `PageHead("players")` | "Export CSV" / `download` | Default | false | false | 10 |
| `create-account` | `PageHead("players")` | "Create account" / `plus` | Primary | false | false | 20 |
| ~~`open-detail`~~ | ~~`RowAction("player")`~~ | ~~"Open detail" / `external`~~ | ~~Default~~ | ~~false~~ | not built (G15) | ~~10~~ |
| `suspend` | `RowAction("player")` | "Suspend account…" / `ban` | Default | false | false | 20 |
| `delete-account` | `RowAction("player")` | "Delete account…" / `trash` | Default | true | false | 30 |
| `suspend-header` | `HeaderAction("player")` | "Suspend" / `ban` | Default | true | false | 30 (lore 10, notifications 20, `reg:89-91`) |
| `edit-profile` | `PanelTool("profile")` | "Edit" / `edit` | Default | false | false | 10 |
| `link-identity` | `PanelTool("identities")` | "Link" / `plus` | Default | false | false | 10 |
| `unlink-identity` | `ActionPoint::TableRow("identities")` (G7) | "Unlink" / none | Ghost | true | false | 10 |
| `revoke-all` | `PanelTool("sessions")` | "Revoke all" / none | Default | true | false | 10 |
| `revoke-session` | `ActionPoint::TableRow("sessions")` (G7) | "Revoke" / none | Ghost | false | false | 10 |

The mock's Suspend and Delete confirms (`pc:13-25`) are never opened: the actions render disabled.

---

## 5. Contributions and `order`

| Contribution | Point | order | Content |
|---|---|---|---|
| NavEntry `players` | section `players` | 10 | "Users & players", `users`, page `players`, counter `nav.players` |
| `nav.players` | `NavCounter` | 0 | `Cell{Badge, thousands(players), tone: None}`; `Empty` at 0 |
| `tile.active` | `DashboardTile` | 10 | §1 dashboard rows |
| `players` | `Page("players")` | 10 | the table; `PageDecl{id:"players", head_actions:[export-csv, create-account], layout: Stack}`, title "" (G2) |
| EntityKindDecl | — | — | `{kind:"player", label:"Player", icon:"user"}` |
| `header` | `EntityHeader("player")` | 0 | G10 |
| `fact.id`, `fact.joined` | `EntitySummary("player")` | 10, 20 | G10 |
| `profile`, ~~`pii`~~, `identities`, `sessions` | `EntityTab{player, "profile", "Profile & identities"}` | 10, ~~20~~, 30, 40 | §2 |
| search | `SearchDecl{kind:"player"}` | — | G13 |

---

## 6. The `EntityListColumn` engine (owner, 2026-11-10; `plan:1893-1905`)

At HEAD the point is wired as a standalone slot: `mode_of` maps it to `inline`
(`rr:594`) and `ColumnCells` renders as a PropertyList (`rr:780-784`). Part `a` replaces this
with the fan-out:

- `PanelQuery.refs: Vec<String>` (`cp:266-283`), appended to `cache_key` (`pn:173-197`), with
  an Opaque `cp.panel` `req.refs` row beside `tools/conformance/src/policy.rs:182-192`
  (`CONTROL_PLANE_READ_RATIONALE`, `:80`). The key spelling for a `Vec` field is the walker's;
  I did not verify it.
- In `rt::panel` (`rt:610-719`), after an `ok`/`readonly` DataTable with `list_columns_for:
  Some(kind)`: every `EntityListColumn{kind, ..}` decl across modules, by `(order, module,
  id)` (`rt:261-273`), is called concurrently, each under its own `deadline` (`pn:199-202`),
  with `entity: None` and `refs` = the page rows' `Row.entity.id` of that kind, in row order.
  A decl below the operator's `min_role` makes no call and draws no column. Zero rows → no
  calls.
- Rendering, from the mock:
  - header `th[.num] > span(label)` (`pc:111`); label from the point, alignment per **G1**;
  - an ok column: `ColumnCells[id]` drawn by the `cell` macro in `td[.num]`; a missing id draws
    an empty `td`;
  - a failed, timed-out or non-`ColumnCells` column: every cell is
    `span.muted[title="<module> module unavailable"] > svg alert` (`pc:92`, `catalog.js:15`);
  - one `div.banner.warn` per failed column: alert icon, "<label> column unavailable —
    <module> module did not answer. Other columns are live." and a `btn sm` Retry with
    `data-panel-retry` and a bare text label, as `pc:106` (the portal's other Retry buttons
    wrap it in `span`, `tmpl:127`). Placed after the module banner (`tmpl:396`), in `ok` and
    `readonly` only.
- Exercising it with no contributor: admindiff cannot (no column exists in any fleet). The
  `plan:2163-2164` engine test sits in Step 25. **Decided (owner, 2026-11-17):** a dedicated
  `[opus]` test-author step right after part `a` writes that gate test with a fake provider
  (one ok, one failing, one denied column; one call per page), plus golden cases for the
  `th > span`, the failure cell and the banner.

---

## 7. Gaps and decisions for part `a`

**G1. Column position, alignment and failure chrome.**
- Problem: the mock puts contributed columns between Identities and Last seen
  (`pc:72-78`); the plan says only "merged as extra columns" (`plan:1903`). Characters and
  Embers are `th.num` even when failing (`pc:74,76`), but `Point::EntityListColumn` carries
  only `{kind, label}` (`cp:435`) and `ColumnCells` is `BTreeMap<String, Cell>`
  (`cp:1025`). The warn banner with Retry (`pc:106`) is not in the plan text.
- Proposal: `DataTable.list_columns_at: usize` (insert index into `columns`) and
  `Point::EntityListColumn { kind, label, align: Align }`; the §6 rendering. ~~Alternative:
  append at the end and right-align when every cell is `Number` (a failing column then loses
  `num`; Last seen and Created move, which the diff reports).~~
- **Decided (owner, 2026-11-17): the proposal.** Insert index `list_columns_at`, `align` on
  the point, header `th > span`; a failing column draws a muted alert icon with a title in
  every cell, plus one warn banner with Retry (§6).

**G2. A panel with no head.**
- Problem: `panel()` draws `.panel-head` only when a title is passed (`bjs:144`); the
  portal always draws it (`tmpl:155,385`). The section also carries `players-table`
  (`pc:105`), a hook for the mock's own `after()`.
- Proposal: `ContributionDecl.title == ""` draws no head in the slot and the fragment;
  `players-table` is a template-choice accept entry. ~~Alternative: title "Users", accepted
  as a portal-only head.~~
- **Decided (owner, 2026-11-17): the proposal.**

**G3. Cell vocabulary.**
- Problem: no carrier for (i) `span.muted` text (Created, the lore "—", the staff name);
  (ii) `span.badge.plain` (+ `accent`); (iii) several elements in one cell (Identities tags;
  User id `mono` + `tag`); (iv) a user cell whose link is inside it (`pc:86` against
  `tmpl:75,82`).
- Proposal: `CellKind::Muted` and `CellKind::Label` (plain badge, tone as `Badge`);
  `Cell.more: Vec<Cell>` drawn after the cell, space-separated; `CellKind::User` with a link
  draws `span.user-cell > avatar + span > a` when `Row.entity` is set (list form), keeping the
  outer link elsewhere. Each new variant needs a golden case (drift guard). ~~Alternative:
  contract-gap accept entries for each.~~
- **Decided (owner, 2026-11-17): the proposal.** `CellKind::Muted`, `CellKind::Label`,
  `Cell.more`; a User cell in a list draws its link inside.

**G4. Sorted header.** `Column` has `sortable` only (`cp:739-745`). Proposal:
`Column.sorted: Option<SortDir>` drawing `th.sorted` + `span` arrow, and mark **Created**
descending, the real order (`st:993`); the arrow on Created instead of Last seen is a declared
deviation. ~~Alternative: a contract-gap entry.~~
**Decided (owner, 2026-11-17): the proposal.** The arrow on Created, not the mockup's Last
seen, is an accept entry citing declared deviation D9 (`plan`, Declared deviations).

**G5. Toolbar right ("Columns:" + "9 of 12").** No carrier and no column chooser exists.
Proposal: contract-gap accept entries. ~~Alternative: `DataTable.toolbar_note` plus a disabled
table-tool action.~~ **Decided (owner, 2026-11-17): the proposal**, plus an item under
**Accepted contract gaps** in `docs/todo/admin-second-pass.md`, written by part `a`.

**G6. Foot text, totals and paging.**
- Problem: the foot reads "N of M" or "N rows" (`rr:925-928`); the mock reads "214,903
  accounts · showing 1–14". There is no `created_at` index, and R1 forbids one
  (`plan:48-49`), so a keyset page on `created_at` sorts the whole table each page.
  `count(*)` runs on every page load through the nav counter.
- Proposal: `DataTable.item_noun` (as `WorkQueue.item_noun`, `cp:882-883`): "M accounts ·
  showing 1–N" on the first page, "M accounts" after; `(created_at, id)` keyset cursor,
  `total` from `stats().0`, `None` while searching (as `cp:886`); a comment naming both
  costs. ~~Alternative: `pg_class.reltuples` estimate for the nav and the total.~~
- **Decided (owner, 2026-11-17): the proposal.** Exact `count(*)` for the total, no new
  index (R1); `DataTable.item_noun`; `(created_at, id)` keyset cursor, with a code comment
  naming the missing-index cost. Part `a` adds a `docs/todo/admin-second-pass.md` item for a
  `reltuples` estimate at scale.

**G7. Row buttons in a DataTable, and icon-less buttons.** Unlink and Revoke are labelled
per-row buttons (`pc:122,124`); "Revoke all" has no icon (`pc:123`); `action_button` always
emits an svg (`tmpl:96`). Proposal: `ActionPoint::TableRow(contribution)` + `Row.item` +
`Row.actions`, drawn in a trailing `td` with an empty `th` (mail G1 shape); omit the svg when
`icon` is empty. ~~Alternative: contract-gap entries.~~ **Decided (owner, 2026-11-17): the
proposal.** New `ActionPoint::TableRow`; an action with no icon draws no svg.

**G8. A non-writer with only NEW actions gets read-only chrome.** `has_write_affordance`
counts any non-Link `PanelTool` (`pn:84-91`), so `writes: false` puts the ro-chip and info
banner on Profile, Linked identities and Active sessions; the mock has none. Proposal: an
`implemented: false` action is not a write affordance (`admit` already refuses it first,
`engine/actions.rs:80-91`). ~~Alternative: `writes: true` plus a probe whose `invoke` refuses
everything, which declares a writer that writes nothing.~~ **Decided (owner, 2026-11-17): the
proposal.** `has_write_affordance` ignores an `implemented: false` action; accounts keeps
`writes: false`.

**G9. "Personal data" denied.** Denial is by role only (`pn:240-243`) and the crawl operator is
admin, so the designed state is unreachable; the detail names a role, not a permission
(`tmpl:134-136`). No PII store exists (FAKE, `ds:82`). ~~Proposal: a provider's `Forbidden` panel
answer maps to `denied` (as `NotFound` maps to `empty`, `pn:271-274`), with the message as the
tag text; accounts answers `Forbidden("accounts.pii.read")` (`// FAKE:`). Alternative: a data
difference entry for good.~~ **Decided (owner, 2026-11-17): no `Forbidden` → denied mapping.**
Part `a` contributes no Personal data panel. The mock's panel is mock-only, with accept kind
`contract gap`: the owner declined the contract change, and no PII store exists, so it is not a
data difference. Part `a` adds its item under **Accepted contract gaps** in
`docs/todo/admin-second-pass.md`.

**G10. Entity header.** One `status` cell (`cp:980-985`); facts draw no icon (`tmpl:360-364`);
the crumb is the uuid (`rt:589`). Proposal: `EntityHeader.badges: Vec<Cell>` after `status`;
the fact branch draws `ContributionDecl.icon` before the cell (`PanelRender.icon` exists,
`rr:644`); the header fragment OOB-swaps the last crumb with `title`, as it does the avatar
(`tmpl:357`). ~~Alternative: accept the three.~~ **Decided (owner, 2026-11-17): the
proposal.** `EntityHeader.badges`, the decl icon on facts, the crumb shows the handle (the
header `title`) instead of the uuid. **Decided (owner, 2026-11-17, Step 13 part `a` review):**
the title is the display name, so the handle needs its own contract field:
`EntityHeader.crumb: Option<String>` (the `title` when `None`); accounts sets it to the handle.

**G11. Tab layout.** Proposal: `Point::EntityTab` gains `columns: u8` (2 → `div.grid.g-2`),
read from the tab's first-ordered contribution. ~~Alternative: a template-choice entry.~~
**Decided (owner, 2026-11-17): the proposal.** The grid holds three panels (Profile, Linked
identities, Active sessions); the mock's fourth, Personal data, is not contributed (G9).

**G12. Data decisions.**
- Kind: ~~FAKE~~ constant `"player"` (operators are `admin.users`, `ds:16-18`) ~~vs an R1 hash
  that invents staff~~. Recommend the constant; staff row shapes become data differences.
- Status: `"active"` for every account, true while no suspension exists, ~~vs an R1 hash~~.
  Recommend `"active"`; suspended/banned rows are entries for good.
- Last seen: `ds:59` says FAKE, but `max(refresh_tokens.issued_at)` per player is DERIV
  (`al:188`, `refresh_player_idx`), relative in SQL (mail G7), "—" past the 30-day prune.
  Recommend DERIV ("a real field always wins", `ds:5-6`).
- Active sessions: access rows live 60 minutes and rotate (`st:10-12`), so one device shows up
  to two rows and "Expires" is under an hour. Recommend one row per live refresh family:
  Started `min(issued_at)`, Expires the family's `expires_at`, count = families (DERIV).
  Client/IP FAKE as a hash of `family_id`; never display or hash the bearer `token`.
- Subject: mask (`dev` subjects are emails): first 4 + "…" + last 4. Country, Last used:
  FAKE hash of the id.
- Tile: the 24 h count has no `issued_at` index; add `expires_at > now() - 24 h` so
  `refresh_expires_idx` bounds it (a successor copies the family expiry, `st:760-779`).
- **Decided (owner, 2026-11-17): every recommendation above.** Kind `"player"` and status
  `"active"` are truthful constants, not FAKE; Last seen is DERIV from
  `refresh_tokens.issued_at`; Active sessions are one row per refresh family, Client/IP a FAKE
  hash of `family_id` (never the token); Subject masked; Country and Last used FAKE; the tile
  bounded by `expires_at`. Mockup differences (staff, suspended/banned rows) are
  data-difference entries. Every `// FAKE:` marker gets a `docs/todo/admin-second-pass.md`
  item.

**G13. Search and filters.** Exact handle (`st:935-952`), uuid (`st:897-920`) and email (PK
`(provider, subject)`, `st:539-552`) are REAL. Substring on `display_name` is a seq scan:
DERIV by R1's letter, NEW by `ds:95`. Recommend DERIV with `LIMIT` under the 800 ms deadline
(mail G2(i) precedent), for the palette and the table. Filters: Provider DERIV; Kind and
Status display "any" and are ignored (no picker sets them, `tmpl:166`). **Decided (owner,
2026-11-17): as recommended.**

**G14. admindiff route for `player`.** `PAGE_MAP` routes are `&'static str` (`pages.rs:13`) and
a fleet's player ids are random. Proposal: `Route::FirstRow("players")`: the crawl follows the
first `tr.clickable` `data-href` of the mapped list. ~~Alternative: keep `player` Pending this
step and hand-review the detail page.~~ **Decided (owner, 2026-11-17): the proposal, built in
part `a`.**

**G15. Row menu "Open detail".** A `Then::Link` is static. Proposal: leave it out (the row
click opens the detail); hand-checked, popover not diffed. ~~Alternative: a link that binds to
the menu's entity.~~ **Decided (owner, 2026-11-17): the proposal.** No "Open detail" entry.

---

## 8. Accept-file plan

**Resolved by this port, deleted when STALE:** `index.txt:10` (Players section), `:14` and
`:19` (tile), `events.txt:10`, `mail.txt:10`.

**New `players.txt` and `player.txt`:** copy `index.txt:5, 6, 7, 8, 9, 11`; not `:10`.

**Expected page-specific entries** (exact text from the first diff):
- both: the `div.stack` page wrapper and the `mock-only [panel …]` frame line, template choice
  (as `mail.txt:14-15`); the search `span`/`form.input-icon` pair (players), template choice.
- players: three mock-only contributed `th` (Characters Step 14; Lore role no lore module;
  Embers Step 15); every mock `tr` shape mock-only and the portal `tr` portal-only, not
  ported yet (the contributed `td`s), plus data differences for suspended/banned/staff and
  multi-provider rows (G12 constants); the Embers banner, not ported yet (Step 15), and a
  data difference once wallet answers; G2's `players-table` class, template choice; G4 the
  sort arrow on Created instead of Last seen, declared deviation D9; G5 the "Columns:" +
  "9 of 12" toolbar control, contract gap.
- player: mock-only tabs, facts and header actions of later ports or no lore module; the
  mock-only Personal data panel (G9), kind `contract gap` (Forbidden→denied declined, no PII
  store); data differences from the G12
  constants and FAKE values. The kind badge, grid layout and row buttons are built
  (G10, G11, G7), so they need no entry.

---

## 9. Predicted `docs/todo/admin-second-pass.md` items (part `a` writes them)

FAKE: country, last used, session client/IP, tile delta and spark. ~~kind; the Personal data
denial (G9); status if G12 picks a hash.~~ Kind and status are truthful constants (G12).
Scale: a `reltuples` estimate for the
players total (G6). Disabled: export-csv, create-account, suspend, suspend-header,
delete-account, edit-profile, link-identity, unlink-identity, revoke-all, revoke-session.
Under **Accepted contract gaps**: the "Columns: 9 of 12" control (G5) and the uncontributed
Personal data panel (G9).

## 10. `pages.rs` change (part `a`)

`tools/admindiff/src/pages.rs:29`: `players` → `Route::Mapped("/admin/p/accounts/players")`;
`:30` `player` gets the G14 route that follows the list's first row.

## Not verified

- No `cargo`, `admindiff mock`, crawl or diff was run; every accept line is a prediction.
- Which providers demoseed's players carry; whether `dev` subjects there are emails.
- The conformance key spelling for `refs`; the split path of the fan-out (admin-svc dials
  `accounts`, `cmd/admin-svc/src/main.rs:34`).
