# Step 17 (friends): checklist

Read-only extraction per `.agents/skills/mockup-implementation/SKILL.md` §1 (at db96c7a8). Mockup page: `UILayout/redesign/player.html?tab=friends` (Friends tab). Builder lines friends owns:
- player: `tabRender.friends` `pc:134-135` (panel, module `friends`, title "Friends", meta "12 · 1 pending", `flush` `pc:134`; columns User/State/Since, five rows, `userCell`, state badge, row menu button, foot "12 friends", page "1 / 1" `pc:135`).
- Registry: tab `reg:71` (`count: () => 12`). No friends row-menu group (`reg:95-112`), header action (`reg:88-92`), fact (`reg:79-85`) or attention item (`reg:57-63`).
- Purges pending: no mockup line (X1, `plan:1915-1917`); order 64 (`plan:1910-1912`).
- No other friends element **[grep]** (`rg -i friend UILayout/redesign/assets`: only `pc:134,135,146`, `dj:98`, `reg:71`). Out of scope: `pc:146` (an Inbox timeline entry, Step 20), `dj:98` (services row, another page).
- The table has no `filters`, so the mock draws no toolbar, search or filter chips (`bj:155`); it has no `pager: false`, so it draws the foot and pager (`bj:163`).

Abbreviations: `pc` = `UILayout/redesign/assets/pages-core.js`, `reg` = `…/registry.js`, `dj` = `…/data.js`, `bj` = `UILayout/design-system/components/bundle.js`, `cp` = `core/controlplane/src/lib.rs`, `tmpl` = `core/admin/src/templates/primitives.html.tmpl`, `tbl` = `core/admin/src/render/table.rs`, `cr` = `core/admin/src/render/cell.rs`, `lb` = `core/admin/src/engine/labels.rs`, `pages` = `tools/admindiff/src/pages.rs`, `adt` = `tools/admindiff/src/tests.rs`, `apt` = `tools/admindiff/src/pages_tests.rs`, `act` = `tools/admindiff/src/crawl_tests.rs`, `fl` = `modules/friends/src/lib.rs`, `fs` = `modules/friends/src/store.rs`, `fp` = `modules/friends/src/projection.rs`, `fsv` = `modules/friends/src/service.rs`, `ft` = `modules/friends/src/tests.rs`, `fa` = `api/friends/api/src/lib.rs`, `fe` = `api/friends/events/src/lib.rs`, `aa` = `modules/accounts/src/admin.rs`, `iad` = `modules/inventory/src/admin.rs`, `is` = `modules/inventory/src/store.rs`, `spm` = `tools/splitproof/src/main.rs`, `siv` = `tools/splitproof/src/inventory_admin_checks.rs`, `ds` = `docs/research/2026-10-22-1830-admin-redesign-data-sources.md`, `gd` = `docs/research/2026-11-20-2230-admin-ports-contract-gaps.md`, `gk` = `docs/research/2026-11-17-ports-contract-gaps.md`, `plan` = `docs/plans/2026-10-22-1759-admin-control-plane-redesign-plan.md`, `acc` = `tools/admindiff/accept`, `s16` = `docs/plans/2026-11-22-step16-inventory-checklist.md`.

Method: targeted reads with line numbers; **[grep]** marks a ripgrep sweep (a lower bound). No cargo, `admindiff mock`, crawl or diff was run; no subagent was used. The read stopped at the context budget; what it did not reach is under **Not verified**.

## 0. Required check: is the O-6 row-kind pattern available without a contract change?

**Yes; Step 16 landed it.**
- O-6 (`plan:2964-2965`): a row with a menu that is not clickable is a module-owned row kind with `RowAction`s and no `EntityHeader`, no contract change; Step 17 is named there.
- Inventory ships exactly that: kind `holding` declared (`iad:319-323`), a `RowAction("holding")` (`iad:332-339`), no `EntityHeader`; rows carry `Row.entity = holding` (`iad:195-196`). The admincheck analysis is `s16` §0 (not re-read here).
- Portal: a row's `href` needs an `EntityHeader` kind (`tbl:220`); its `menu_url` needs a `RowAction` kind (`tbl:230-234`).
- **The trap `gd:113` names is avoided by this pattern:** a user cell drawn in a row with no `href` keeps `inner_link: false` (`tbl:159-163`, `cr:97`), so a linked `User` cell draws `a[style=color:inherit] > span.user-cell > span.avatar + span` (`tmpl:83,91`), the mockup's `userCell` nesting (`bj:102-107`).

