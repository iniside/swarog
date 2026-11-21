# Step 16 (inventory): checklist

Read-only extraction per `.agents/skills/mockup-implementation/SKILL.md` §1 (at 212c7d86). Mockup page: `UILayout/redesign/player.html?tab=inventory` (Inventory tab, row menu). Builder lines inventory owns:
- player: `tabRender.inventory` `pc:130-133` (panel, title, meta and Grant item tool `pc:130`; filters, search, columns `pc:131`; rows `pc:132`; row menu button, foot, pager `pc:133`).
- Registry: tab `reg:70` (`count: () => 37`), row-menu group `reg:105` (one item, "Grant item…", icon `box`, enabled, toast only).
- Purges pending: no mockup line (X1, `plan:1915-1917`); order 63 (`plan:1910-1912`).
- No inventory nav entry, nav section, search provider, dashboard tile, attention item, fact, list column or header action in the registry **[grep]** (`rg -i inventory registry.js`: only `reg:51,59,70,105,117`).
- Out of scope: `reg:51`, `reg:59`, `reg:117` and `dj:145,156` name a demo subscription `inventory.on_purchase.v1` owned by `core.events`, and `pp:159-160` its poison modal; inventory has no such subscription (its four are `il:233,248,274` and `ip:210-211`) **[grep]**. `dj:96` (services row) and `dj:115` (api-key scope) belong to other pages.

Abbreviations: `pc` = `UILayout/redesign/assets/pages-core.js`, `pp` = `…/pages-platform.js`, `reg` = `…/registry.js`, `dj` = `…/data.js`, `cp` = `core/controlplane/src/lib.rs`, `tmpl` = `core/admin/src/templates/primitives.html.tmpl`, `rt` = `core/admin/src/routes.rs`, `tbl` = `core/admin/src/render/table.rs`, `pnl` = `core/admin/src/engine/panels.rs`, `ac` = `tools/admincheck/src/checks.rs`, `act` = `tools/admincheck/src/checks_tests.rs`, `pages` = `tools/admindiff/src/pages.rs`, `il` = `modules/inventory/src/lib.rs`, `is` = `modules/inventory/src/store.rs`, `ip` = `modules/inventory/src/projection.rs`, `io` = `modules/inventory/src/owner.rs`, `ia` = `api/inventory/api/src/lib.rs`, `cha` = `api/characters/api/src/lib.rs`, `che` = `api/characters/events/src/lib.rs`, `ca` = `modules/characters/src/admin.rs`, `wad` = `modules/wallet/src/admin.rs`, `ds` = `docs/research/2026-10-22-1830-admin-redesign-data-sources.md`, `gd` = `docs/research/2026-11-20-2230-admin-ports-contract-gaps.md`, `plan` = `docs/plans/2026-10-22-1759-admin-control-plane-redesign-plan.md`, `acc` = `tools/admindiff/accept`.

Method: targeted reads with line numbers; **[grep]** marks a ripgrep sweep (a lower bound). No cargo, `admindiff mock`, crawl or diff was run; no subagent was used.

## 0. Required check: does admincheck accept a module-owned row kind with `RowAction`s and no `EntityHeader`?

**Yes.**
- `findings` runs `entity_kinds`, `kind_owners`, `entity_tabs` and eleven per-manifest checks (`ac:11-36`); none names `EntityHeader`.
- `entity_kinds` asks only that a `RowAction`'s kind be declared by some manifest (`ac:82-84`, `ac:93-116`). `kind_owners` constrains only `EntityLabel` (`ac:118-158`).
- The admincheck fixture declares kind `message` with no `EntityHeader` contribution (`act:49-71`), and a `RowAction("message")` there yields only the unrelated head-action finding (`act:81-100`): no entity-kind finding.
- Portal side: a row's `menu_url` needs only a `RowAction` on its entity kind (`rt:759-766`, `tbl:229-234`); its `href` (`tr.clickable`) needs an `EntityHeader` of that kind (`rt:768-775`, `tbl:220`). The menu route serves any `RowAction` kind (`rt:885-900`).
- Side effect: `/admin/e/<kind>/<id>` resolves for any declared kind (`rt:542-550`) and draws a page with no header; nothing links to it, since the rows are not clickable.

