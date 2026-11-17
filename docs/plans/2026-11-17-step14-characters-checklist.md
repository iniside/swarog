# Step 14 (characters): checklist

Read-only extraction per `.agents/skills/mockup-implementation/SKILL.md` §1 (as of 8a2507b0, which rewrote the hand-check paragraph and the `meta.after` note, `SKILL.md:90-102`). Mockup pages: `UILayout/redesign/players.html` (`PAGES.boot("players")`) and `player.html` (`PAGES.boot("player")`). Builder lines that characters owns:
- players: `P.players` list column `pc:74` (header) and `pc:90` (cell). The `th > span` wrap is in `P.players.meta.after`, `pc:109-112`.
- player: `P.player` facts `pc:165`, tab bar `pc:167`, `tabRender.characters` `pc:126-127`.
- Registry: tab `reg:68` (with `count`), fact `reg:82` (`swords`).
- Rows: `data.js:4-17` (`D.users[].characters`; aiko.tanaka has 3, `data.js:4`; priya.n and elena.p have 0, `data.js:8,10`).

No characters element exists on any other page. Grep over `UILayout/redesign` for `charact|swords` is a lower bound: hits are only `pc:74,90,126-127,131-132` (the last two are inventory's Owner column, Step 16), `reg:68,82`, `data.js` rows, and `data.js:95` (the `characters-svc` service-health row, which the portal owns as a FAKE). There is no characters search provider (`reg:37-42`), header action (`reg:88-92`), row-menu group (`reg:95-112`), dashboard tile (`reg:45-54`), attention item (`reg:57-63`) or nav entry (`reg:16-34`).

Abbreviations: `pc` = `UILayout/redesign/assets/pages-core.js`, `reg` = `UILayout/redesign/assets/registry.js`, `bjs` = `UILayout/design-system/components/bundle.js`, `cp` = `core/controlplane/src/lib.rs`, `tmpl` = `core/admin/src/templates/primitives.html.tmpl`, `shell` = `core/admin/src/templates/shell.html.tmpl`, `rr` = `core/admin/src/render.rs`, `rt` = `core/admin/src/routes.rs`, `pn` = `core/admin/src/engine/panels.rs`, `col` = `core/admin/src/engine/columns.rs`, `gt` = `core/admin/src/golden_tests.rs`, `sk` = `tools/admindiff/src/skeleton.rs`, `mock` = `tools/admindiff/src/mock.rs`, `crawl` = `tools/admindiff/src/crawl.rs`, `pages` = `tools/admindiff/src/pages.rs`, `cl` = `modules/characters/src/lib.rs`, `cs` = `modules/characters/src/store.rs`, `csv` = `modules/characters/src/service.rs`, `aa` = `modules/accounts/src/admin.rs`, `ds` = `docs/research/2026-10-22-1830-admin-redesign-data-sources.md`, `plan` = `docs/plans/2026-10-22-1759-admin-control-plane-redesign-plan.md` (line numbers after 8a2507b0).

Method: rust-analyzer was not used. Every claim comes from a targeted read with line numbers, unless it is tagged **[grep]**, which is a ripgrep sweep and a lower bound. No research subagent was used.

Comparison scope (as in the Step 13 checklist):
- Tags, classes and nesting only. The §7 `diff` runs without `--text`, and attributes are never compared (`sk:1-8`, `SKILL.md:90-96`).
- `.frag` is unwrapped and `.frag-label` removed on the mock side (`sk:25-30`); `.slot` is unwrapped on the portal side (`sk:34`).
- Only `tbody>tr`, `ul>li`, `.svc`, `.pop-row` and `.uptime>i` collapse (`sk:126-137`). Everything else compares by position: an LCS of canons, then kin pairing inside each gap (`sk:451-559`).
- The mock harness never runs `meta.after` (`mock:22-23`). The players mock's contributed `th` is therefore a bare `th.num`, while the browser mockup draws `th.num > span` (`pc:109-112`).
- The crawl follows the first players row to the entity page's default tab and fetches its slots and row menus (`crawl:55-98,153-178`). It never opens `?tab=`.

What characters holds (verified):
- `characters.characters(id uuid PK, player_id uuid, name text, class text DEFAULT 'novice', created_at timestamptz)` (`cl:46-54`), indexed on `player_id` (`cl:146-148`).
- `purged_players(player_id PK, handle, created_at, swept_at)` with a pending partial index (`cl:55-60,150-154`).
- No level, region, last-played or activity column.
- `player_id` is a plain ref to `accounts.players.id` with no FK (`cl:43-45`).
- The player entity id is `accounts.players.id::text` (`aa:232-237`, accounts `store.rs:288`). The two ids are the same uuid, so a ref binds as `$n::uuid` directly.

Readers at HEAD:
- `list_by_player` (`cs:63-72`, `ORDER BY created_at, id`, `LIMIT 1000` = `LIST_HARD_LIMIT` `cs:12`). The per-player cap is clamped to that limit (`csv:150-159`), so a player's list never truncates.
- `count_owned_tx` (tx only, `cs:130-141`) and `get` (`cs:76-88`).
- `count` (`cs:119-124`), `list_all` (`cs:190-198`) and `pending_purges` (`cs:163-171`) have no caller **[grep]**. They are left over from the Step 1 raze, b8435c9a; the old `admin.rs` used them.
- No `controlplane` dependency in `modules/characters/Cargo.toml` **[grep]**.
- admin-svc already dials `characters` (`cmd/admin-svc/src/main.rs:31`).
- No module contributes `Point::EntityListColumn` or `Point::EntityTabCount` at HEAD **[grep over modules/]**. Characters is the first live provider of both.

---

## 1. Element table: Users & players (`players.html`)

Only the elements characters owns. Every other row is unchanged from the Step 13 checklist §1.

| Element | Mockup line | Carrier | Status |
|---|---|---|---|
| Header "Characters" `th.num`, between Identities and Lore role | `pc:74` (`num: true`) | `Point::EntityListColumn{kind:"player", label:"Characters", align: Right}` (`cp:440-442`), inserted at accounts' `list_columns_at: 4` (`aa:549-551`); drawn `th.num > span` (`tmpl:203`, `rr:870-871`) | ok. Harness side: bare `th.num` (`mock:22-23`), portal `th.num > span` → a new diff line (§8, **G6**); header shape in hand-check (§6) |
| Cell `td.num` with plain number (`u.characters`, 0 included) | `pc:90`, `bjs:161` | `ColumnCells{<ref>: Cell{Number, "<n>"}}` (`cp:1063`) → `td.num` + plain value (`tmpl:209`, `tmpl:82`) | ok. **DERIV** batch count per `player_id` (`ds:57`), served by `characters_player_idx` (`cl:146-148`). Answer every ref, `"0"` for none (the mock draws 0, `data.js:8`) |
| Failure cell (characters down) `td.num > span.muted[title] > svg alert` | as `pc:92` | Step 13 G1 engine (`col:70-80`, `tmpl:209`) | ok, already built; not crawled while characters is up |
| Failure banner "Characters column unavailable — characters module did not answer. Other columns are live." + Retry | as `pc:106` | `rr:778`, `tmpl:423` | ok, already built (golden `table_list_columns`, `gt:431-437,887-941`); not crawled |

Shell frame: unchanged. The `players.txt:5-12` copies stay. Characters adds no nav entry (`reg:16-34`).

## 2. Element table: player detail (`player.html?u=aiko.tanaka`)

### Entity header (`section.panel`, keyed `#1`)

| Element | Mockup line | Carrier | Status |
|---|---|---|---|
| Fact `span > svg(swords) + "3 characters"`, third, after id and joined | `reg:82`, `pc:165` | `Point::EntitySummary("player")` with `ContributionDecl.icon = "swords"` → `span > {icon} {cell}` (`tmpl:387-391`); `swords` exists in the icon set (`tmpl:62`) | ok. **REAL** count (`ds:77`). Wording at 1: **G7** |
| Fact error `span[style=color:err] > svg alert + "characters unavailable"` | as `reg:84` (wallet's) | `tmpl:390` | ok, built (golden `fact_error`, `gt:1757`) |
| Tab "Characters" + ` span.count` "3", second after Profile | `reg:68`, `pc:167` | `Point::EntityTab{kind:"player", tab:"characters", tab_label:"Characters", columns: 1}`; position = first contribution by `(order, module, id)` (`rt:542-551`) | ok |
| Tab count `span.count` | `reg:68` (`count` is a function, so it always draws, even "0"), `pc:167` | `Point::EntityTabCount{kind:"player", tab:"characters"}` (`cp:438`) → count slot inside the tab button (`rt:552-557`, `shell:62`) → `span.slot > span.count` (`tmpl:392-396`); the slot is unwrapped (`sk:34`) | ok, **no contract gap**. Answer `Cell{Number}` always, including "0" (an `Empty` would draw no count). DERIV (`ds:80`). Golden `count_cell`/`count_error` exist (`gt:1758-1759`); first live use |
| Tab count error `span.dot.err` | as wallet `pc:167` | `tmpl:395` | ok, built |

### Characters tab body (`?tab=characters`, `tabRender.characters`, `pc:126-127`)

| Element | Mockup line | Carrier | Status |
|---|---|---|---|
| Tab body: the panel sits directly in `.page`, no wrapper | `pc:169` | `columns: 1` → `div.stack` (`shell:64`) | template choice (only visible if **G5** is adopted) |
| Panel title "Characters", no icon | `pc:126` | `ContributionDecl.title`, `icon: ""` | ok |
| Meta "3" | `pc:126` | `PanelBody.meta` = row count | ok, REAL (`rows.len()` is exact: cap clamp `csv:150-159`, `cs:4-12`) |
| `flush` body, no toolbar, no pager | `pc:126-127` (`pager:false`), `bjs:152-163` | DataTable with no `search_placeholder`/filters, `PageInfo{next_cursor: None, total: None}` (as `aa:283-299` `detail_table`); flush for a table (`rr:701-710` per the Step 13 checklist) | ok |
| Headers Name, Class, Level (`th.num`), Region, Last played, Id | `pc:127` | `Column`; Level `align: Right` | ok |
| Rows plain `tr` (not clickable, no row menu, no select) | `pc:127` (array rows, no `href`), `bjs:161` | `Row{entity: None}` | ok |
| Name: plain text | `pc:127` | `CellKind::Text` | REAL `name` |
| Class: plain text | `pc:127` | `CellKind::Text` | REAL `class` (D4 vocabulary: seeded classes, `tools/demoseed/src/tests.rs:92`) |
| Level `td.num` | `pc:127` | `CellKind::Number`, `Align::Right` | **data: FAKE** (`ds:85`), **G3** |
| Region `span.tag` | `pc:127` | `CellKind::Ref` | **data: FAKE** (`ds:85`), **G3** |
| Last played: plain text ("4 min ago", not `span.dim`) | `pc:127` | `CellKind::Text` | **data: FAKE** (`ds:85`), **G3** |
| Id `span.mono.muted` "c_91a2" | `pc:127` | `Mono` draws `span.mono` (+ an inline colour for a tone), `Muted` draws `span.muted` (`tmpl:76,79`); nothing draws both classes | **contract gap G1**; value **G2** |
| Row count: at least 1 even when `u.characters` is 0 (`slice(0, Math.max(1, …))`) | `pc:127` | — | mock quirk; no designed empty state (**G8**) |

---

## 3. States per panel, and URLs

| Panel | ok | empty | readonly | stale | error | denied / narrowed to zero |
|---|---|---|---|---|---|---|
| Characters list column | count per row | n/a: every ref answered, "0" for none | n/a (column) | none: the fan-out calls the face directly, uncached (`col:70`); a stale players table shows no list columns (`plan:2463`), and players has `cache_ttl_s: None` (`aa:396`) | failure cell + one warn banner (`col:70-80`, `rr:778`, `tmpl:209,423`) | `min_role: Viewer` → never for the crawl's admin. Narrowed to zero: no refs, no call, no column (`col:42-44`); the table draws its "No results" state (`tmpl:357`), golden `table_empty_search` per the skill |
| Fact | "N characters" | never (always a `Cell`) | — | none | `tmpl:390` | denied: golden `fact_denied` (`gt:1794`), unreachable at Viewer |
| Tab count | `span.count` | never (always a `Cell`, "0" included) | — | none | `span.dot.err` (`tmpl:395`) | — |
| Characters tab panel | rows (demoseed seeds 1–3 per player, `tools/demoseed/src/tests.rs:89`) | 0 characters → "Nothing here yet" (`pn:260-261`, `tmpl:359`); the mock has no empty design (**G8**) | unreachable: `writes: false` and no tools | none | portal error block + Retry | — |
| Purges pending (G9) | per G9 | at 0 pending: none drawn (an empty `AttentionItems` is `Empty`, `pn:218`) | — | `cache_ttl_s: Some(60)` if a tile | per point | — |

Tombstoned player (deleted, not yet swept): accounts answers `NotFound` for the header (fallback h1), but characters still lists the rows until the sweep (`cl:212-253`). This is truthful: the rows exist until then. I recommend no special case.

URLs:
- The Characters tab panel builds none: no toolbar, no pager.
- The tab button URL is `entity_url?tab=characters` (`rt:547`). It keeps no other key, and none is needed.
- A list column has no fragment URL of its own; its URL returns a 404 toast (`plan:2464`).
- The players search and pager fragments go through `rt::panel`, which re-runs the fan-out per page (`plan:2463`, `col:36-85`). No URL is built from raw `params` by this port.

## 4. `writes` and every ActionDecl

The mockup shows no characters action anywhere: none in the `tabRender.characters` tools (`pc:126`), header actions (`reg:88-92`), row menu (`reg:95-112`) or list column (`pc:74`). Characters therefore declares **no `ActionDecl`** and **`writes: false`**. That means no conformance writer probe, no `cp.invoke` CapCase, no `request_id` double-apply risk, and no `implemented: false` todo items. The old "View Characters" row-menu extension and character modal were razed in b8435c9a. The mockup has neither, so neither is rebuilt (`ds:85`; `CHARACTER_MODAL_ACTIONS` is gone from `api/characters/api/src/lib.rs` **[grep]**).

## 5. Contributions and `order`

All have `min_role: Viewer` and `deadline_ms: DEFAULT_PANEL_DEADLINE_MS` (2000 ms, `cp:119`), as the accounts `contribution()` helper sets them (`aa:301-319`), and `cache_ttl_s: None`. The manifest has no sections, entries, pages, `entity_kinds` (accounts declares `player`, `aa:387-391`) or search.

| Contribution | Point | order | Content |
|---|---|---|---|
| `column.count` | `EntityListColumn{kind:"player", label:"Characters", align: Right}` | 10 (Lore role 20, Embers 30 reserved, per `pc:74-76`) | `ColumnCells` keyed by the **input** ref string |
| `fact.count` | `EntitySummary("player")`, icon `swords` | 30 (accounts 10, 20 `aa:398-399`; Lore 40, wallet 50 reserved, `reg:80-84`) | `Cell{Text, "N characters"}` (G7) |
| `characters` | `EntityTab{player, "characters", "Characters", columns: 1}`, title "Characters" | 20 (accounts' Profile tab is 10/30/40 `aa:400-402`; it comes first because 10 < 20) | the §2 DataTable, `meta` = row count |
| `tab.count` | `EntityTabCount{kind:"player", tab:"characters"}` | 0 (counts attach by tab id, `rt:552-557`) | `Cell{Number, n}` |
| purges (G9) | per G9 | per G9 | `pending_purges()` (`cs:163-171`) |

Store (part `a`):
- One new pooled reader, `counts_by_player(&[String]) -> BTreeMap<String, i64>`, serves the column, the fact and the tab count. Suggested SQL: `unnest($1::text[]) LEFT JOIN … ON player_id = r::uuid GROUP BY r`, which answers 0 for every absent ref and keys by the exact input.
- The panel reuses `list_by_player` (`cs:63-72`).
- Provider: `modules/characters/src/admin.rs`, contributed to `controlplane::PROVIDER_SLOT` in `init`, as accounts does (accounts `lib.rs:1403-1404`), plus `controlplane = { workspace = true }`.

## 6. Hand-check section (what the §7 `diff` cannot see)

| # | Element | Why diff can't see it | Check |
|---|---|---|---|
| H1 | Characters header shape `th.num > span` and its text "Characters" | harness never runs `meta.after` (`mock:22-23`, `SKILL.md:98-102`); text not compared | golden `table_list_columns` (`gt:431-437,887-941`) pins `th > span`; live look for label and right alignment |
| H2 | Characters cell values = real counts; "0" at zero | text | live look: one row against `GET /characters` for that player |
| H3 | Failure cell title and banner text (`rr:778`) | attribute/text; not crawled while characters is up | golden `table_list_columns` (one failed column) |
| H4 | Fact icon is `swords`; text "N characters" | svg drawing and text not compared (`sk:3-4`) | live look (§7) |
| H5 | Tab label "Characters", count value, "0" drawn at zero | text | live look |
| H6 | Tab order: Characters directly after Profile | positional compare of identical shapes may hide a swap | live look |
| H7 | **The whole Characters tab panel** (title, meta, flush, no pager, six headers, Level right-aligned, Region tag, Id mono muted, row order) | the crawl never opens `?tab=` (`crawl:83-98`); the harness draws the default tab (`pc:163`) | **G5**; otherwise a live look at `?tab=characters` in §7, monolith and split |
| H8 | Id width: full uuid vs the mock's "c_91a2" (G2) | text | declared here (deviation), cited by the step report |
| H9 | Fact wording at 1 (G7) | text | declared here if G7 departs from the mockup |
| H10 | 0-character empty state (G8) | no mock design; demoseed never yields it | golden for an empty `DataTable` (exists per §3); declared here |
| H11 | Purges pending element (G9) | appears only with a sweep backlog; a demoseeded fleet has none | live look after deleting one player, or a golden case |

## 7. Gaps and decisions for part `a`

**G1. `span.mono.muted` Id cell (contract gap).**
- Problem: `Mono` draws `span.mono` and puts a tone in an inline style (`tmpl:79`); nothing draws both `mono` and `muted`.
- The same cell recurs in inventory's Source column (`pc:132`, Step 16) and storage's Checksum column (`pc:141`, Step 19).
- Proposal: `CellKind::MonoMuted` drawing `span.mono.muted`, with a golden case (drift guard).
- Alternative: draw `Mono` and accept a `contract gap` line (only visible under G5), plus an Accepted contract gaps item.

**G2. Id value.**
- The mock shows a short `c_91a2`; the real id is a uuid.
- Proposal: the full uuid. This follows mail G7 ("shown in full", `plan:1892`) and the accounts profile id (`aa:573-576`).
- Width and text are a hand-check deviation (H8).
- Alternative: the first 8 hex digits, which is not copyable as an id.

**G3. FAKE Level, Region, Last played (R1, `plan:45-49`; `ds:85`).**
- Each is a pure function of the character id, using an explicit-width FNV-1a 64 hash with no clock and no rand, marked `// FAKE:`:
  - Level: 1–60.
  - Region: a pick from `eu-west, eu-central, us-east, us-west, ap-ne, sa-east` (the mock's shapes).
  - Last played: a pick from relative strings ("4 min ago", "2 h ago", "6 d ago", "41 d ago").
- Proposal: a local `fake_hash`/`fake_pick` copied from `aa:174-186`. The fortress rule means no import from accounts.
- Alternative: one shared `controlplane` fake helper, which would also move accounts (a refactor beyond this step).
- Each marker gets a todo item (§9).

**G4. `PanelQuery.refs` bound (the proof-auditor remark).**
- Problem: `refs` is `Opaque` on the wire (`tools/conformance/src/policy.rs:192`) and unbounded. The portal sends one page's ids (accounts `PAGE_SIZE` 25, `aa:59`), but a provider must not trust that.
- Proposal: a contract const `controlplane::MAX_PANEL_REFS`, for example 100. This follows the `MAX_INPUT_VALUE_BYTES` precedent (`plan:2452`).
  - `col::fan_out` never sends more: over the cap, every column fails with a reason.
  - characters refuses `refs.len() > MAX_PANEL_REFS` with `Error::invalid` before any SQL.
  - Ids bind as one `text[]` parameter, never interpolated.
  - A malformed id hits Postgres 22P02, which maps to `Invalid`, and the column draws its failure marker.
- Alternative: a characters-local const only, which every later column provider (wallet, Step 15) must copy.

**G5. The Characters tab panel is never diffed (admindiff blind spot).**
- Problem: `PAGE_MAP` holds one entry per mockup page stem (`pages:29-40`, validated in `pages:79-80`). The crawl follows the first players row to the default tab only (`crawl:83-98`), and the harness renders `?u=aiko.tanaka` (`pages:33`), so its tab is Profile (`pc:163`).
- This hides every tab panel of Steps 14–20.
- Proposal, built in part `a` as a `pages.rs`/crawl change:
  - a tab view of a `FirstRow` page, e.g. `Page { page: "player", tab: Some("characters"), … }`;
  - the crawl appends `?tab=characters` to the followed href (the portal reads `tab`, `rt:559-563`);
  - the harness search is `?u=aiko.tanaka&tab=characters` (the mock reads `q.get("tab")`, `pc:163`);
  - its own accept file, e.g. `player-characters.txt`.
- Alternative: hand-check only (H7), plus an item under "Contract gaps admindiff cannot see".

**G6. Accept kind for the harness-only `th.num > span` line.**
- Problem: `SKILL.md:98-102` says the shape "is never compared". In fact the diff will print a NEW line, because the mock's bare `th.num` pairs with the portal's `th.num > span` (worked through `sk:451-559`, §8). The portal matches the browser mockup (`pc:109-112`), so the line is a harness artifact, and none of the five kinds fits exactly.
- Proposal: kind `template choice`, with the reason naming `mock:22-23` and `pc:109-112`. Also correct the skill sentence to say the diff reports it.
- Alternative: teach `mock.rs` to apply the players `after` wrap. There is no DOM in the harness, so this is fragile.
- Wallet's Embers column (Step 15) produces the same line text, so one entry covers both.

**G7. Fact wording.**
- The mock prints `${n} characters`, which gives "1 characters" for marta.k (`reg:82`, `data.js:5`).
- Proposal: "1 character" for one, "N characters" otherwise. This is a text deviation, declared in H9.
- Alternative: verbatim.

**G8. Row order and zero characters.**
- Order: the mock lists rows by recency of Last played, which is FAKE. Proposal: the real `created_at, id` order of `list_by_player` (`cs:63-72`). No sort arrow is drawn, so there is no header deviation.
- Zero characters: the mock draws a row even at 0 (`pc:127`). Proposal: the portal's empty state ("Nothing here yet"), declared in H10.

**G9. "Purges pending" KPI (Q1, D6: `plan:2021`, `plan:2349-2352`).**
- Problem: Q1 puts it "in each of the 7 modules' port steps". Its reference doc places it "on each purging module's existing admin page" (`docs/reference/event-plane-ops.md:117-122`), but characters has no page in the new mockup. That doc text is stale: it still names `adminapi::Item`, which was razed in b8435c9a.
- Options:
  - **(a)** A `DashboardAttention` item, "Character purges pending", with tone warn, the text "N players awaiting sweep · oldest <date>", and order after mail 40 (`plan:1855`). It shows only while `pending_purges().0 > 0`, so there is no accept entry on a fresh fleet.
  - **(b)** A `DashboardTile`. This means six permanent extra tiles across ports, each a `portal-only tile` D6 entry in `index.txt`.
  - **(c)** Defer to one later step, which contradicts Q1's "each port step".
- Recommend (a). It also gives `pending_purges` (`cs:163-171`) a caller. Fix the stale doc text in the same rollout.

**G10. Dead readers.**
- `count` (`cs:119-124`) and `list_all` (`cs:190-198`) have no caller **[grep]**, and no mockup element needs them.
- Proposal: part `a` deletes them. `pending_purges` follows G9.

**G11. Sequencing with `docs/plans/2026-11-17-1900-purge-race-locks-plan.md` (untracked).**
- Its Steps 2–3 edit `modules/characters/src/{store.rs,service.rs,projection.rs}` (lines 193-226 of that plan).
- Proposal: the owner orders the two rollouts. Step 14 part `a` touches `store.rs` (new reader, G10 deletions) and `lib.rs`. There is no logical conflict, but there is a text overlap and the purge-race plan's test runs would collide with this step's.

## 8. Accept-file plan

The pairing was worked out by hand from `sk:451-559`; it is a prediction.

**`players.txt`:**
- **Delete `:22`** (`… tbody > tr* > tr.clickable: mock-only td.num`). The portal's Characters `td.num` now anchors the mock's, so the entry goes STALE (resolved).
- **Keep `:20`** (`… thead > tr: mock-only th.num`). The Embers header still has no partner. **Reword its reason** to "not ported yet: the Embers column arrives with the wallet port (Step 15)".
- Keep `:19`, `:21`, `:23` and `:24-27` unchanged. All mock rows gain the same matching `td.num`, so row pairing is not expected to shift (see Not verified).
- **NEW:** `panel "#1" section.panel.players-table > div.flush.panel-body > div.tbl-wrap > table.tbl > thead > tr > th.num: portal-only span | <G6 kind>: the portal draws the span the mockup's meta.after hook adds around a contributed header (pages-core.js:109-112); the mock harness never runs that hook (tools/admindiff/src/mock.rs:22-23)`.

**`player.txt`:**
- Keep `:13`, `:14` (facts `span#1b4ac0`, `span#35b730`). The Lore fact (`span > svg`) and the wallet fact (`span > span > svg`) are still mock-only. **Reword both reasons**: "the wallet fact arrives with the wallet port (Step 15); no lore module exists for the Lore fact".
- Keep `:17-20` (tabs). The span.count shape is still owned by Inventory, Friends, Groups and Inbox, the dot shape by Wallet, the muted lock by Cloud saves, and the bare tab by Lore. **Reword the four reasons** to drop "Characters (Step 14)".
- No new entry while only the default tab is crawled.

**If G5 is adopted**, new `player-characters.txt`:
- Shell-frame copies of `index.txt:5, 6, 7, 8, 9, 11`.
- The header entries as `player.txt:13-20` and `:15` (the tab shapes are unchanged; `on` moves to Characters on both sides).
- The template-choice pair for the tab body wrapper, as `players.txt:11-12`: `frame … div.page: mock-only [panel "Characters"]` and `portal-only div.stack#…`.
- If G1 is rejected: a `contract gap` line in `panel "Characters" … tbody > tr* > tr` (mock `span.mono.muted`, portal `span.mono`).

**`index.txt`:** no change under G9(a). Under G9(b): `frame div.app > main.main > div.page > div.g-tiles.grid: portal-only [tile "<label>"]` and `portal-only tile "<label>"`, kind `declared deviation D6`.

`events.txt` and `mail.txt` have no characters entry **[grep]** and do not change.

## 9. Predicted `docs/todo/admin-second-pass.md` items (part `a` writes them)

New subsection "Fake data of the characters port", continuing after ADMIN2-35:
- FAKE character level.
- FAKE character region.
- FAKE character last played.

Conditional:
- Under **Accepted contract gaps**: the `span.mono.muted` Id cell, if G1 is rejected.
- Under **Contract gaps admindiff cannot see**: the Characters tab panel, if G5 is rejected.
- A docs item (or same-rollout fix) for the stale `docs/reference/event-plane-ops.md:117-122` (G9).

No `implemented: false` items: characters declares no actions.

## 10. `pages.rs` change (part `a`)

None for `players` and `player`, which are already `Mapped` and `FirstRow` (`pages:32-33`). There is no characters page, so no `Pending` entry flips. With G5 adopted, part `a` adds the tab view (a new `Page` field or `Route` variant, and its crawl and validation handling).

## Not verified

- No `cargo`, `admindiff mock`, crawl or diff was run. Every accept line, including the exact hash ids staying put, comes from reading `sk:431-570` by hand.
- Whether row-shape pairing keeps `players.txt:24-27` unchanged once portal rows gain `td.num`.
- Whether core accepts a manifest with no pages, nav or entity kinds, whose contributions name a kind another module declares. Characters is the first such provider.
- Whether a tool-less titled panel's head matches the mock's (relevant to G5's predicted entries).
- Whether the conformance policy has a bounded kind that could replace `Opaque` for `req.refs` (G4).
- Whether the purge-race-locks plan is already being executed.
- The thousands separator at counts of 1,000 or more (`bjs:75` formats with `toLocaleString`). Only reachable at the `LIST_HARD_LIMIT` cap.
