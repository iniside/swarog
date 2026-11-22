# Step 19 (storage): checklist

Read-only extraction per `.agents/skills/mockup-implementation/SKILL.md` §1, on the layout of `docs/plans/2026-11-22-step18-groups-checklist.md` (§0-§7, §9, §10, Not verified). HEAD at extraction: `6410b270`. Mockup page: `UILayout/redesign/player.html?tab=saves` (Cloud saves tab). Scope: plan `plan:2328-2331`.

Builder lines storage owns:
- player: `tabRender.saves` `pc:138-141`. Panel for module `storage` at `entity.user.tab`, prim DataTable, title "Cloud saves", meta "3 slots · 41.2 MB of 64 MB", `readonly: true`, `flush` (`pc:138`); `readonlyText` "Read-only: storage exposes saves for inspection and download; edits and deletes are not remotely supported." (`pc:139`); tools "Download all" (`btn sm`, download icon) and "Delete" (`btn sm danger-ghost`, trash icon, `disabled` when readonly) (`pc:140`); table `pager: false`, columns Slot, Size (`num`), Updated, Version, Checksum, "" (`pc:141`). Rows: Slot `span.mono`, Size plain "18.4 MB", Updated plain "4 min ago", Version `span.tag` "v214", Checksum `span.mono.muted` "sha256:9c1f…e02a", last cell `button.btn.sm.ghost` holding only the download icon (`pc:141`).
- Registry: tab `reg:73` (`readonly: true`, **no `count`**); tile `reg:49` (label "Cloud saves used", value "1.82", unit "TB", delta "64 MB quota / player" with no span, spark of 7 points, `sparkColor: var(--chart-2)`, **no `href`**); row-menu group `reg:111` (one item "Download cloud saves", icon `download`, `hint: "read-only"`, run is a toast).
- Panel chrome in the mockup: a readonly panel draws the `ro-chip` (`bj:144`), the info banner with `readonlyText` (`bj:141`), and its tools (`bj:143`).
- The table has no `filters` and no `searchPlaceholder`, so no toolbar; `pager: false`, so no foot (`pc:141`; same reading as the Step 18 checklist's `bj:155,163`). No `rowMenu`, so no `td.act`; the download button is an ordinary last cell.
- Out of scope, with reasons:
  - `reg:62` scheduler attention "storage.gc failed at 03:30": the `portal` provider's fake (`core/admin/src/fakes.rs:182-183`), moving to the scheduler port (`plan:2363-2366`, `plan:3679`).
  - `pc:23` delete-account modal impact row "Cloud saves 3 slots · 41 MB": owned by accounts' `delete-account` action, which is `implemented: false` (`modules/accounts/src/admin.rs:435-444`), so the portal draws no such modal (`tools/admindiff/src/pages.rs:703-707`, `NO_DELETE`). Storage feeds nothing; `ConfirmSpec.impact` is the declaring provider's own (`cp:1024-1034` per the Step 18 checklist), with no cross-module impact point. Revisit only if an operator delete path is ever planned.
  - `dj:103,147,183,193` (services row, subscription row, scheduler rows): other pages.
  - `dj:200` `storage.save_quota_mb`: a row of the mockup's config page fixture, not a key in code (§0.3).

Abbreviations: `pc` = `UILayout/redesign/assets/pages-core.js`, `reg` = `…/registry.js`, `dj` = `…/data.js`, `bj` = `UILayout/design-system/components/bundle.js`, `cp` = `core/controlplane/src/lib.rs`, `ord` = `core/controlplane/src/order.rs`, `tmpl` = `core/admin/src/templates/primitives.html.tmpl`, `shell` = `core/admin/src/templates/shell.html.tmpl`, `pan` = `core/admin/src/engine/panels.rs`, `act` = `core/admin/src/engine/actions.rs`, `at` = `core/admin/src/tests.rs`, `ack` = `tools/admincheck/src/checks.rs`, `ss` = `modules/storage/src/store.rs`, `sl` = `modules/storage/src/lib.rs`, `sa` = `api/storage/api/src/lib.rs`, `svc` = `cmd/storage-svc/src/lib.rs`, `gad` = `modules/groups/src/admin.rs`, `iad` = `modules/inventory/src/admin.rs`, `pages` = `tools/admindiff/src/pages.rs`, `ds` = `docs/research/2026-10-22-1830-admin-redesign-data-sources.md`, `gd` = `docs/research/2026-11-20-2230-admin-ports-contract-gaps.md`, `plan` = `docs/plans/2026-10-22-1759-admin-control-plane-redesign-plan.md`, `todo` = `docs/todo/storage.md`, `s18` = `docs/plans/2026-11-22-step18-groups-checklist.md`.

Method: targeted reads with line numbers; ripgrep sweeps are marked **[grep]** (lower bound). No LSP, no subagent, no cargo, `admindiff mock`, crawl, diff or splitproof. The extraction stopped at the context budget before the accept files and a few wiring files were read; those are under **Not verified**, and §7 names the exact sweep part `b` must run.

## 0. Required checks

**0.1 Readonly is a per-contribution declaration, and the portal already draws all three mockup marks.**
- `ContributionDecl.readonly: Option<String>` "forces the panel's read-only state with this banner text, and an entity tab holding the contribution shows a lock" (`cp:642-644`).
- `is_readonly` is true when the decl sets it, whatever `writes` says (`pan:86-92`), so the tab never draws `ok`: its `ok` shape is `readonly`.
- Drawn: `ro-chip` (`tmpl:443`), banner `banner info` with `p.readonly_text` instead of the generic text (`tmpl:455`), tab lock `<span class="muted" title="read-only">` (`shell:66`). A portal test already pins exactly this case with a storage-like "saves" contribution (`at:2471-2503`).
- `Manifest::write_refusal` refuses every invoking action on a readonly contribution, with its text (`cp:535-548`), and `admit` checks role, then `implemented`, then that refusal (`act:104-115`).
- **Constraint:** admincheck fails an `implemented: true` action that a contribution's `readonly` refuses (`ack:187-198`). So the Delete tool and the download controls on the saves panel must be `implemented: false` (§3, S8).

**0.2 Disabled controls.** A disabled button draws `disabled title="{hint}"` plus `data-refusal` (`tmpl:121,134`, `act:70-80`); the hint for `implemented: false` is "Not implemented yet" (`act:95`). An `icon_only` row action draws only the icon with `aria-label` = label (`tmpl:132-134`); admincheck requires `TableRow` and an icon (`ack:292-299`). This is 13e1 (`plan:3101-3105`, landed 9967d754). A disabled menu item is `div.menu-item.disabled` with `title` (`tmpl:143`).

**0.3 Is there a real quota?** No **[grep]** (`rg "save_quota|quota"` over `modules api core cmd`, `*.rs`/`*.toml`: only gateway push-hub quotas and one core/admin test). The real ceilings are `MAX_OBJECTS_PER_OWNER = 10_000` (`sa:50-51`) and `MAX_VALUE_BYTES = 65_536` caller bytes per value (`sa:43-48`), with the DB backstop `octet_length(value::text) <= 131072` on normalized jsonb (`ss:38`). The mockup's "64 MB" and `dj:200` are fixtures. Consequence: the mockup sizes ("18.4 MB" per slot) are impossible: a real slot is at most ~64-128 KB.

**0.4 Byte formatting helper.** None found in controlplane **[grep]** (`rg "pub fn format_bytes|fn bytes"` in `core/controlplane/src`: no hit). `CellKind::Bytes` exists (`cp:1099`) but renders the module's preformatted value as plain text (SKILL §2; `gd:132`). So storage formats sizes itself (S3).

## What storage holds (verified)

- **Table** `storage.objects(owner_id uuid, collection ≤64B, key ≤128B, value jsonb, version bigint, acl private|public_read, created_at, updated_at)`, PK `(owner_id, collection, key)` (`ss:24-40`); `storage.purged_players` (`ss:41-46`).
- **Indexes:** `storage_list_idx (owner_id, collection, created_at DESC, key DESC)` (`ss:250-254`), `purged_players_pending_idx (player_id) WHERE swept_at IS NULL` (`ss:255-259`). **No index has `updated_at`**, so an owner-scoped `ORDER BY updated_at` is served only for the `owner_id` equality (the PK or `storage_list_idx` prefix) and then top-N sorts all that owner's rows, at most 10,000 (`sa:51`).
- **Version** increments on every CAS write and `updated_at = now()` (`ss:161-173`); a recreated address restarts at 1 (`sa:182-184`).
- **Dead admin helpers waiting for Step 19**, all `#[expect(dead_code)]` citing this step:
  - `pending_purges()` → `(count, oldest created_at::text)` (`ss:464-481`). Ready for the attention item.
  - `admin_counts(owner)` → objects, owners, collections, public (`ss:516-551`), struct `AdminCounts` (`ss:599-612`). **It has no byte sum**, so it cannot serve the tile its `expect` reason names (S6).
  - `admin_recent(owner, limit)` + `admin_recent_sql(scoped)` (`ss:553-597`): newest by `updated_at DESC, owner_id, collection, key`, `LIMIT`, **no cursor**; size is `octet_length(value::text)` computed above the ordered subquery so only the surviving rows are detoasted (`ss:557-560`). Times are `YYYY-MM-DDTHH:MI:SS.USZ` text, not relative. `AdminObjectRow` (`ss:614-630`).
  - Only these definitions reference them **[grep]** (`rg admin_recent|admin_counts|pending_purges|AdminObjectRow` over `modules/storage tools core`).
- **Purge sweep sub:** `projection::PURGE_SWEEP_SUB` (`sl:130-137`).
- **Wiring today:** no `admin.rs` (`modules/storage/src` holds conformance, lib, projection, service, service_tests, store, store_tests), no `PROVIDER_SLOT` contribution in `init` (`sl:81-139`). storage-svc dials no peer (`svc:8-11`); it serves its own edge. Precedent for the provider: groups (`gad`, contributed in `init` as friends does, `s18` "Wiring today").
- **Order constants already exist:** `player_tab::STORAGE = 70` (`ord:57`), `player_row_action::STORAGE = 80` (`ord:90`), `dashboard_tile::STORAGE = 40` (`ord:19`), `dashboard_attention::STORAGE_PURGES = 66` (`ord:38`).
- **No contract change, so no Na0.** Every carrier below exists.

## 1. Element table

### Player detail, Cloud saves tab

| Element | Mockup line | Carrier | Status |
|---|---|---|---|
| Tab "Cloud saves", seventh, lock, no count | `reg:73` | `EntityTab{player, "saves", "Cloud saves", 1, []}` order `player_tab::STORAGE` (70); lock from `readonly` (`shell:66`); **no `EntityTabCount`** | ok |
| Panel title "Cloud saves", `flush` | `pc:138` | `ContributionDecl.title`; flush for a table (SKILL §3.7) | ok |
| Meta "3 slots · 41.2 MB of 64 MB" | `pc:138` | `PanelBody.meta` | DERIV count + byte sum; quota **S7** |
| `ro-chip` "read-only" | `pc:138`, `bj:144` | `ContributionDecl.readonly` → `tmpl:443` | ok |
| Info banner, `readonlyText` | `pc:139`, `bj:141` | `ContributionDecl.readonly = Some(<pc:139 text verbatim>)` → `tmpl:455` | ok |
| Tool "Download all" `btn sm` + download icon | `pc:140` | `PanelTool("saves")`, `Emphasis::Default`, `implemented: false` | O-3; mockup draws it enabled, portal disabled (H5) |
| Tool "Delete" `btn sm danger-ghost` disabled + trash | `pc:140` | `PanelTool("saves")`, `danger: true`, `implemented: false` | **S8** |
| No toolbar | `pc:141` | `filters: []`, `search_placeholder: None` | ok |
| Slot `span.mono` | `pc:141` | `CellKind::Mono` | REAL; label **S2** |
| Size `td.num`, plain "18.4 MB" | `pc:141` | `Column{align: Right}` + `CellKind::Bytes` (plain) | DERIV `octet_length(value::text)`; format **S3** |
| Updated plain "4 min ago" | `pc:141` | `CellKind::Text` (plain, not `span.dim`) | REAL `updated_at`, relative in SQL **S4** |
| Version `span.tag` "v214" | `pc:141` | `CellKind::Ref`, value `v{version}` | REAL (`ss:31`) |
| Checksum `span.mono.muted` "sha256:9c1f…e02a" | `pc:141` | `CellKind::MonoMuted` (`cp:1107`) | **S5** (ds says FAKE; SQL can derive it) |
| Last column: icon-only `btn sm ghost` download | `pc:141` | `TableRow("saves")`, `icon_only: true`, icon `download`, `Emphasis::Ghost`, `implemented: false`; `Row.item` = the slot address, `Row.actions` = [it] | 13e1; header label "" vs the portal's action column header (H8) |
| No foot or pager | `pc:141` (`pager: false`) | `PageInfo.total: None`; foot only when `next_cursor` is set (`s18` G3, `tbl:267`) | **S1** |
| Rows not clickable | `pc:141` | `Row.entity = None` | ok |

### Player row menu (every `.menu.user` host)

| Element | Mockup line | Carrier | Status |
|---|---|---|---|
| Group "storage", item "Download cloud saves", download icon, `span.hint` "read-only", enabled | `reg:111` | `RowAction("player")`, order `player_row_action::STORAGE` (80), `implemented: false` → `div.menu-item.disabled title="Not implemented yet"` (`tmpl:143`) | O-3 + St2 (`gd:128-129`): enabled vs disabled and the hint are a declared difference (H6, accept §7) |

### Dashboard

| Element | Mockup line | Carrier | Status |
|---|---|---|---|
| Tile "Cloud saves used" | `reg:49` | `Point::DashboardTile` order `dashboard_tile::STORAGE` (40), `KpiTile.label` | ok |
| Value "1.82" + unit "TB" | `reg:49` | `KpiTile.value` + `KpiTile.unit` (`cp:1257-1267`) | DERIV `sum(octet_length(value::text))` (`ds:37`); **S6** |
| Delta "64 MB quota / player" (plain text) | `reg:49` | `KpiTile.delta: Delta{value, period, tone: None}` (`cp:1248-1254`) | quota FAKE in `ds:37`; **S7** |
| Spark, `--chart-2` | `reg:49` | `KpiTile.series` + `Palette::Chart2` | FAKE (`ds:37`, no metric store); **S6** |
| No link | `reg:49` | `KpiTile.link: None` | ok |
| Purges pending (no mockup line) | none (X1) | `DashboardAttention` order 66 → `controlplane::purge_backlog_body("Storage purges pending", pending, oldest, PURGE_SWEEP_SUB.id)` (`cp:390-414`, as `gad:331-334`) | REAL `pending_purges` |

Shell frame on `player.saves` and `player.saves.menu.user`: copy the frame lines (Sign out, pending nav entries, Lore ×3, D3, theme toggle) and the entity-head lines from `tools/admindiff/accept/player.groups.txt` / `player.groups.menu.user.txt` (read and line-numbered by part `b`; not reached here). Re-derive the tab-bar lines: the Cloud saves tab is now drawn and selected.

## 2. States per panel, and URLs

| Panel | ok | empty | readonly | stale | error |
|---|---|---|---|---|---|
| Cloud saves tab | never: the decl forces `readonly` (`pan:87`) | owner holds no object: portal Empty "Nothing here yet" (`tmpl:390`); which of `empty`/`readonly` wins is not verified (H9) | rows + chip + banner + both tools (disabled) | `cache_ttl_s: None`: n/a | error block (`tmpl:465`) |
| Tile | value + unit + delta + spark | sum 0 → value "0" unit "B" (a count is a value, never absent) | n/a | only if S6 picks a `cache_ttl_s` (then "Showing cached data…", `tmpl:454`, tile variant not read) | tile error |
| Purges pending | warn item while backlog | none at 0 (`cp:397-399`) | n/a | n/a | per point |

Narrowed to zero: n/a, no search or filter. The tab declares no `params`. URLs: none but the pager's next link, and only if S1 (a) keeps a cursor; it builds from `Ctx.query` like every `DataTable` (verify in part `a`). Disabled controls build no URL.

## 3. `writes` and every ActionDecl (proposal)

`writes: false`. Helper as `iad:282-301` (`not_implemented`, `Then::Invoke`, `min_role: Operator`).

| id | Point | Label / icon | order | Emphasis | danger | icon_only | implemented | then |
|---|---|---|---|---|---|---|---|---|
| `saves.download-all` | `PanelTool("saves")` | "Download all" / `download` | 0 | Default | no | no | **false** (O-3) | Invoke |
| `saves.delete` | `PanelTool("saves")` | "Delete" / `trash` | 1 | Default | **yes** | no | **false** (S8) | Invoke |
| `save.download` | `TableRow("saves")` | "Download" / `download` | 0 | Ghost | no | **yes** | **false** (O-3) | Invoke |
| `player.download-saves` | `RowAction("player")` | "Download cloud saves" / `download` | `player_row_action::STORAGE` (80) | Default | no | no | **false** (O-3) | Invoke |

Ids are proposals (≤64 bytes, `ack:6-8`). Every `implemented: false` gets an item in `docs/todo/admin-second-pass.md` in the 19a commit (SKILL "Standing decisions"); the three downloads also stay under the existing `todo` item "Admin download carrier for cloud saves" (`todo:9-14`). No modal anywhere: `Then::Invoke` on a disabled control opens nothing, so no `EXCLUDED_MODALS` row.

## 4. Contributions and `order`

| Contribution id | Point | order | min_role | readonly | cache_ttl_s |
|---|---|---|---|---|---|
| `saves` | `EntityTab{player, saves, "Cloud saves", 1, []}` | 70 | Viewer | `Some(pc:139 text)` | None |
| `tile.used` | `DashboardTile` | 40 | Viewer | None | S6 |
| `attention.purges` | `DashboardAttention` | 66 | Viewer | None | None |

No entity kind, no `EntityLabel`, no search. Wiring as groups: a `controlplane` dependency, `admin.rs`, `PROVIDER_SLOT` in `Storage::init`; in the split admin-svc needs a `storage` peer (S10).

## 5. Hand-check section (what the §7 `diff` cannot see)

| # | Element | Check |
|---|---|---|
| H1 | Tab label "Cloud saves", lock, no count, order after Groups | live look; `at:2471-2503` already pins the lock mechanism |
| H2 | Meta text per S7 (`N slot`/`slots` singular at 1) | unit test of the meta builder + live look |
| H3 | Banner text equals `pc:139` verbatim | unit test on the manifest |
| H4 | Slot text per S2, in full | live look |
| H5 | "Download all" drawn disabled `title="Not implemented yet"` where the mockup's is enabled (O-3) | live look; `admindiff` never compares attributes |
| H6 | Row-menu item disabled with a title, where the mockup's is enabled with `span.hint` "read-only" (St2) | live look of `player.saves.menu.user` |
| H7 | Size, Updated, Version, Checksum values per S3-S5 (real sizes are KB, not MB; checksum in full, not cut) | unit tests of the formatters + live look |
| H8 | The download column's header: mockup `th` with "" label; portal action column header | crawl diff shows tag shape; text by look |
| H9 | Empty owner: which state draws (empty vs readonly), tools present or not | golden or a unit test in part `a` |
| H10 | No sort arrow on any column (mockup draws none) | live look |
| H11 | Tile value/unit/delta text, spark colour Chart2, no link | live look |
| H12 | Purges pending item (66) after deleting a player with saves | golden or live look |

## 6. Gaps and decisions for the owner

**S1. Tab reader, order and paging.** `admin_recent` has no cursor and sorts by `updated_at` with no index for it (`ss:553-597`, `ss:250-254`); the mockup has `pager: false`; an owner can hold 10,000 objects (`sa:51`).
- (a) **Recommended:** an owner-scoped keyset reader built on `admin_recent_sql`'s nested shape: `ORDER BY updated_at DESC, collection, key`, cursor `(updated_at, collection, key)`, page 25, `PageInfo.total: None`. One page draws no foot, matching `pc:141`; more than 25 draws the portal's pager. The comment names the missing `(owner_id, updated_at)` index (top-N over ≤10,000 rows per owner). Drop the unscoped branch, which nothing will call.
- (b) Literal `pager: false`: `LIMIT 10_000`, one panel of up to 10,000 rows. Not recommended.
- (c) Order by `created_at` to use `storage_list_idx`: but the visible column is Updated, and the index leads with `collection`, so it serves one collection only.

**S2. Slot label.** The address is `(collection, key)` (`ss:35`); the mockup shows one word.
- (a) **Recommended:** `collection/key` in one `Mono` cell, in full. `/` cannot occur in either segment (`sa:81-86`), so the label is unambiguous. It is also `Row.item` for the row download.
- (b) Key only: two collections can share a key, so it is ambiguous.
- (c) Two columns: invents an element.

**S3. Size format.** No controlplane helper (§0.4).
- (a) **Recommended:** a storage-side formatter, decimal units with one decimal place as the mockup has ("18.4 MB"): `B`, `KB`, `MB`, `GB`, `TB`. One function serves the Size cells, the meta and the tile's value/unit split. Its doc-comment says the size is normalized jsonb bytes (`ss:15-19`), not the client's bytes.
- (b) `pg_size_pretty` in SQL: binary units and "kB", which do not match the mockup.

**S4. Updated as a relative time.** The rule is SQL, not a Rust clock. **Recommended:** a SQL `CASE` over `now() - updated_at` that yields the mockup's words ("N s ago", "N min ago", "N h ago", "N d ago"), as `CellKind::Text`. Whether another port already has such an expression was not checked; part `a` reuses one if it exists. Alternative: the absolute RFC3339 text the reader already returns (`ss:591`), which breaks the mockup's format.

**S5. Checksum.** `ds:90` says FAKE, but Postgres can derive it: `sha256(convert_to(value::text, 'UTF8'))`, computed above the ordered subquery like the size, so only the page's rows are detoasted.
- (a) **Recommended:** DERIV `sha256:<64 hex>` in full (identifiers are never cut; the mockup's `…` cut is a hand-check, H7), with the comment that it hashes the normalized jsonb text, so it is not the client's bytes.
- (b) FAKE through `controlplane::fake_hash`: 64 bits cannot be a SHA-256, so the prefix would lie.
- (c) Drop the column: not allowed.

**S6. Tile reader, value, spark and cost.** `admin_counts` has no byte sum (`ss:527-551`). A full-table `sum(octet_length(value::text))` detoasts every value on every dashboard load.
- **Recommended:** replace `admin_counts`/`AdminCounts` with one `admin_used_bytes()` (an exact sum whose comment names its cost), and delete the dead fields. Set `cache_ttl_s: Some(300)` on `tile.used`, which makes the tile's stale state reachable. Value and unit come from the S3 formatter. The spark is a `// FAKE` series from `controlplane::fake_hash("storage.tile.spark", "dashboard")` (no metric store, `ds:37`), with `Palette::Chart2`, `link: None`, and a second-pass item.
- Alternatives: no cache (exact on every load); or an empty `series` (drops the spark element, not allowed).

**S7. Quota: the meta and the tile delta.** No quota exists (§0.3). The truthful constant is the object cap.
- (a) **Recommended:** meta `"{N} of 10,000 slots · {size}"` and delta `"10,000 slots / player"` (`Delta{value: "", period: …, tone: None}`, or the other way round, whichever draws a plain span; the wallet and mail tile providers were not read). Both come from `MAX_OBJECTS_PER_OWNER`.
- (b) A byte ceiling `10,000 × 64 KiB`: it mixes caller bytes with normalized bytes (`sa:43-48`, `ss:15-19`), so it would state a limit the data never had.
- (c) FAKE "64 MB": a FAKE where a truthful constant exists breaks the rule.

**S8. The Delete tool.** The mockup draws it disabled because the panel is read-only (`pc:140`). admincheck forbids an `implemented: true` action on a readonly contribution (`ack:187-198`).
- (a) **Recommended:** `implemented: false`, `danger: true`. It draws disabled with the title "Not implemented yet" (H5), and gets a second-pass item worded "by design: no operator delete".
- (b) Declare no readonly on the contribution, set `writes: false` and `implemented: true`: the hint becomes the generic read-only text (`act:97`), but the `pc:139` banner text is lost and the decl claims an invoke the provider has none of.
- (c) Omit Delete: drops an element.

**S9. Dead helpers.** Under S1 (a) and S6: `admin_recent` becomes the keyset reader (owner-scoped only), `AdminObjectRow` keeps `collection`, `key`, `version`, `bytes`, `updated_at` plus the S4 text and the S5 hash, and drops `owner_id` and `acl` unless shown. `admin_counts`/`AdminCounts` are deleted. `pending_purges` is used as is. **Recommended** as stated. Showing `acl` would invent a column.

**S10. The split.** admin-svc must dial storage-svc for the provider, as it does the `groups` peer (`s18`: `cmd/admin-svc/src/main.rs:42`, `lib.rs:36`). Whether a `storage` peer already exists there was not read. **Recommended:** 19a2 adds it if missing; storage-svc's own wiring is unchanged (it serves, `svc:8-11`).

**S11. Delete-account impact row `pc:23`.** **Recommended:** out of scope (see the header); no storage work, no accept entry, since the portal draws no delete-account modal.

## 7. Accept-file plan (predicted; exact text needs a crawl)

- `pages`:
  - Remove `("player.saves", …Step 19)` from `EXCLUDED_TABS` (`pages:392`).
  - Add two `PAGE_MAP` entries `player`/`tab: Some("saves")`, without and with `USER_MENU`, copied from the groups pair (`pages:338-355`).
  - `EXCLUDED_MOCK_MODALS`: copy the six groups rows to `player.saves` and `player.saves.menu.user` (`pages:687-692`).
  - `EXCLUDED_MODALS`: copy the wallet grant/revoke rows to host `player.saves.menu.user` (`pages:490-499` pattern).
  - No storage modal row (§3). No `EXCLUDED_MENUS` row: storage declares no row kind.
- **`player.saves.txt`** (new): shell-frame and entity-head lines copied from `player.groups.txt`, with reasons adjusted; tab-bar lines re-derived. Predicted entries: the rows (mockup 3 vs the crawled player's, likely 0 on a demoseeded fleet → `data difference`, Seed rule; or the empty block if demoseed writes no saves); the Download-all/Delete `disabled` differences (attribute-only, so likely no line; H5); the icon-only download header cell (H8, `template choice` if it shows).
- **`player.saves.menu.user.txt`** (new): copy `player.groups.menu.user.txt`.
- **Every `.menu.user` file** (`players`, `player`, `player.characters`, `player.wallet`, `player.inventory`, `player.friends`, `player.groups`, + the new one): the storage group now draws; delete each `not ported yet … (Step 19)` line for it; St2's enabled-vs-disabled difference is attribute/class level (`menu-item disabled`), and if the crawl shows it, it is `template choice` (O-3 keeps it disabled).
- **`player*.txt`**: in `player.txt`, `player.characters.txt`, `player.wallet.txt`, `player.inventory.txt`, `player.friends.txt`, `player.groups.txt`, delete the Cloud saves tab-button line (`button.tab#f729f6`, `not ported yet … Step 19`) and add none: the tab is now drawn.
- **`index.txt`**: delete the "Cloud saves used" tile's `not ported yet` line(s); a tile value difference (mockup "1.82 TB" vs fleet sum) is text-only, so no line; no Purges pending line on a fresh fleet.
- **Sweep part `b` must run first:** `rg -n -i "saves|f729f6|step 19|storage|cloud" tools/admindiff/accept`. This extraction did not reach it.

## 9. Splitproof (19t, on the groups/friends admin-checks pattern)

File `tools/splitproof/src/storage_admin_checks.rs`, called from both topologies' passes in `tools/splitproof/src/main.rs` (as `s18` §9). Each check has a recorded break run with admin-svc missing its `storage` peer.
- **SA1:** player B `put`s `saves/slot-1` through the public API (`sa:162-165`). B's entity page draws the Cloud saves tab with the lock. `/admin/_panel/storage/saves?entity=player:B` is `data-state="readonly"` with the `pc:139` banner and one row: `saves/slot-1`, `v1`, and a non-empty size. In the split the panel is built in storage-svc: that is the at-risk path.
- **SA2:** the three download controls and Delete draw disabled with `data-refusal="not-implemented"`. `POST /admin/_action/storage/save.download` is refused, and no provider call is made.
- **SA3:** the dashboard draws the "Cloud saves used" tile with a value.
- **SA4** (optional): after deleting B, "Storage purges pending" appears until the sweep runs. Prefer a unit test; it races the sweep.

## 10. Part sizes (estimate; small tasks)

| Part | Content | Files | Tool calls |
|---|---|---|---|
| 19a1 | `store.rs`: keyset reader (S1), size/relative-time/sha256 columns (S3-S5), `admin_used_bytes` (S6), deletions (S9); store tests | 2-3 | ~30-40 |
| 19a2 | `admin.rs` (manifest, tab, tile, purges, formatter, FAKE spark), `lib.rs` `PROVIDER_SLOT`, `Cargo.toml`, admin-svc peer (S10), admin unit tests, `admin-second-pass.md` items, `todo` | 6-8 | ~45-55 |
| 19a3 | `pages.rs` (§7) | 1 | ~15 |
| 19t | `storage_admin_checks.rs` + call sites | 2-3 | ~35-45 |
| 19b | crawls, accept files of §7 | 9-12 | ~25-35 |

## Not verified

- I ran no cargo, `admindiff mock`, crawl, diff, splitproof or admincheck. Every §7 line is a prediction.
- The accept files themselves: `player.groups*.txt` frame lines, the `button.tab#f729f6` lines and the Step 19 lines in the menu files and `index.txt` (the budget stopped before the sweep §7 names).
- How a `KpiTile.delta` with an empty `value` draws, and the wallet/mail tile providers (S7).
- Whether `empty` or `readonly` wins for an owner with no objects (`pan:308` region not read; H9).
- Whether the disabled `menu_item` draws `span.hint` (`plan:3104` says it does; `tmpl:143` as read shows only a `title`).
- `cmd/admin-svc` peers (S10), `modules/groups/src/lib.rs` `PROVIDER_SLOT` line, and `modules/storage/Cargo.toml`.
- Whether a SQL relative-time expression already exists in another provider (S4).
- Whether demoseed writes any cloud save for the crawled player.
- The `DataTable` action-column header markup for a `TableRow` (`tmpl:232,238` per `s18`, not re-read).
- `docs/todo/admin-second-pass.md` current items (St2/St3 may already be listed).

## 11. Owner decisions (2026-11-22)

Owner, 2026-11-22: "ok zrob z rekomendacjami". Every §6 item is decided as recommended:
S1 (a), S2 (a), S3 (a), S4 (relative time in SQL), S5 (a), S6 (recommendation), S7 (a),
S8 (a), S9 (recommendation), S11 (out of scope).

Correction to S10: admin-svc already dials storage (`cmd/admin-svc/src/lib.rs:38`,
`peer_face("storage", wiring, "127.0.0.1:9016")`; `cmd/admin-svc/src/main.rs:43`), so 19a2
adds no peer.