## What friends holds (verified)

- `friends.edges(id uuid PK, low_id, high_id, requester_id, state 'pending'|'accepted', created_at, accepted_at nullable)`, one row per ordered pair (`fl:39-51`). Indexes: unique `(low_id, high_id)`, side indexes `(low_id, state, created_at DESC, id DESC)` and `(high_id, state, created_at DESC, id DESC)` (`fl:120-135`). `accepted_at` is set on accept (`fs:158,237`).
- **No handle or name is stored** in `friends.edges` (`fl:41-51`). Handles are denormalized only into the three durable events, as emit-time snapshots (`fe:13-14`, `fe:63-68`); the player API hydrates names through the wire-only `accountsapi::Directory` at read time (`fsv:402-440`, `fl:151-154`). So the friend's name on the tab comes from accounts, not from friends (G6).
- **Presence**: `Friend.online_until` (`fa:78`) comes from the directory answer (`fsv:206`), not from a friends table. The mockup draws no presence on this tab (`pc:135`); nothing to port.
- Purge: tombstone `friends.purged_players` + `purged_players_pending_idx` (`fl:52-57,136-140`), planted by `friends.player-deleted.v1` (`fl:186-203`); drained by the sweep sub `friends.purge-on-scheduler.v1` (`fp:13-14`, `fl:207-214`), which deletes the edges on both sides (`fs:338-351`). Backlog reader `pending_purges` (`fs:304-312`), tested (`ft:1596-1616`).
- Player readers are caller-relative and one state per call (`fs:394-431`, UNION ALL of two side branches, keyset `(created_at, id)`, `fs:380-393` for why).
- Admin readers with **no caller** **[grep]** (`rg admin_counts|admin_recent|AdminEdgeRow modules/friends`: only `fs:28,436,467,471,497,499`): `admin_counts(player_id, pending, accepted) -> (total, pending, accepted)` in one statement (`fs:436-462`); `admin_recent` (`fs:467-494`, not keyset, `LIMIT` only), `AdminEdgeRow` (`fs:28-34`), `row_to_admin_edge` (`fs:497-506`). Leftovers of the pre-redesign `/admin/friends` page.
- Vocabulary: `STATE_PENDING`/`STATE_ACCEPTED` (`fa:38-41`), `DIRECTION_INCOMING`/`DIRECTION_OUTGOING` (`fa:44-47`). Unbounded accepted edges per player (`fl:173-176`); authored pending capped at 100 (`fa:57`).
- Wiring today: no `admin.rs` (`modules/friends/src` holds `conformance, lib, projection, service, store, tests`), no `controlplane` dependency **[grep]** (`modules/friends/Cargo.toml`), no `PROVIDER_SLOT` contribution (`fl:146-216`).
- **Both topologies (check 1):** the provider is contributed in `init` as inventory does (`modules/inventory/src/lib.rs:364-366`); every edge peer serves `cp.*` from `PROVIDER_SLOT` (`core/app/src/lib.rs:1030-1034`); admin-svc already dials friends at `:9014` (`cmd/admin-svc/src/main.rs:41`, `cmd/admin-svc/src/lib.rs:36`). The tab reads only friends' own schema; the name label is the portal's label pass to accounts (`lb:105-119`), a second hop in the split (to accounts-svc, `cmd/admin-svc/src/main.rs:34`). Friends itself makes no accounts call for the tab.