## What inventory holds (verified)

- `inventory.items(id text PK, name, kind)`, seeded `coin`/Coin/currency, `starter_sword`/Starter Sword/weapon, `health_potion`/Health Potion/consumable (`il:56-65`).
- `inventory.holdings(owner_type 'player'|'character', owner_id uuid, item_id FK, quantity 0..2000000, PK (owner_type, owner_id, item_id))` (`il:67-73`); index `holdings_owner_idx (owner_type, owner_id)` (`il:185-186`). No source, no acquired time, no player id on a character's holding.
- `inventory.wiped_characters` (`il:81-84`), `inventory.purged_players` + `purged_players_pending_idx` (`il:86-91,189-191`).
- Character holdings come from the starter grant on `character.created` (`starter_sword` ×1 by default, `ip:12-13,103,152`); the handler is given only `e.character_id` (`il:241`), though the event carries `player_id` (`che:21-26`). Player holdings come from the dev grant `/inventory/me/grant` (`ia:68-80`), which demoseed calls (`tools/demoseed/src/lib.rs:547-568`).
- Contract (`ia`): `Holding{owner_type, owner_id, item_id, item_name, quantity}` (`ia:32-38`); `Holdings`: `list_mine`, `list_character`, `grant`, all `Identity`-bound (`ia:48-80`). No admin reader.
- characters' contract offers inventory only `Ownership::owner_of(character_id)` (character → player, `cha:51-53`) and the identity-bound `Player::list` (`cha:72`): **no wire call lists a player's characters**.
- Store readers with no non-test caller in `modules/inventory/src` **[grep]**: `pending_purges` (`is:162-170`), `list_detail` + `HoldingDetail` (`is:91,200-220`; the only reader with `kind`), `stats` (`is:246-254`), `list_owners` + `OwnerStat` (`is:80,256-271`). Live: `list` (`is:172-196`, `ORDER BY item_id LIMIT 1000`, no cursor; INVENTORY-04).
- No `controlplane` dependency (`modules/inventory/Cargo.toml`), no `admin.rs` (`ds:23` cites `modules/inventory/src/admin.rs:203`, which does not exist at HEAD).

Carriers already in the contract (verified): `Point::EntityTab` + `EntityTabCount` (`cp:658-659`), `ActionPoint::RowAction` (`cp:714`), `ActionDecl.order` / `implemented` (`cp:675-698`), `Cell.label` (`cp:1127-1130`, 13e3), `Row.entity` (`cp:1156-1164`), `FilterKind::Select` (`cp:1182-1183`), `DataTable.item_noun` + `Noun` (`cp:1207-1226`), `PanelBody.meta` (`cp:1463`), `purge_backlog_body` (`cp:395-417`), `fake_hash` (`cp:212`). A disabled action draws `title` = "Not implemented yet" (`core/admin/src/engine/actions.rs:95`; `tmpl:117-123`, menu `tmpl:141-147`). No contract change unless G1 (c) is chosen.

---

## 1. Element table: player detail, Inventory tab and row menu

