# Step 15 (wallet): checklist

Read-only extraction per `.agents/skills/mockup-implementation/SKILL.md` §1 (at b69e4f90). Mockup pages: `UILayout/redesign/economy.html` (`P.economy`), `players.html`, `player.html` (Wallet tab, fact, row menu) and `index.html` (tile). Builder lines wallet owns:
- economy: `pp:164-185` (Currencies `pp:168-169`, Grant or revoke `pp:170-177`, Ledger `pp:178-182`, head and layout `pp:183`).
- players: Embers column `pc:76` (header), `pc:92` (cell, hard-coded unavailable), banner `pc:106` (hard-coded).
- player: `tabRender.wallet` `pc:128-129`.
- Registry: nav section `reg:11`, nav entry `reg:25` (`down: true`), search `reg:41` (out of scope, O-5), tile `reg:52` (`state: "error"`), attention `reg:58` (FAKE demo item, `ds:44`; not this port), tab `reg:69` (no `count`, `state: "error"`), fact `reg:84` (hard-coded unavailable), row-menu group `reg:101-104` (hard-coded disabled, note "unavailable").
- Out of scope: `reg:116` (a `notification.source` demo item "wallet-svc is down", `module: "wallet"`): a demo outage the shell's notification source draws, not a wallet contribution; no plan step names `notification.source` **[grep]**; its line is a `data difference` (demo) wherever it is diffed. `pc:23` ("Wallet balance" impact row in the Delete-account confirm): accounts' `ConfirmSpec` (`pc:21`, Step 13), so accounts owns it; the hard-coded "unknown (wallet unavailable)" is a `data difference` (demo) in accounts' accept file for that modal.
- Rows: `dj:4-17` (`wallet: {EMB, GLD, TOK}`), `dj:163-171` (ledger), `dj:173-176` (currencies).

Abbreviations: `pp` = `UILayout/redesign/assets/pages-platform.js`, `pc` = `…/pages-core.js`, `reg` = `…/registry.js`, `dj` = `…/data.js`, `bjs` = `UILayout/design-system/components/bundle.js`, `cp` = `core/controlplane/src/lib.rs`, `tmpl` = `core/admin/src/templates/primitives.html.tmpl`, `shell` = `core/admin/src/templates/shell.html.tmpl`, `rt` = `core/admin/src/routes.rs`, `fr` = `core/admin/src/render/form.rs`, `pages` = `tools/admindiff/src/pages.rs`, `wl` = `modules/wallet/src/lib.rs`, `ws` = `modules/wallet/src/store.rs`, `wsv` = `modules/wallet/src/service.rs`, `wp` = `modules/wallet/src/projection.rs`, `wa` = `api/wallet/api/src/lib.rs`, `ca` = `modules/characters/src/admin.rs`, `ds` = `docs/research/2026-10-22-1830-admin-redesign-data-sources.md`, `gd` = `docs/research/2026-11-20-2230-admin-ports-contract-gaps.md`, `plan` = `docs/plans/2026-10-22-1759-admin-control-plane-redesign-plan.md`, `acc` = `tools/admindiff/accept`.

Method: targeted reads with line numbers; **[grep]** marks a ripgrep sweep (a lower bound). No cargo, `admindiff mock`, crawl or diff was run; no research subagent was used.

Comparison scope: as in the Step 14 checklist (tags, classes and nesting; no attributes, no text). New for this port:
- The mockup draws wallet **only down or stale**. `Page.force` (`pages:68-73`, plan T4) sets `UI.state.force = "ok"`; `panel()` and `kpi()` honour it for a contributed fragment (`bjs:131,177`). Hard-coded demo markup is out of its reach: the players cell and banner (`pc:92,106`), the fact (`reg:84`), the row-menu group (`reg:101-104`), the nav `down` class (`reg:25,140`), the Grant panel's `o.banner` (drawn in ok, `bjs:140-142`) and its `disabled: true` fields (`pp:173-176`).
- Forced ok, the Transactions panel (`pc:129`) has no body: `o.body` is undefined (`bjs:138`).