Carriers already in the contract (verified): `Point::EntityTab` with `params` + `EntityTabCount` (`cp:661-662`), `Point::EntityLabel` (`cp:672`), `ActionPoint::RowAction` (`cp:717`), `Link::Entity` (`cp:372-375`), `CellKind::User`/`Badge`/`Text` (`cp:1092-1113`), `Cell.tone`/`link` (`cp:1118-1134`; never `link` and `label` together, `cp:1130-1132`, refused at `lb:70-75`), `Tone::{Ok, Info, …}` (`cr:33-36`), `Row.entity` (`cp:1159-1167`), `PageInfo.total` (`cp:1170-1173`), `DataTable.item_noun` + `Noun` (`cp:1211-1230`), `PanelBody.meta` (`cp:1466`), `purge_backlog_body` (`cp:395-413`), `MAX_PANEL_REFS` (`cp:208`). accounts answers `EntityLabel{player}` (`aa:396`) with the handle (`aa:626-628`). **No contract change; no Na0.**

---

## 1. Element table: player detail, Friends tab

| Element | Mockup line | Carrier | Status |
|---|---|---|---|
| Tab "Friends", fifth, count 12 | `reg:71` | `EntityTab{player, "friends", "Friends", 1, params: []}` order 50 (`plan:1905-1906`) + `EntityTabCount{player, friends}` | ok; count meaning **G4** |
| Panel title "Friends", `flush` | `pc:134` | `ContributionDecl.title` | ok |
| Meta "12 · 1 pending" | `pc:134` | `PanelBody.meta` | REAL counts (`fs:436-462`); text **G4** |
| No toolbar (no search, no filters) | `pc:135`, `bj:155` | `filters: []`, `search_placeholder: None` → no toolbar (`tbl:262`) | ok |
| User: linked `a > span.user-cell` (avatar + handle) | `pc:135`, `bj:102-107` | `CellKind::User`, value = other player's id, `link: Link::Entity{player, id}`; label pass → handle | ok (§0); text **G6** |
| State badge: "accepted" ok, "pending (outgoing)" info | `pc:135`, `bj:97` | `CellKind::Badge` + explicit `Tone::Ok`/`Tone::Info` | REAL; direction **G2** |
| Since, plain "2025-03-14" | `pc:135` | `CellKind::Text`, not `Time` (`gd:60-63`) | REAL; which time **G3** |
| Row `td.act` more button, row not clickable | `pc:135`, `bj:157,161` | `Row.entity = friendship` + a `RowAction("friendship")`, no `EntityHeader` (O-6, §0) | **G5** |
| Foot "12 friends", pager "1 / 1" | `pc:135`, `bj:163` | `PageInfo.total`, `Noun::new("friend", "friends")`, page label (`tbl:82-91`) | text differs, **G4**/H6 |
| Purges pending | none (X1) | `DashboardAttention` order 64 → `purge_backlog_body` | REAL, **G8** |

Shell frame on `player.friends`: copy `acc/player.inventory.txt:7-13` (Sign out, pending nav entries, Lore ×3, D3, theme toggle) and the entity-head lines `:14-16`; the tab-bar lines `:17-20` change with the friends tab now drawn (§7).

## 2. States per panel, and URLs

| Panel | ok | empty | readonly | stale | error |
|---|---|---|---|---|---|
| Friends tab | rows, keyset pager | portal Empty (`Primitive::Empty`); no search/filter, so no narrowed-to-zero state | n/a: `writes: false` with only `implemented: false` actions (`s16` §2) | `cache_ttl_s: None` | error block |
| Tab count | number | "0" (always a Cell) | — | — | tab error |
| Purges pending | item while backlog | none at 0 (`cp:396-398`) | — | — | per point |

URLs: only the pager's next link (`tbl:270`, built from `ctx.query`); no search or filter URL. The tab declares no `params`.

## 3. `writes` and every ActionDecl (proposal)

`writes: false` (inventory precedent `iad:342`).

| id | Point | Then | Label / icon | order | Emphasis | danger | implemented |
|---|---|---|---|---|---|---|---|
| per G5 | `RowAction("friendship")` | `Invoke` (as `iad:292`) | "Remove friend…" / `trash` | 0 | Default | yes | false |