| Element | Mockup line | Carrier | Status |
|---|---|---|---|
| Tab "Inventory", fourth, count 37 | `reg:70` | `EntityTab{player, "inventory", "Inventory", 1}` order 40 (`plan:1906`) + `EntityTabCount{player, inventory}` | ok; count DERIV, **G6** |
| Panel title "Inventory", `flush` | `pc:130` | `ContributionDecl.title` | ok |
| Meta "37 items · owner user:&lt;id&gt;" | `pc:130` | `PanelBody.meta` | REAL count; text **G7** |
| Tool "Grant item" (`gift`) | `pc:130` | `ActionPoint::PanelTool("inventory")`, `implemented: false` → disabled | not in the plan's Step 16 text, **G5** |
| Toolbar search "Item id or name" | `pc:131` | `DataTable.search_placeholder` | **G9** |
| Filter chips Owner "all characters", Type "any" | `pc:131` | `FilterKind::Select` (standing rule) | **G9**; chips vs select → accept |
| Item (plain text) | `pc:131-132` | `Text` | REAL `items.name` |
| Type `span.tag` | `pc:132` | `CellKind::Ref` | REAL `items.kind` (D4: currency/weapon/consumable, `il:62-64`) |
| Qty `td.num` | `pc:131-132` | `Number`, `Align::Right` | REAL `quantity` |
| Owner `span.mono` "user" / "character:Yuki" | `pc:132` | `Mono`; character rows `Cell.label{character, id}` (O-7, 13e3) | **G1**, **G2** |
| Source `span.mono.muted` | `pc:132` | `MonoMuted` | no source column, **G3** |
| Acquired (plain "2026-10-22") | `pc:132` | `Text`, not `Time` (`gd:59-63`) | no column, **G3** |
| Row `td.act` more button, row not clickable | `pc:133` | `Row.entity = holding` with a `RowAction("holding")`, no `EntityHeader` (O-6, §0) | **G4** |
| Foot "37 items", pager "1 / 2" | `pc:133` | `PageInfo.total`, `Noun::new("item", "items")` | **G8** |
| Row-menu group "inventory": "Grant item…" (`box`) | `reg:105` | `RowAction("player")`, order 50 (`plan:1909-1910`), `implemented: false` | ok (plan: disabled); mock enabled → accept |
| Purges pending | none (X1) | `DashboardAttention` order 63 → `purge_backlog_body` | REAL `pending_purges`, **G10** |

Shell frame on `player.inventory`: copy `acc/player.characters.txt:6-12` (Sign out, pending nav entries, Lore ×3, D3, theme toggle) and the header lines `:13-15`.

## 2. States per panel, and URLs

| Panel | ok | empty | readonly | stale | error |
|---|---|---|---|---|---|
| Inventory tab | rows, keyset pager | portal Empty (`Primitive::Empty`); narrowed to zero keeps the toolbar: golden `table_empty_search` | n/a: `writes: false` with only `implemented: false` actions draws no read-only state (`pnl:85-91`) | `cache_ttl_s: None` | error block |
| Tab count | number | "0" (always a Cell) | — | — | tab error |
| Purges pending | item while backlog | none at 0 (`cp:396-398`) | — | — | per point |

URLs: search, both filters and the pager build from `Ctx.query` (skill §1) and carry `tab=inventory`; part `a` names the golden case for a tab table's search URL or adds the gap.

## 3. `writes` and every ActionDecl (proposal)

`writes: false` (no implemented action; characters precedent `ca:295`).

| id | Point | Then | Label / icon | order | Emphasis | danger | implemented |
|---|---|---|---|---|---|---|---|
| `item.grant` | `RowAction("player")` | `Form` | "Grant item…" / `box` | 50 | Default | no | false |
| `item.grant-tool` | `PanelTool("inventory")` | `Form` | "Grant item" / `gift` | 0 | Default | no | false (G5) |
| per G4 | `RowAction("holding")` | per G4 | per G4 | 0 | Default | per G4 | false |

Each `implemented: false` action gets an item under `docs/todo/admin-second-pass.md` **Not-implemented actions** (`:111`) in the part `a` commit.

## 4. Contributions and `order`

| Contribution | Point | order |
|---|---|---|
| `inventory` | `EntityTab{player, inventory, "Inventory", 1}` | 40 |
| `tab.count` | `EntityTabCount{player, inventory}` | 0 |
| `attention.purges` | `DashboardAttention` | 63 |

Entity kind `holding` declared (`EntityKindDecl{holding, "Holding", "box"}`), no `EntityHeader`, no `EntityLabel`. All `min_role: Viewer`. Wiring as wallet: `controlplane` dependency, `admin.rs`, `PROVIDER_SLOT` (`modules/wallet/src/lib.rs:380-382`).

## 5. Hand-check section (what the §7 `diff` cannot see)