What wallet holds (verified):
- `wallet.currencies(code PK ≤32B, display_name ≤64B, kind ≤32B DEFAULT 'soft', decimals 0..18, created_at)` (`wl:54-68`). No cap column.
- `wallet.balances(player_id uuid, currency FK, amount 0..10^15, updated_at, PK (player_id, currency))` (`wl:70-79`). No index leads with `currency`.
- `wallet.ledger(seq bigserial, id uuid PK, idempotency_key UNIQUE, player_id, currency, delta, balance_after, reason, at clock_timestamp())` (`wl:81-92`); one index `ledger_player_seq_idx (player_id, seq DESC)` (`wl:185-188`). No index on `seq` or `at` alone.
- `wallet.purged_players` with `purged_players_pending_idx` (`wl:94-99,189-193`).
- Dev seed `gold`/Gold/soft and `gems`/Gems/hard, decimals 0 (`wl:101-105`), only when `WALLET_DEV_SEED` is truthy (`wl:111-118`). Every dev fleet sets it: `tools/processctl/src/fleet.rs:904,911` (development split), `:961` (proof split), `:995` (monolith), `:1024` (proof monolith), built by devctl (`tools/devctl/src/supervisor.rs:350`) and splitproof (`tools/splitproof/src/main.rs:710`); also `compose.dev.yaml:27`, `weles/fleet.monolith.toml:33`, `weles/fleet.split.toml:151`. A dev fleet's catalog is `gold` and `gems`.
- Contract (`wa`): `Balance`, `Currency` (`decimals` is a display hint, amounts are minor units, `wa:30-41`), `Movement` (`idempotency_key` required, whole-wallet namespace, `starter:` reserved; identity `(player_id, currency, signed delta, reason)`, `wa:43-75`), caps `MAX_IDEMPOTENCY_KEY_BYTES` 128, `MAX_CURRENCY_CODE_BYTES` 32, `MAX_REASON_BYTES` 256, `MAX_MOVEMENT_AMOUNT` 10^12 (`wa:79-97`). `Wallet`: `balances`, `currencies`, `credit`, `debit` (wire-only, `wa:106-136`).
- Store readers with no non-test caller in `modules/wallet/src` **[grep]**: `pending_purges` (`ws:208`), `list_catalog` + `CatalogEntry` (`ws:74-83,460`), `recent_ledger` + `LedgerEntry`/`LedgerPage`/`MAX_RECENT_LEDGER` (`ws:108-132,495`; no `id`, no cursor), `OnConflict::Overwrite` (`ws:97`; only the seed's `Skip` is used, `wl:208`). Live: `list_balances` (`ws:423`), `list_currencies` (`ws:442`, `ORDER BY code`).
- No `controlplane` dependency (`modules/wallet/Cargo.toml`), no `admin.rs`.

Carriers already in the contract (verified): `Point::EntityTabTile` (`cp:635`) + `Primitive::KpiTiles` (`cp:1458`), tile row `shell:68`; `FormSpec.modes` (`cp:753`, `FormMode` `cp:761`), `FieldKind::Money` (`cp:794`, `tmpl:278`), `FieldKind::RequestId` (`cp:800`, `tmpl:279`), `FieldKind::Ref` (`tmpl:277`); `CellKind::Label`/`MonoMuted`/`Dim` (`cp:1062-1070`, `tmpl:76-79`), `Cell.label` (`cp:1091`), `Column.wrap` (`cp:1104`); `MAX_PANEL_REFS` 100 and `fake_hash` (`cp:208,212`); tab `span.dot.err` when down (`shell:66`). No Na0 is needed unless the owner picks an option marked Na0 below.

---

## 1. Element table: Economy (`economy.html`, `P.economy`)

| Element | Mockup line | Carrier | Status |
|---|---|---|---|
| Nav section "Economy" (order 30, owner wallet) | `reg:11` | `Manifest.sections` (`cp:492`) | ok (15a3) |
| Nav entry "Currencies & ledger", icon `coins`, class `down` | `reg:25,140` | `NavEntry` (`cp:549`) | ok; `down` is hard-coded demo state → accept on every page (§8) |
| Crumbs "Economy › Currencies & ledger", h1 "Currencies & ledger", sub "wallet module" | `pp:166,183` | `PageDecl` (`cp:560`) | ok; text → H9 |
| Layout: `div.grid.g-21` [Currencies │ Grant], Ledger below | `pp:183` | `Layout::Rows([Grid21([cur],[grant]), Stack([ledger])])` (`cp:577-583`); one-panel cells draw bare (13e6, 5a94f07c) | ok |
| **Currencies** panel, title, `flush`, no pager, no toolbar | `pp:168-169` | DataTable, `PageInfo{next_cursor: None, total: None}` | ok; stale demo → **G14** |
| Code `span.tag` | `pp:169` | `CellKind::Ref` | REAL `code` (D4: `gold`, `gems`) |
| Name | `pp:169` | `Text` | REAL `display_name` |
| Kind `span.badge.plain[.accent]` | `pp:169` | `Label` + `tone` (`tmpl:76`) | REAL `kind`; accent rule **G11** |
| Supply `td.num`, Holders `td.num` | `pp:169` | `Number`, `Align::Right` | DERIV aggregates (`ds:172`), **G11**; format **G10** |
| Cap `span.dim` | `pp:169` | `Dim` (`tmpl:79`) | `ds:173` says FAKE; **G11** proposes "—" |
| Create currency tool | none (D6, Q1) | `ActionPoint::PanelTool("currencies")` + `Then::Form` | extra element, **G13** (15a4) |
| **Grant or revoke** panel, no `flush` | `pp:170` | `Primitive::Form` at `ActionPoint::PanelForm("grant")` (`cp:699`) | ok; part **G4** |
| Banner `div.banner.err` "Submissions are disabled…" | `pp:171` | portal error state; `PanelBody.banner` | hard-coded demo, drawn even when forced → accept (§8), **G14** |
| `div.seg > button.on "Grant" + button "Revoke"` | `pp:172` | `FormSpec.modes` (13e4), drawn `tmpl:289/300`; fresh form picks the first mode (`fr:147`) | ok; **G3** |
| Submit "Grant" (swaps to "Revoke"), note "Idempotent per request id." | `pp:172` | `FormMode.submit_label`, `FormSpec.note` | ok; note true only with **G5** |
| Field Player (`type: user`, "ref user") | `pp:173` | `FieldKind::Ref("player")` → `span.input-icon` + user icon (`tmpl:277`) | REAL as uuid; handle picker NEW (`ds:175`) |
| Field Amount (`money`, options EMB/GLD/TOK) | `pp:174` | `FieldKind::Money(codes)` (`tmpl:278`) | REAL; options = catalog codes (D4): `gold`, `gems` on a dev fleet |
| Field Reason, required, help "Shown in the ledger and audit log." | `pp:175` | `String`, `required`, `help` | REAL; help text **G6** |
| Field Request id, mono, read-only | `pp:176` | `FieldKind::RequestId` (13e5) | ok; **G5** |
| Fields `disabled: true` | `pp:173-176` | — | demo state; portal draws them enabled → accept or hand-check (H10) |
| **Ledger** panel, `flush`, toolbar (search "Player or tx id", filters Currency, Reason) | `pp:178-179` | DataTable `search_placeholder`, `FilterKind::Select` | **G2**, **G12** |
| Time `span.mono` (inline 11.5px) "2026-10-22 14:02" | `pp:181` | `Mono` | REAL `at`, formatted in SQL (`to_char(at, 'YYYY-MM-DD HH24:MI')`) |
| Player `a > span.user-cell` | `pp:181`, `bjs:102-106` | `User` + `Link::Entity(player)`, value = player id; label from accounts' `EntityLabel{player}` (Refs seam, `plan:2376-2381`) | ok; needs real ids, **G2** |
| Currency `span.tag` | `pp:181` | `Ref` | REAL |
| Amount `td.num > span.mono`, colour err/ok, "+" sign | `pp:181` | `Mono` + `Tone::Err/Ok` | REAL `delta`; format **G10** |
| Balance `td.num > span.mono` | `pp:181` | `Mono` | REAL `balance_after` |
| Reason `td.wrap > span.dim` | `pp:181` | `Dim`, `Column.wrap` | REAL; format **G6** |
| Tx `span.mono.muted` "tx_8a1f" | `pp:181` | `MonoMuted` (`tmpl:78`) | REAL `id`, full uuid (standing rule); text H12 |
| Foot "7 of 2.4 M transactions", pager "1 / 98,112" | `pp:182` | `PageInfo.total`, `item_noun` | **G12** |

## 2. Element table: players list and player detail

| Element | Mockup line | Carrier | Status |
|---|---|---|---|
| Header "Embers" `th.num` after Lore role | `pc:76` | `EntityListColumn{kind:"player", label, align: Right}` (`cp:639`), order 30 (`plan:1906-1908`) | ok; label **G1**; `th.num > span` as characters G6 |
| Cell (hard-coded `span.muted[title] > svg`) | `pc:92` | `ColumnCells{ref: Cell{Number}}` | DERIV batch balances (`ds:59`), **G17**; mock shape is demo → accept |
| Banner "Embers column unavailable…" + Retry | `pc:106` | failure banner (Step 13 G1 engine) | built; only when wallet fails → accept |
| Fact (hard-coded "wallet unavailable") | `reg:84` | `EntitySummary("player")`, order 50 | ok shape undesigned, **G8** (15a1) |
| Tab "Wallet", third, no count, dot when down | `reg:69` | `EntityTab{player, "wallet", "Wallet", columns: 1}`, order 30; no `EntityTabCount` | ok (15a1) |
| Tab tiles "Embers (EMB)", "Gold (GLD)", "Reroll tokens (TOK)" | `pc:128` | `EntityTabTile{player, wallet}` → `KpiTiles`, row `shell:68` | REAL `list_balances` + catalog, **G9** |
| Transactions panel (error only) | `pc:129` | `EntityTab` DataTable | ok shape undesigned, **G7** |
| Row-menu group "wallet" with note "unavailable"; "Grant currency…" (gift), "Revoke currency…" (coins), disabled | `reg:101-104` | `ActionPoint::RowAction("player")` (`cp:688`), group order 40 | REAL (`ds:66`); **G3**; disabled + note are demo → accept |
| Dashboard tile "Embers spent (24 h)" "1.94 M" (error) | `reg:52` | `DashboardTile` `KpiTile`, order 70 | DERIV (`ds:40`), **G19** |
| Purges pending | none (X1, D6) | `DashboardAttention`, order 62 | REAL `pending_purges`, **G16** |

Shell frame on `economy`: copy `acc/index.txt:5, 6, 7, 9, 10` (Sign out, pending nav entries, Lore, D3, theme toggle). The Economy-section line (`index.txt:8` and its copies) resolves everywhere (§8).

## 3. States per panel, and URLs

| Panel | ok | empty | readonly | stale | error |
|---|---|---|---|---|---|
| Currencies | catalog rows | portal Empty + create tool (`when_empty`, G13); not on a dev fleet (seeded) | n/a (`writes: true`) | mock demo only, **G14** | portal error block |
| Grant or revoke | form | n/a | `writes: false` never | none | portal error (the mock's ok+banner is unreachable, `gd:88`) |
| Ledger | rows, keyset pager | Empty; narrowed to zero keeps the toolbar (golden `table_empty_search`) | — | **G14** | error block |
| Wallet tab tiles | one per currency | no tiles on an empty catalog (not a dev fleet) | — | none | per-slot error |
| Transactions | rows | Empty | — | none | error block (the only state the mock designs) |
| Embers column / fact / tile | values | "0" (always a Cell) | — | none | failure cell + banner / fact error / tile error |
| Purges pending | item while backlog | none at 0 | — | — | per point |

URLs: Ledger search, filters and pager must build from `Ctx.query` (skill §1); the Transactions pager carries `tab=wallet`. Part `a` names its golden case or adds the gap.

## 4. `writes` and every ActionDecl (proposal; ids per X8)

`writes: true`. All `implemented: true` (REAL).

| id | Point | Then | Label / icon | Emphasis | danger | Part |
|---|---|---|---|---|---|---|
| `currency.grant` | `RowAction("player")` | `Form` (Amount, Reason, Request id; player from the row) | "Grant currency…" / `gift` | Default | no | 15a2 |
| `currency.revoke` | `RowAction("player")` | `Form` (same fields) | "Revoke currency…" / `coins` | Default | no | 15a2 |
| `ledger.grant` | `PanelForm("grant")` | `Invoke` (mode "Grant") | — | — | no | per G4 |
| `ledger.revoke` | `PanelForm("grant")` | `Invoke` (mode "Revoke") | — | — | no | per G4 |
| `currency.create` | `PanelTool("currencies")`, `when_empty` | `Form` | "New currency" / `plus` | Default | no | 15a4 |

Grant = `credit`, revoke = `debit`: insufficient funds and an unknown currency map to the service's 409/400 (`wsv:16-36`). Probe and CapCase rows in 15a2/15a4 (Na2).

## 5. Contributions and `order`

| Contribution | Point | order | Part |
|---|---|---|---|
| `wallet` | `EntityTab{player, wallet, "Wallet", 1}`, title "Transactions" | 30 | 15a1 |
| `wallet.tiles` | `EntityTabTile{player, wallet}` | 0 | 15a1 |
| `fact.balance` | `EntitySummary("player")`, icon `coins` | 50 | 15a1 |
| `currencies`, `grant`, `ledger` | `Page("economy")` | 10, 20, 30 | 15a2/15a3 (G4) |
| `column.balance` | `EntityListColumn{player, G1 label, Right}` | 30 | 15a5 |
| `tile.spent` | `DashboardTile` | 70 | 15a5 |
| `attention.purges` | `DashboardAttention` | 62 | 15a5 |

All `min_role: Viewer` except the actions' own roles; `cache_ttl_s` per G14. Wallet declares no entity kind (no `EntityLabel`): the ledger consumes accounts' `player` label. No search provider (O-5).

## 6. Hand-check section (what the §7 `diff` cannot see)

| # | Element | Check |
|---|---|---|
| H1 | Embers/G1 header text, `th.num > span`, right alignment | golden `table_list_columns`; live look |
| H2 | Column cell values (balance of the G1 currency, "0" when none) | live look against `wallet.balances` |
| H3 | Column failure title and banner text | golden `table_list_columns` |
| H4 | Fact icon `coins` and text (G8) | live look |
| H5 | Tab label "Wallet", no count, order after Characters | live look |
| H6 | Tab tile labels and values (G9) | live look; tile ok shape under force |
| H7 | Transactions columns, order, Tx full uuid (G7) | live look at `?tab=wallet` |
| H8 | Row-menu labels, icons `gift`/`coins`, enabled (mock disabled, demo) | live look; modal fields |
| H9 | Economy crumbs, h1, sub, nav label and icon | live look |
| H10 | Grant panel: seg labels, submit swap, note, field labels, help, request-id value, Money options; fields enabled (mock disabled, demo) | golden form-modes and request-id cases (13e4/13e5); live look |
| H11 | Currencies text: kind tone (G11), Cap "—", number format (G10) | live look |
| H12 | Ledger: time format, Amount sign and colour, grouping, Reason wrap, Tx full uuid, no sort arrow, foot text, search placeholder | live look; golden for the coloured Mono cell |
| H13 | Embers-spent tile label and value (G19) | live look |
| H14 | Purges pending item (G16) | golden or live look after deleting a player |
| H15 | Create-currency tool and modal (D6, G13) | live look on an empty catalog (a fleet without `WALLET_DEV_SEED`, or the seed rows deleted) |
| H16 | Demo states declared, not drawn: stale Currencies/Ledger, Grant err banner, nav `down`, error tile, error fact, disabled row group | declared here; cited by the step report |

## 7. Gaps and decisions for part `a`

**G1. Which currency is "Embers"** — DECIDED (owner 2026-11-21): option (b), a typed config value naming the currency, not a hard-coded `gems` (before 15a5, `plan:2146-2148`; lands in 15a5, also 15a1's fact).
- D4 names `gold/gems` the real vocabulary; the catalog is operator data, but a manifest label is static.
- Proposal: a wallet constant naming the hard dev-seed currency `gems` (`wl:102-105`); column "Gems", tile "Gems spent (24 h)", fact "N gems"; a missing catalog row or balance draws "0".
- Alternatives: (b) a typed config value naming the code instead of a hard-coded `gems`; (c) one column per catalog currency (needs a dynamic manifest, Na0).

**G2. The global ledger: FAKE or REAL** — DECIDED (owner 2026-11-21): REAL, unindexed reads with a comment naming the missing index; this overrides plan:2125 "FAKE per R1" (15a3, 15t).
- `plan:2125` says FAKE per R1; `ds:176` says NEW because it "needs an index on seq/at". R1 forbids new indexes (`plan:82-86`), not unindexed reads.
- A FAKE ledger has no real player ids, so its Player cells never resolve a label and the 15t splitproof assertion (label answered by accounts-svc) cannot hold.
- Proposal: REAL, as DERIV under R1: `ORDER BY seq DESC` with a keyset cursor on `seq`, no new index, a comment naming the missing `ledger(seq)` index, and a `docs/todo/wallet.md` item for it. Search by player id uses `ledger_player_seq_idx`; by tx id the PK.
- Alternatives: (b) FAKE amounts on real player ids taken from `wallet.balances` (misleading money); (c) FAKE with fake ids and 15t dropped.
- Cost of REAL: each Economy render runs an unindexed `ORDER BY seq DESC` and an exact `count(*)` over the whole ledger, plus the Supply/Holders aggregates (G11); each dashboard render runs G19's 24 h sum. A panel deadline drops the answer, not the server statement, which keeps running.

**G3. Two row-menu items vs one form with modes** — DECIDED (owner 2026-11-21): two row-menu items, modes only on the Economy panel form (15a2).
- The mock draws two items (`reg:102-103`) and no modal (`gd:46`). Mode actions never draw in a menu (`rt:263,299,508`), and a fresh form opens on its first mode (`fr:147`).
- Proposal: two `Then::Form` row actions without modes (§4); the modes live only on the Economy panel form, the one place the mockup draws `div.seg` (`pp:172`). Both paths share one invoke path.
- Alternatives: (b) both items carry modes, the Revoke item ordered [Revoke, Grant]; (c) Na0: a picked mode on `Then::Form`.

**G4. Which part lands the Grant-or-revoke panel** (15a2 vs 15a3).
- Proposal: 15a2 declares the `economy` page with the form panel and its mode actions; 15a3 adds Currencies, Ledger, the nav section and the `pages.rs` mapping.

**G5. `request_id` → `idempotency_key`** (15a2, X8). Owner (owner 2026-11-21): the Economy form must refuse a player id that names no player (no ghost balances from a typo); this lands in 15a2, not as a Known limit.
- Proposal: key `admin:<request_id>`, refused if over 128 bytes; never under `starter:` (`wa:55-69`).
- The panel form's id is `<contribution id>:<token>` (`core/admin/src/routes.rs:876`), a row-action modal's `<action id>:<token>` (`core/admin/src/routes.rs:1028`), both minted at `core/admin/src/engine/forms.rs:63-66`, the token 43 base64url chars (`core/admin/src/lib.rs:823-828`). The key is 50 bytes plus that id (64 for `admin:currency.grant:<token>`), so it fits while the id is at most `MAX_IDEMPOTENCY_KEY_BYTES` − 50 bytes.
- A resubmit of one instance with a changed field is a 409 (identity includes reason, `wa:48-52`); it shows as a form error.

**G6. Ledger reason text and the Reason help** — DECIDED (owner 2026-11-21): option (a), the reason text only (player-visible), operator only in the audit; help "Shown in the ledger and audit log." (15a2).
- The movement's reason is in `wallet.changed` (`api/wallet/events/src/lib.rs:28`), which audit sinks (`modules/audit/src/lib.rs:56`), and the mockup's audit row shows it (`dj:155`): the help "Shown in the ledger and audit log." (`pp:175`) holds. (The `admin.action` detail carries only the outcome and the portal's own `_reason`, `core/admin/src/engine/actions.rs:479,586,609-611`.)
- notifications shows the reason to the player in a credit's inbox row (`modules/notifications/src/projection.rs:165-169`).
- Options: (a) the reason only, player-visible; the operator stays in audit only (`admin.action`); (b) `admin.grant · <reason> (<operator>)` / `admin.revoke · …` as the mockup shows (`dj:165,169`), which puts the operator's login in the player's inbox. Either is refused over `MAX_REASON_BYTES`.

**G7. Transactions panel ok shape** (15a1).
- Proposal: the Ledger columns minus Player (Time, Currency, Amount, Balance, Reason, Tx), keyset pager on `ledger_player_seq_idx`, rows shared with the global ledger reader (replaces `recent_ledger`, G15). Forced, the mock draws an empty body: accept kind `template choice` with that reason (owner may prefer a declared deviation).

**G8. Fact ok shape** (15a1). Proposal: icon `coins`, "N gems" (G1 currency; "0 gems" at 0). Alternative: "N currencies held".

**G9. Tab tiles** (15a1). Proposal: one tile per catalog currency in `list_currencies` order (`ws:442`; on a dev fleet "Gems (gems)", "Gold (gold)"), label `<display_name> (<code>)`, value the balance ("0" when none), no delta, spark or link.

**G10. Number format** (15a1, 15a3, 15a5). The mock mixes compact ("41.8 M", "1.94 M") and grouped ("91,300"). Proposal: exact en-US grouped integers everywhere, "+" on a positive Amount, minor units as stored (dev decimals are 0). Alternative: compact for Supply and the tile.

**G11. Currencies data** — DECIDED (owner 2026-11-21): Cap shows "—"; Supply/Holders real (15a3). Supply `sum(amount)` and Holders `count(*) WHERE amount > 0` per currency: exact, unindexed (no index leads with `currency`, `wl:78`), with a comment. Cap: wallet has no cap; proposal "—" (truthful constant) instead of `ds:173`'s FAKE. Kind: `Tone::Accent` on `hard`, the real premium kind; none otherwise.

**G12. Ledger toolbar and foot** — DECIDED (owner 2026-11-21) with G2 (real ledger): proposal as written, except the Reason filter (owner, option a): it selects the system reasons (e.g. starter) and admin movements are recognised by the idempotency key prefix `admin:`, since under G6 (a) an admin reason is free text (15a3). Search: exact player id or tx id. Currency filter: `Select` over catalog codes. Reason filter: `Select` over the reasons wallet mints (`starter-grant` `wp:17`, `admin.grant`, `admin.revoke` per G6), prefix match. Foot: exact `count(*)` with a comment, `Noun::new("transaction", "transactions")`. Alternative: no Reason filter (a contract-gap accept line).

**G13. Create currency** (15a4; Q1, D6). `currency.create` (§4) with Code (≤32 B), Name (≤64 B), Kind (string ≤32 B, help "soft or hard"), Decimals (0–18). An existing code is a 409: today's `write_currency_tx` either overwrites or silently skips (`ws:85-104,540-563`). Proposal: replace the dead `OnConflict::Overwrite` with a refusing insert. Its modal has no mockup opener: an `EXCLUDED_MODALS` row (`pages:336`) with reason D6. Under `when_empty` the tool draws only on an empty catalog, so never on a seeded dev fleet.

**G14. Stale and demo states** (15a3). Proposal: `cache_ttl_s: None` everywhere; stale is unreachable and declared (H16). Map `economy` and `player.wallet` with `force: true`. Alternative: `Some(60)` on Currencies only. The cache is read only when the live fetch fails (`core/admin/src/engine/panels.rs:264-308`) and nothing drops an entry after an invoke (`panel_cache` is touched only at `core/admin/src/engine/panels.rs:275,298`), so a ttl shows, during an outage, a stale copy that can predate a grant just made.

**G15. Dead readers** (X4; 15a1, 15a3–15a5). Delete `list_catalog`/`CatalogEntry`; replace `recent_ledger` and its types with the keyset reader (G2/G7); `pending_purges` gets 15a5's caller; `Overwrite` per G13. Adjust their tests in `modules/wallet/src/tests.rs`.

**G16. Purges pending at 62** (15a5, X1). "Wallet purges pending", warn, "N players awaiting sweep · oldest <date>", link to the Events page searched by `PURGE_SWEEP_SUB` (`wp:141`); none at 0. Copy `ca:193-215` (fortress rule).

**G17. Embers column reader** (15a5). One batch read for the refs and the G1 currency, refusing more than `MAX_PANEL_REFS` before SQL (as `ca:51-60,93-100`), ids bound as one `text[]`, "0" for every ref without a row.

**G18. Sequencing (X7).** The purge-race-locks plan also edits wallet's store. DECIDED (owner 2026-11-21): Step 15 first, the purge-race-locks plan after it.

**G19. Embers-spent tile** (15a5). `sum(-delta)` over ledger rows with `delta < 0`, the G1 currency, `at > now() - interval '24 hours'`: exact, unindexed, with a comment; revokes count as spent. No delta, spark or link (the mock designs none).

## 8. Accept-file plan (predicted; the exact text needs a crawl)

- **Every mapped page's file** (`index, players, player, player.characters, events, mail` and the menu files that carry the frame): delete the `mock-only div.nav-section#5694ed` line (Economy section now drawn). Expect instead a pairing line on the nav entry, mock `a.down.nav-item` vs portal `a.nav-item`: kind `data difference`, "the mockup draws wallet-svc down (registry.js:25)".
- **`economy.txt`** (new): the shell-frame copies (§2); Grant `mock-only div.banner.err` → `data difference` (demo, G14); the panel `form` wrapper and hidden inputs → `template choice` (`gd:85`); Ledger search pair → copy `players.txt:11-12`; filter chips vs `Select` → copy `events.txt`'s pair if the same; Currencies rows (two seeded vs the mock's three) and an empty Ledger on a fresh fleet → `data difference` until 24b1; create tool → `declared deviation D6`.
- **`player.wallet.txt`** (new tab entry, `force: true`; drop `player.wallet` from `EXCLUDED_TABS`, `pages:309`): shell copies, header lines as `player.characters.txt:12-19`, Transactions body per G7, tiles per G9 (the two seeded currencies vs the mock's three: `data difference`). The Wallet tab's `span.dot.err` is drawn from `state: "error"` (`reg:69`, `pc:167`), which `force` does not reach: a mock-only line → `data difference` (demo), on every player page's tab bar. Plus `player.wallet.menu.user.txt` if a menu entry is added, as for characters.
- **`players.txt`**: delete `:18` and `:21` (Embers header and cell now drawn); the cell becomes a pairing line (mock `span.muted > svg`, `pc:92`) → `data difference` (demo); `:15` banner → reword to `data difference`: drawn only when wallet fails. The `th.num > span` line (G6 of Step 14) should cover both headers; verify.
- **`player.txt`, `player.characters.txt`**: the wallet fact pairs mock error `span > span > svg` with portal `span > svg` → `data difference` (demo); the Lore fact keeps its line, reason reworded.
- **Menu files** (`players.menu.user`, `player.menu.user`, `player.characters.menu.user`): the `menu-group#198a5b` line resolves; expect pairing lines for the note badge and disabled items → `data difference` (demo). The row-menu modals have no mockup opener: `EXCLUDED_MODALS` rows per menu host.
- **`index.txt`**: delete `:13`, `:16`; the tile pairs mock `div.kpi.state-mini` (error, `bjs:180`) with portal `div.kpi` → `data difference` (demo). No line for Purges pending on a fresh fleet.

## 9. Predicted `docs/todo/` items (part `a` writes them)

- `docs/todo/wallet.md`: the missing `ledger(seq)` and `ledger(at)` indexes (G2, G19) and the balances-by-currency index (G11).
- `admin-second-pass.md`: a FAKE item only if G2 (b/c) or `ds:173`'s FAKE Cap is chosen; no `implemented: false` items.

## Not verified

- No cargo, `admindiff mock`, crawl or diff was run; every accept line in §8 is a prediction.
- How the portal draws a `KpiTile` with an empty `series` (G9, G19).
- Whether the purge-race-locks plan is already being executed (G18).