Each `implemented: false` action gets an item under `docs/todo/admin-second-pass.md` **Not-implemented actions** in the part `a` commit.

## 4. Contributions and `order`

| Contribution | Point | order |
|---|---|---|
| `friends` | `EntityTab{player, friends, "Friends", 1, []}` | 50 |
| `tab.count` | `EntityTabCount{player, friends}` | 0 |
| `attention.purges` | `DashboardAttention` | 64 |

Entity kind `friendship` declared (`EntityKindDecl{friendship, "Friendship", "users"?}`; icon is a part-a choice), no `EntityHeader`, no `EntityLabel`. All `min_role: Viewer` (`iad:274`). Wiring as inventory: `controlplane` dependency, `admin.rs`, `PROVIDER_SLOT` in `init` (`modules/inventory/src/lib.rs:364-366`).

## 5. Hand-check section (what the §7 `diff` cannot see)

| # | Element | Check |
|---|---|---|
| H1 | Tab label "Friends", count value, order after Inventory | live look |
| H2 | Meta text (G4) | live look |
| H3 | User cell text: the real handle `Name#1234` (D4) for the mock's "jonas.l"; `href` = the player's entity page; an unresolved label draws the uuid (`lb:11-12`) | live look; golden or unit test of the label |
| H4 | Avatar colour is seeded from the cell value, the uuid (`cr:92`); initials from the label (`cr:93`) | live look (attribute) |
| H5 | State text and tone per G2 | live look |
| H6 | Foot text: portal "N friends · showing 1–k" on page 1 (`tbl:204-206`) vs the mock's "12 friends"; page label "1 / N" | live look |
| H7 | Since values per G3, no sort arrow | live look |
| H8 | Rows not clickable; `td.act` opens the `friendship` menu, item disabled, title "Not implemented yet" | live look |
| H9 | Purges pending item (G8) | golden or live look after deleting a player |

## 6. Gaps and decisions for the owner

**G1. Which rows and in what order.** The mock lists accepted and pending rows together (`pc:135`). The player readers are one state per call and caller-relative (`fs:394-431`).
- **Recommendation:** every edge the player is party to, both states, both directions; a new keyset reader on the `page` pattern (UNION ALL of the two side branches, each with its own `LIMIT`, `fs:380-393,532-541`), ordered `(state, created_at DESC, id DESC)`, which the side indexes `(x_id, state, created_at DESC, id DESC)` serve directly (`fl:126-135`): accepted rows first, then pending. Page size 25 as inventory (`is:80`); no sort arrow (the mock draws none, `pc:135`).
- Alternative: order by `created_at` alone across states; the side indexes lead with `state`, so each branch sorts all the player's edges, unbounded (`fl:173-176`).

**G2. State text and pending direction** (`gk:79`, `gd:188`). **Recommendation:** "accepted" (`Tone::Ok`); "pending (outgoing)" when `requester_id` is the viewed player, else "pending (incoming)" (`Tone::Info`), computed in SQL as `fs:210` does; real words from `fa:38-47`. Set the tone explicitly; do not rely on the value-keyed default (`cr:84`).

**G3. Since.** **Recommendation:** `accepted_at` for an accepted edge, `created_at` for a pending one (`fl:47-48`), rendered `YYYY-MM-DD` in SQL (standing rule; `fs:13-14` precedent), as `CellKind::Text` (`gd:60-63`). Alternative: `created_at` for both (when the request was made).

**G4. Counts, meta and foot.** The mock draws 12 in the tab, the meta and the foot (`reg:71`, `pc:134-135`), and its five rows include one pending row, so 12 reads as all relations with 1 of them pending.
- **Recommendation:** N = all edges the player is party to (both states) = tab count = `PageInfo.total`; meta "N · M pending" with M the pending count, drawn always (also "· 0 pending"); foot noun `Noun::new("friend", "friends")`. One statement: reuse `admin_counts` (`fs:436-462`, exact `count(*)` served by the two side indexes).
- Alternative: N = accepted only; then the foot total (all listed rows) and the tab count disagree.