| # | Element | Check |
|---|---|---|
| H1 | Tab label "Inventory", count value, order after Wallet | live look |
| H2 | Meta text (G7) | live look |
| H3 | Grant item tool and row-menu item: disabled, title "Not implemented yet" (mock enabled) | live look |
| H4 | Search placeholder, filter labels and options (G9) | live look |
| H5 | Owner text: "player" vs the mock's "user"; character label without the `character:` prefix (13e3, `plan:2965-2966`) | live look; golden for an unlinked labelled `span.mono` |
| H6 | Source / Acquired values (G3) | live look |
| H7 | Qty right-aligned, no sort arrow, foot text and page label | live look |
| H8 | Rows not clickable; `td.act` menu opens the `holding` menu (G4) | live look |
| H9 | Purges pending item (G10) | golden or live look after deleting a player |

## 6. Gaps and decisions for the owner

**G1. Which holdings the tab lists.** The mock lists player and character holdings under an Owner filter "all characters" (`pc:131-132`). Inventory cannot list a player's characters: a character holding stores no player id (`il:67-73`), the starter handler drops `player_id` (`il:241`, `che:21-26`), and characters offers no wire call from player to characters (`cha:51-53,72`). On a dev fleet the starter item is character-owned (`ip:12-13,152`), so a player-only tab is empty for a player who has only characters.
- Options: (a) player-owned holdings only; character rows and the Owner filter become a `contract gap` accept entry; (b) inventory records character → player in its own table from `character.created`; (c) a wire-only characters capability listing a player's character ids, capped, called by the tab.
- (b): the table is additive (`CREATE TABLE IF NOT EXISTS`; the doctrine is DROP + boot fresh anyway). The handler already receives `e.player_id` (`il:237-241`, `che:21-26`), and a new subscription at `StartPosition::Genesis` (as `il:234`) replays retained `character.created` (`MinRetention{7 days}`, `che:43`). A character's owner never changes (`cha:63-78` has no transfer op), so the projection is exact; `character.deleted` removes the row with the holdings. No contract change, one indexed query, and the tab and its count do not need characters-svc.
- (c): a contract change in `api/characters`; in the split the tab, its count and the keyset count depend on characters-svc being up; one extra hop per page; a new unauthenticated internal call that lists any player's characters; the count becomes `owner_id IN (<list>)` across N owners (G8).
- **Recommendation: (b)**: the full mock with no contract change and no cross-service dependency. (a) only if Step 16 must stay schema-free; then a `docs/todo/inventory.md` item.

**G2. Owner cell.** Player rows: `Mono` "player" (the real owner type, `io:6-13`; D4) instead of the mock's "user". Character rows: `Mono` with value = character id and `Cell.label{character, id}` (O-7), drawn "Yuki" (characters' `EntityLabel`, `ca:272-274`); an unresolved label draws the uuid. **Recommendation: as written**; text-only differences in H5.