**G5. The `friendship` row kind and its menu.** O-6 needs at least one `RowAction("friendship")` or no `td.act` is drawn (`tbl:230-234,258`). The mock's row button has no handler (`pc:135`). Entity id: `friends.edges.id` (`fl:42`).
- Options: (a) one `implemented: false` "Remove friend…" (`trash`, danger) with a todo item (friends has only the identity-bound `remove`, `fa:166-168`; no admin op) and an `EXCLUDED_MENUS` row `("player.friends", "friendship", "the mockup's friends row button has no handler (pages-core.js:135)")` (precedent `pages:373`); (b) no row action and a `contract gap` accept line for the missing `td.act`.
- **Recommendation: (a)**, the owner's Step 16 choice for the identical inventory case (`s16` §8 G4).

**G6. User cell and the friend's name** (`gd:113`, `gk:32`). Not a gap: `CellKind::User`, value = the other player's id, `link: Link::Entity{player, id}`, no `label` (`cp:1130-1132`); `drawn_ref` picks the link (`lb:16-22`) and the label pass asks accounts' `EntityLabel{player}` (`lb:105-132`, `aa:396,626-628`). Friends stores no handle (`fl:41-51`) and needs no directory call for the tab. A deleted player whose edges the sweep has not removed yet draws its uuid (`lb:11-12`). **Recommendation: as written**; text-only differences in H3/H4.

**G7. Toolbar.** The mock draws none (`pc:135`, `bj:155`). **Recommendation:** no search, no filters; no `table_empty_search` case applies.

**G8. Purges pending at 64** (X1). Not a gap: `purge_backlog_body("Friends purges pending", pending, oldest, PURGE_SWEEP_SUB.id)` (`cp:395`; sub `fp:13-14`; reader `fs:304-312`), as `iad:263-264`.

**G9. Dead readers (X4).** Reuse `admin_counts` (G4). Delete `admin_recent`, `AdminEdgeRow` and `row_to_admin_edge` (`fs:28-34,467-506`; no caller **[grep]**). That closes FRIENDS-06 (`docs/todo/friends.md:41-45`, the `admin_recent` ordering): move it to `friends-done.md` in the same commit. FRIENDS-04 (`docs/todo/friends.md:11-15`) asks for a live check of the old `/admin/friends` page, which no longer exists: **recommend** closing it as obsolete in the same commit (owner).

**G10. Stale splitproof FR8.** `spm:2098-2116` asserts the old `/admin/friends` page ("Relations", "REQUESTER"/"ADDRESSEE"); the plan rewrites FR8 in 25h4 (`plan:3146,3321`). **Recommendation:** 17t writes `friends_admin_checks.rs` (§9) and FR8's rewrite stays in 25h4 as planned, now pointed at the Friends tab; or 17t deletes FR8 and 25h4 drops it from its list. Owner picks.

**G11. admindiff tests hard-code `player.friends`.** Dropping `player.friends` from `EXCLUDED_TABS` (`pages:356`) breaks `adt:258-260`, `adt:357-378` and `apt:77-78,95-96`; the four player-host lists in `act` (`:1281-1286`, `:1314-1319`, `:1677-1682`, `:2099-2104`) need the friends hosts (todo `docs/todo/admin.md:59-63`).
- Options: (a) retarget the examples to `player.groups` and extend the four lists (Steps 18–20 repeat it); (b) close `admin.md:59-63` now: pick the example from `EXCLUDED_TABS` and derive the hosts from `PAGE_MAP`.
- **Recommendation: (b)** as a separate small part before 17a (tools only, ~15 tool calls), since every remaining tab port hits it.

## 7. Accept-file plan (predicted; the exact text needs a crawl)

- `pages`: drop `player.friends` from `EXCLUDED_TABS` (`pages:356`); add `PAGE_MAP` tab entries `player`/`friends` with and without `USER_MENU` (as `pages:302-319`); the G5 `EXCLUDED_MENUS` row; `EXCLUDED_MODALS` rows for `player.friends.menu.user` `wallet/currency.grant`/`revoke` (as `pages:436-444`) and `mock_modal` rows for `player.friends` and `player.friends.menu.user` (as `pages:615-620`); no modal for the disabled item.
- **`player.friends.txt`** (new): shell copies (`acc/player.inventory.txt:7-13`); entity-head lines (`:14-16`); tab-bar lines (`:17-20`) re-derived (one fewer "not ported yet" tab, the moved selected tab now Friends); no toolbar lines (neither side draws one); no `div.stack` pair (`acc/player.inventory.txt` carries none). Rows: the mock's five against the crawled player's → `data difference` until 24b2 seeds friends (`plan:3067-3068`); if the crawled player has no friendship the portal draws Empty, and the mock-only table, foot and `td.act` lines are one `data difference`.
- **`player.friends.menu.user.txt`** (new): copy `acc/player.inventory.menu.user.txt` with the reasons adjusted.
- **`player.txt`, `player.characters.txt`, `player.wallet.txt`, `player.inventory.txt`** and their `.menu.user` files: the Friends tab now pairs, so one "not ported yet" `button.tab` line goes or changes per file.
- **`index.txt`**: no line for Purges pending on a fresh fleet.

## 8. Part sizes (estimate)

| Part | Content | Files | Tool calls |
|---|---|---|---|
| 17a0 (G11 b, tools) | derive admindiff test examples and hosts | 3 | ~15 |
| 17a (Na1 with Na3, `plan:1959-1963`) | `admin.rs`, keyset reader + counts reuse + deletions in `store.rs`, `lib.rs`, `Cargo.toml`, admin tests, `pages.rs`, two todo files | 8 | ~55-60 (audit: 55, `docs/research/2026-11-18-2205-admin-plan-step-size-audit.md:29`) |
| 17t | splitproof `friends_admin_checks.rs` + two call sites | 2-3 | ~35-45 |
| 17b | crawls, accept files of §7 | 6-10 accept files | ~20-30 |

No Na0: every carrier exists (§ "Carriers").

## 9. Splitproof (17t, on the `siv` pattern)

Pattern: `siv:14-21` (doc of what crosses), `siv:22-24` (IV0 completion check), checks `siv:90-121`, called from `spm:582-583` (monolith) and `spm:6141-6142` (split). Proposal, both topologies, each with a recorded break run:
- **FA1** two pass-minted players with one accepted and one pending edge (made through the public API as `spm:1838-1895` does): the viewed player's Friends tab draws both rows, each user cell labelled with the other player's handle, not a uuid. In the split the panel is built in friends-svc (`:9014`) and the label comes from accounts-svc: the at-risk path.
- **FA2** the pending row reads "pending (outgoing)" on the requester's tab and "pending (incoming)" on the addressee's (G2).
- **FA3** the tab count and meta read 2 · 1 pending (G4).

## Not verified

- No cargo, `admindiff mock`, crawl or diff was run; every accept line in §7 is a prediction.
- admincheck's acceptance of a second header-less row kind: relied on `s16` §0 and inventory having landed (`iad:319-339`), not re-read.
- `status_tone`'s mapping of "accepted"/"pending" (`cr:84`); G2 sets the tone explicitly anyway.
- Whether the dev fleet (demoseed) creates any friendship, i.e. whether the crawl's first player draws rows or Empty.
- The `purge_backlog_body` call sites in characters and wallet (the grep did not complete); inventory's (`iad:263-264`) was read.
- Where inventory's admin tests live (no `modules/inventory/src/admin_tests.rs` at HEAD); part `a` follows that precedent.
- `EXCLUDED_MODALS`/`mock_modal` rows actually required for the friends hosts (validator logic `pages:922-1060` not traced).
- `docs/todo/admin.md:29-41` (ADMIN-02/03/04) cite `modules/friends/src/admin.rs`, which does not exist at HEAD; not changed here.