**G3. Source and Acquired.** Neither exists (`il:67-73`); `ds:87` says FAKE. Standing rule: a truthful constant beats FAKE. **Recommendation: "—"** in both (as characters' Last played, `ca:202`). Alternative: `fake_hash` picks over the mock's vocabulary, each with a `// FAKE:` marker and an admin-second-pass item.

**G4. The `holding` row kind and its menu.** O-6 needs at least one `RowAction("holding")` or no `td.act` is drawn (`tbl:229-234,258`). The mock's row button has no handler (`pc:133`), so it names no item. Entity id: `<owner_type>:<owner_id>:<item_id>` (the PK, `il:72`).
- Options: (a) one `implemented: false` item, "Revoke item…" (`trash`, danger), with a todo item; the mockup has no such item (its button has no handler, `pc:133`), so this invents a menu entry; the menu is not diffed: add `("player.inventory", "holding", "the mockup's inventory row button has no handler (pages-core.js:133)")` to `EXCLUDED_MENUS` (`pages:350-354` precedent). (b) no row action and a `contract gap` accept line for the missing `td.act`.
- **Recommendation: (b)**, the more faithful: it adds nothing the mockup lacks. Whether O-6's wording requires a row action here is a plan-wording question for the owner.

**G5. The tab's "Grant item" tool** (`pc:130`) is not named in the plan's Step 16 text (`plan:2164-2168`). **Recommendation:** draw it as a disabled `PanelTool` (copy the mockup), sharing the row item's todo entry.

**G6. Tab count meaning** (`gd:187`). **Recommendation:** the number of holding rows (stacks), equal to the foot total (mock 37 = 37); not the sum of quantities.

**G7. Meta text.** **Recommendation:** "N items · owner player:&lt;full uuid&gt;" with `Noun::of` (identifiers in full; `player` per D4).

**G8. Paging.** `list`/`list_detail` have no cursor (`is:172-220`). **Recommendation:** a keyset reader ordered by the PK `(owner_type, owner_id, item_id)` (`il:72`), portal page size, exact `count(*)` per owner (bounded by `holdings_owner_idx`), no sort arrow. It replaces `list_detail` for the tab (G11) and closes INVENTORY-04 for this view only.

**G9. Toolbar.** Select options are plain strings (`cp:1182-1183`), so an option cannot carry a character's label. **Recommendation:** Owner `Select(["player", "character"])` (owner type) rather than one option per character uuid; Type `Select` over the catalog's kinds; search: `item_id` or name, case-insensitive substring, within the owner's rows.

**G10. Purges pending at 63** (X1). Not a gap: `purge_backlog_body("Inventory purges pending", pending, oldest, PURGE_SWEEP_SUB.id)` (`cp:395`; sub `inventory.purge-on-scheduler.v1`, `ip:210-211`), as `ca:190-192` and `wad:662`; reader `is:162-170`.

**G11. Dead readers (X4).** Reuse `pending_purges` (G10) and `HoldingDetail`'s shape for the tab reader; delete `stats`, `list_owners`, `OwnerStat` and `list_detail` (`is:200-220`, no caller). No test references them **[grep]** (they appear only at `is:80,200,246,256,269`).

## 7. Accept-file plan (predicted; the exact text needs a crawl)

- `pages`: drop `player.inventory` from `EXCLUDED_TABS` (`pages:338`), add its `PAGE_MAP` tab entry (no `force`: the mock draws ok), the G4 `EXCLUDED_MENUS` row, and `EXCLUDED_MODALS` rows for none (every inventory action is disabled, so no modal opens).
- **`player.inventory.txt`** (new): shell copies (§1); chips vs `Select` (copy `events.txt`'s pair if the same) and the `span.input-icon` / `form.input-icon` search pair (`gd:63-64`); Source/Acquired only if the tag changes (G3, text otherwise); rows: the mock's five against the seeded player's → `data difference` until 24b1; the mock's Type values `cosmetic`/`material` are not in the real catalog (`il:62-64`) → `data difference`; under G1 (a) a `contract gap` line for the missing character rows; disabled tool (mock enabled) → `contract gap` (`implemented: false`).
- **`player.txt`, `player.characters.txt`, `player.wallet.txt`**: one `mock-only button.tab … EXCLUDED_TABS` line (`player.characters.txt:17-18`) now pairs; expect a count-`span` difference or none.
- **Menu files** (`players.menu.user`, `player.menu.user`, `player.characters.menu.user`, `player.wallet.menu.user`): the `menu-group#6c5cf7` line (`player.menu.user.txt:9`) is reworded to drop inventory; the "Grant item…" pair, mock enabled vs portal `div.disabled.menu-item` → `contract gap` (`implemented: false`).
- **`index.txt`**: no line for Purges pending on a fresh fleet.

## Not verified

- No cargo, `admindiff mock`, crawl or diff was run; every accept line in §7 is a prediction.
- What the mock's row-menu "Grant item…" draws as a tag (`reg:105`, `run` with no `disabled`) **[hypothesis: `button.menu-item`]**.
- Whether the purge sweep's player-only `clear_owner_exec` (`ip:264`) leaves a deleted player's character holdings to `character.deleted`: characters' purge emits `character.deleted` per removed character (`modules/characters/src/projection.rs:95`) and inventory's `wipe_character` clears character holdings (`il:256`, `ip:165`); not traced end to end.
