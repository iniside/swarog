# Step 18 (groups): checklist

Read-only extraction per `.agents/skills/mockup-implementation/SKILL.md` §1 (at 8639b724). I wrote it from the Step 17 template; I did not re-read the skill's own sections before the context budget ran out. Mockup page: `UILayout/redesign/player.html?tab=groups` (Groups tab). Builder lines groups owns:
- player: `tabRender.groups` `pc:136-137`. A panel for module `groups` at point `entity.user.tab`, title "Groups", meta "2", `flush` (`pc:136`). Columns: Group, Role, Members (`num: true`), Joined. Two rows. Group is `<a href="#">name</a>`, Role is `badge(r[1], "", true)` (plain), then Members and Joined. `pager: false`, no `rowMenu` (`pc:137`).
- Registry: tab `reg:72` (`count: () => 2`). No groups row-menu group, header action, fact or attention item **[grep]** (`rg -i group UILayout/redesign/assets`: only `reg:72`, `pc:136,137` and `dj:99`; the other hits are the generic word "grouped" in `catalog.js` and nav `groups` in `reg:136,143`).
- Purges pending: no mockup line (X1, `plan:1915-1917`). Order 65 (`plan:1910-1912`).
- **No group entity page in the mockup** **[grep]** (no `group` page, tab or header in `UILayout/redesign/*.html` or the assets). O-4 (`plan:3028`) asks for one anyway (G7).
- Out of scope: `dj:99` (the groups-svc services row, on another page).
- The table has no `filters`, so the mockup draws no toolbar (`bj:155`). It has `pager: false`, so it draws no foot and no pager (`bj:163`). No `rowMenu`, so no `th.act`/`td.act` (`bj:157,161`).

Abbreviations: `pc` = `UILayout/redesign/assets/pages-core.js`, `reg` = `…/registry.js`, `dj` = `…/data.js`, `bj` = `UILayout/design-system/components/bundle.js`, `cp` = `core/controlplane/src/lib.rs`, `tmpl` = `core/admin/src/templates/primitives.html.tmpl`, `shell` = `core/admin/src/templates/shell.html.tmpl`, `tbl` = `core/admin/src/render/table.rs`, `cr` = `core/admin/src/render/cell.rs`, `url` = `core/admin/src/render/url.rs`, `lb` = `core/admin/src/engine/labels.rs`, `act` = `core/admin/src/engine/actions.rs`, `rt` = `core/admin/src/routes.rs`, `evp` = `core/admin/src/events_provider.rs`, `ack` = `tools/admincheck/src/checks.rs`, `gl` = `modules/groups/src/lib.rs`, `gs` = `modules/groups/src/store.rs`, `gsv` = `modules/groups/src/service.rs`, `gp` = `modules/groups/src/projection.rs`, `gc` = `modules/groups/src/conformance.rs`, `ga` = `api/groups/api/src/lib.rs`, `ge` = `api/groups/events/src/lib.rs`, `old` = `b8435c9a^:modules/groups/src/admin.rs`, `oldc` = `b8435c9a^:modules/groups/src/conformance.rs`, `oldp` = `b8435c9a^:tools/conformance/src/policy.rs`, `fad` = `modules/friends/src/admin.rs` (ad923bfd), `aa` = `modules/accounts/src/admin.rs`, `mc` = `modules/mail/src/conformance.rs`, `mad` = `modules/mail/src/admin.rs`, `wad` = `modules/wallet/src/admin.rs`, `pol` = `tools/conformance/src/policy.rs`, `chk` = `tools/conformance/src/checks.rs`, `spm` = `tools/splitproof/src/main.rs`, `sfa` = `tools/splitproof/src/friends_admin_checks.rs` (uncommitted), `er` = `tools/splitproof/expected-red.txt`, `pages` = `tools/admindiff/src/pages.rs`, `gd` = `docs/research/2026-11-20-2230-admin-ports-contract-gaps.md`, `gk` = `docs/research/2026-11-17-ports-contract-gaps.md`, `ds` = `docs/research/2026-10-22-1830-admin-redesign-data-sources.md`, `plan` = `docs/plans/2026-10-22-1759-admin-control-plane-redesign-plan.md`, `acc` = `tools/admindiff/accept`, `s17` = `docs/plans/2026-11-22-step17-friends-checklist.md`, `todo` = `docs/todo/groups.md`.

Method: targeted reads with line numbers. **[grep]** marks a ripgrep sweep, which is a lower bound. I ran no cargo, `admindiff mock`, crawl, diff or splitproof, and used no subagent. The read stopped at the context budget; what it did not reach is under **Not verified**.

## 0. Required checks

**0.1 Is a non-player entity page possible without a contract change? Yes, and a precedent exists.**
- The route `GET /admin/e/{kind}/{id}` (`rt:46,529-639`) needs only that some manifest declares the kind (`snapshot.kind_owner`, `rt:542-550`; an undeclared kind draws "Unknown entity kind" 404, `rt:543-548`). Headers, summaries, tabs and the row menu are all optional (`rt:553-613`).
- With no `EntityHeader` the page draws `<h1>{id}</h1><span class="muted">{kind_label}</span>` (`shell:63`). The avatar is seeded from the id (`rt:625-626`). The crumb is the owner's nav entry if it has one (groups has none), then the raw id (`rt:614-620`).
- **Precedent:** core.events declares kind `Subscription` (`evp:282-286`) with `EntityHeader` (`evp:297`), a `Details` `EntityTab` (`evp:298-310`) and a header body (`evp:591-602`). It is the only non-player kind with an entity page **[grep]** (`EntityHeader` in modules: only `aa:390`; in core/admin: `evp:297`). characters declares a kind with an `EntityLabel` and no header (`modules/characters/src/admin.rs:273-281`, not re-read).
- **A trap:** `EntityHeader(group)` makes every row whose `Row.entity` is a group clickable (`tbl:220`). The mockup's rows are not clickable (`pc:137`, no `href` on the row), so the tab's rows must not carry `Row.entity = group` (G2).
- admincheck: every point and action kind must be declared somewhere (`ack:93-116`). One owner per kind, with at most one `EntityLabel` (`ack:118-140`).

**0.2 How does a group-name cell link?** `Cell{kind: Text, value: name, link: Link::Entity(group:id)}` draws `<a href>name</a>` (`tmpl:91`, `cr:91`), which matches the mockup's `<a href="#">` (`pc:137`). `drawn_ref` fires only when `value == id` (`lb:16-22`), so a name value triggers no label pass. Groups needs no `EntityLabel`.

## What groups holds (verified)

- **Tables** (`gl:55-84`):
  - `groups.groups(id, name ≤64B, join_policy open|request|invite, creator_id, created_at)`.
  - `groups.memberships(group_id, player_id, state member|invited|requested, role ''|admin|member, created_at)`, PK `(group_id, player_id)`. Role is non-empty iff state is member (`gl:74-76`).
  - `groups.purged_players` (`gl:78-84`).
  - Indexes: `memberships_player_idx (player_id, state, created_at DESC, group_id DESC)`, `memberships_group_idx (group_id, state, created_at DESC, player_id DESC)`, partial `memberships_pending_idx`, and `purged_players_claim_idx` (`gl:152-172`).
- **Vocabulary** (`ga:45-64`): `STATE_MEMBER/INVITED/REQUESTED`, `ROLE_ADMIN="admin"`, `ROLE_MEMBER="member"`, `JOIN_*`. `MAX_MEMBERS=500` per group (`ga:42`). No per-player cap on groups **[grep]** (`pub const` in `ga`).
- **`created_at` is set once on insert and is not updated by `promote_tx`** (`gs:420-441`, which updates only `state` and `role`). So for a member who came in by invite or request, `created_at` is when the request or invite was made, not when they joined (G5).
- **DERIV reader `page_mine`** (`gs:498-534`, SQL `gs:661-671`): every row the player holds, any state, joined to `groups.groups`. It returns `(group_id, name, join_policy, group created_at, my_state, my_role, membership created_at)` (`gs:21-29`). Keyset `(m.created_at, m.group_id) DESC`. It has **no member count**. The leading `state` in the index breaks the ordered scan (todo GROUPS-05, `todo:29-34`).
- **Cursor helpers:** `encode_cursor`/`decode_cursor` are base64 of `created_at|id`, with the cap checked first (`gsv:102-135`). `is_cursor_time` (`gsv:67`). `created_text` renders `YYYY-MM-DDTHH:MI:SS.USZ` in SQL (`gs:14-16`).
- **Counts:**
  - `counts_tx(group)` (`gs:316-339`): rows, members and admins of one group, under a transaction.
  - `COUNTS_LATERAL` (`gs:710-712`): members and pending per `g.id`, correlated.
- **Dead admin readers (X4)** **[grep]**: `rg` over `modules/groups/src` for `admin_totals|admin_recent_groups|admin_group|AdminGroupRow|ALL_ROWS|COUNTS_LATERAL|row_to_admin_group` hits only their definitions and doc-comments.
  - `admin_totals` (`gs:584-593`).
  - `admin_recent_groups` (`gs:597-614`, todo GROUPS-09 `todo:52-56`).
  - `admin_group` (`gs:618-639`): one group plus members and pending, folding a non-uuid into `None`.
  - `AdminGroupRow` (`gs:50-57`), `row_to_admin_group` (`gs:714-724`), `COUNTS_LATERAL` (`gs:710-712`), `ALL_ROWS` (`gs:654-656`).
  - The Step 1 commit message says these stay for the ports (`git show b8435c9a`).
- **Purge:** `pending_purges()` (`gs:119-127`), sweep sub `PURGE_SWEEP_SUB` (`gp:211`, wired at `gl:200-207`).
- **Wiring today:**
  - No `admin.rs`, no `controlplane` dependency (`modules/groups/Cargo.toml`), no `PROVIDER_SLOT` contribution (`gl:178-244`).
  - **Both topologies:** friends contributes its provider in `init` (`git show ad923bfd -- modules/friends/src/lib.rs`: `ctx.contribute(controlplane::PROVIDER_SLOT, …)`). admin-svc already has a `groups` peer at `:9015` (`cmd/admin-svc/src/main.rs:42`, `cmd/admin-svc/src/lib.rs:36`).
  - The tab reads only the `groups` schema. No accounts hop: the mockup draws no user cell.
- **Event `group.role_changed` v1** (`ge:103-109,126-127`): `{group_id, player_id, role, actor_kind, actor_id}`. `ACTOR_OPERATOR` has an empty `actor_id`, and the operator is named by the portal's `admin.action` row (`ge:39-42`).
  - **No producer at HEAD** **[grep]**: no `ROLE_CHANGED` in `modules/groups/src`. The only producer was the deleted `old:515-528`.
  - It is still a registered contract (`tools/topiccheck/src/main.rs:211`, `docs/reference/contract-golden/contracts.txt`).
  - No consumer was found: `modules/chat` does not exist; other modules not swept.

Carriers already in the contract (verified):
- Entity points and actions: `EntityKindDecl` (`cp:625-629`), `Point::EntityTab`/`EntityTabCount`/`EntityHeader`/`DashboardAttention` (`cp:649-674`), `ActionPoint::TableRow` (`cp:729-732`), `Then::{Invoke, Confirm}` (`cp:753-758`), `ConfirmSpec` (`cp:1024-1034`).
- Rows and links: `Row.item`/`Row.actions` (`cp:1159-1167`), `Link::Entity` (`cp:372-375`).
- Invoke: `InvokeCall` with `entity`, `item`, `operator`, `request_id` (`cp:467-482`), `InvokeOutcome::{Ok, NoOp, FieldErrors}` (`cp:501-507`), `MAX_INPUT_VALUE_BYTES = 4096` (`cp:204`).
- Tables and cells: `PageInfo` (`cp:1170-1174`), `DataTable` (`cp:1198-1212`), `CellKind::Label` → `span.badge.plain` (`tmpl:76`).
- `purge_backlog_body` (used as in `fad:197-199`).
- A precedent for `TableRow`: accounts' unlink/revoke rows use `entity: None`, `item: Some(id)`, `actions: vec![id]` (`aa:655-682`), declared `not_implemented(…, table_row(…))` (`aa:424`). **No `implemented: true` `TableRow` invoke exists yet** **[grep]** (only `aa:355` among the provider `admin.rs` files; most modules have no `admin.rs`).
- **No contract change, so no Na0.**

---

## 1. Element table: player detail, Groups tab

| Element | Mockup line | Carrier | Status |
|---|---|---|---|
| Tab "Groups", sixth, count 2 | `reg:72` | `EntityTab{player, "groups", "Groups", 1, []}` order 60 (`plan:1905-1906`) + `EntityTabCount{player, groups}` | ok; count meaning **G3** |
| Panel title "Groups", `flush` | `pc:136` | `ContributionDecl.title` | ok |
| Meta "2" | `pc:136` | `PanelBody.meta` | DERIV; **G3** |
| No toolbar | `pc:137`, `bj:155` | `filters: []`, no `search_placeholder` (`tbl:262`) | ok |
| Group: `a[href]` name | `pc:137` | `CellKind::Text`, value = name, `link: Link::Entity{group, id}` (§0.2) | REAL; target **G7** (O-4) |
| Role: `span.badge.plain` "officer"/"member" | `pc:137`, `bj:97` | `CellKind::Label` (`tmpl:76`) | REAL words `admin`/`member` (`ga:53-55`); **G4** |
| Members, `td.num` | `pc:137` | `Column{align: Right}` + `CellKind::Number` | needs a count per row, **G1** |
| Joined, plain "2025-04-02" | `pc:137` | `CellKind::Text`, not `Time` (`gd:60-63`) | **G5** |
| No foot or pager | `pc:137` (`pager: false`), `bj:163` | foot drawn only when `next_cursor` is set or `total` is `Some` (`tbl:267`); `total: None` + one page → no foot | ok; **G3** |
| No `td.act`, rows not clickable | `pc:137`, `bj:157,161` | `Row.entity = None` (`tbl:220,230-234`) | ok, **G2** |
| Promote member to admin (Q1 extra) | none (`plan:3531`) | `TableRow` action on the row: extra `th`/`td` (`tmpl:232,238`) | D6 accept line (`gd:56`), **G6** |
| Purges pending | none (X1) | `DashboardAttention` order 65 → `purge_backlog_body` | REAL, **G9** |
| Group entity page | none | `EntityKindDecl{group}` (+ header?) at `/admin/e/group/{id}` | **G7** |

Shell frame on `player.groups`: copy `acc/player.inventory.txt:7-13` (Sign out, pending nav entries, Lore ×3, D3, theme toggle) and the entity-head lines `:14-16`. The tab-bar lines `:17-20` change now that the Groups tab is drawn (§7).

## 2. States per panel, and URLs

| Panel | ok | empty | readonly | stale | error |
|---|---|---|---|---|---|
| Groups tab | rows; a foot appears only past one page | portal Empty; no search, so no narrowed-to-zero state | n/a (`writes: true` with one implemented `TableRow`; an operator below `min_role` sees it disabled) | `cache_ttl_s: None` | error block |
| Tab count | number | "0" | — | — | tab error |
| Purges pending | item while backlog | none at 0 | — | — | per point |
| Group entity page | `h1` id + kind label (+ header per G7) | — | — | — | "Unknown entity kind" only if undeclared (`rt:543-548`) |

URLs: the pager's next link (`tbl:270`) beyond one page; `/admin/e/group/{id}` from the name link (`url:21-23`). The tab declares no `params`.

## 3. `writes` and every ActionDecl (proposal)

`writes: true` (groups becomes a control-plane writer, as mail and wallet are; §8).

| id | Point | Then | Label / icon | order | Emphasis | danger | min_role | implemented |
|---|---|---|---|---|---|---|---|---|
| `membership.promote` (name per X8 style, `gk:58`) | `TableRow("groups")` | `Confirm` (G6) or `Invoke` | "Promote to admin" / `shield`? (part-a choice) | 0 | Ghost (as `aa:424`) | no | Operator | **true** |

Bound element: entity = the panel's `player:<id>` (`tbl:240`, `r.entity.or(ctx.input.entity)`), item = `group_id` (`Row.item`). `Row.actions` lists it only on rows with `state = member` and `role = member` (G6).

## 4. Contributions and `order`

| Contribution | Point | order |
|---|---|---|
| `groups` | `EntityTab{player, groups, "Groups", 1, []}` | 60 |
| `tab.count` | `EntityTabCount{player, groups}` | 0 |
| `attention.purges` | `DashboardAttention` | 65 |
| `group.header` (G7 b only) | `EntityHeader("group")` | 0 |

Entity kind `group` declared (`EntityKindDecl{group, "Group", "users"?}`). No `EntityLabel` (§0.2). All panels `min_role: Viewer` (`fad:208`). Wiring as friends: a `controlplane` dependency, `admin.rs`, and `PROVIDER_SLOT` in `init`.

## 5. Hand-check section (what the §7 `diff` cannot see)

| # | Element | Check |
|---|---|---|
| H1 | Tab label "Groups", count, order after Friends | live look |
| H2 | Meta text (G3) | live look |
| H3 | Group name text and `href` = `/admin/e/group/{id}` | live look; unit test of the cell |
| H4 | Role text: real `admin`/`member` vs the mockup's "officer" (G4) | live look |
| H5 | Members number per row matches the group's live member count | live look |
| H6 | Joined value per G5, `YYYY-MM-DD` | live look |
| H7 | Promote button only on member-role rows; confirm text; toast after; row re-renders with role `admin` | live look in both topologies |
| H8 | Group entity page draws (G7) with no error card | live look |
| H9 | Purges pending item (G9) | golden or a live look after deleting a player |

## 6. Gaps and decisions for the owner

**G1. Reader and the Members column.** `page_mine` has no count (`gs:21-29,661-671`). The mockup shows a member count per row (`pc:137`).
- **Recommendation:** a new admin keyset reader on the `page_mine` SQL plus `JOIN LATERAL (COUNTS_LATERAL)` for `members` (`gs:710-712`). The lateral costs at most one group's roster per row (≤500 members, `ga:42`). Keep the keyset `(m.created_at, m.group_id)` and the existing `encode_cursor`/`decode_cursor` (`gsv:102-135`). Page size 25, as inventory and friends. Reusing `COUNTS_LATERAL` keeps it alive (X4).
- Alternative: call `page_mine` as is and then `counts_tx` per row: N+1 statements, with counts taken at a different instant (`gs:39-40`).

**G2. Rows are not clickable.** With a group `EntityHeader`, `Row.entity = group` would make rows clickable (`tbl:220`). **Recommendation:** `Row.entity = None`, `item = group_id` (the accounts precedent `aa:655-682`), with the link only on the name cell.

**G3. Which rows; tab count, meta and foot.** The mockup shows two member rows, count 2, meta "2" and no foot (`reg:72`, `pc:136-137`). `page_mine` also returns invited and requested rows (`ga:130-133`).
- **Recommendation:** list every row the player holds (all states), with pending rows shown by state in the Role cell (G4). N = all rows, which is the tab count and the meta "N". `PageInfo.total: None` so no foot is drawn on one page (`tbl:267`); the foot appears only when `next_cursor` is set. The count comes from one statement, `SELECT count(*) FROM groups.memberships WHERE player_id = $1` (served by `memberships_player_idx`).
- Alternative: members only (`state = member`); then invitations are invisible to the operator.
- Alternative for the meta: "N · M pending", as friends did (`s17` §10 G4), which differs from the mockup.

**G4. Role text.** The mockup says "officer"; the real roles are `admin|member` (`ds:26,89`). **Recommendation:** draw the real word (`admin`, `member`). A pending row draws its state (`invited`, `requested`) as a `Label`, since its role is empty (`gl:74-76`). Text differences go in H4.

**G5. Joined.** Only `memberships.created_at` exists, and it is not updated when a request or invite is accepted (`gs:420-441`). **Recommendation:** `created_at` rendered `YYYY-MM-DD` in SQL as `CellKind::Text`, with the meaning stated in a doc-comment. Alternative: a `joined_at` column, which is a schema change (DROP and boot fresh, per the wipe rule) and needs owner approval.

**G6. Promote placement and modal.** The mockup has no element for this (Q1 extra, D6, `plan:3531`). `gk:80` marks the placement as open.
- (a) **Recommended:** a `TableRow` on the Groups tab row (`gd:56`), bound to the viewed player and the row's group, offered only where `state = member` and `role = member`. It needs a D6 accept line for the extra `th`/`td` (`tmpl:232,238`).
- (b) A Members tab on the group entity page, with the `TableRow` per member row. This adds a mockup-less tab (G7 c).
- Modal: `Then::Confirm` (non-danger, `ReasonReq` optional; the variant names were not read) or a bare `Invoke`. **Recommendation:** Confirm, because the role change is not undoable from the portal (there is no demote; the player contract has no promote/demote, `old:12-17`).

**G7. The group entity page (O-4).** No mockup page exists. Options:
- (a) Declare the kind only: the page draws `h1 {uuid}` plus "Group" (`shell:63`).
- (b) **Recommended:** the kind plus an `EntityHeader` built from the dead `admin_group` (`gs:618-639`): title = name, subtitle = join policy · created, badges or status = members and pending. This mirrors `evp:591-602` and reuses a dead reader (X4).
- (c) (b) plus a Members tab over `page_group(…, ALL_ROWS, …)` (`gs:542-580,654-656`), which also reuses `ALL_ROWS`. Its user cells would need the accounts label pass (`lb:105-119`).
- Every option needs a `docs/todo/admin-second-pass.md` item for the missing mockup. The owner picks.

**G8. Dead readers (X4).**
- Delete `admin_totals` (`gs:584-593`) and `admin_recent_groups` (`gs:597-614`). This closes GROUPS-09 (`todo:52-56`): move it to `groups-done.md`.
- Reuse `COUNTS_LATERAL` (G1). Reuse `admin_group`, `AdminGroupRow` and `row_to_admin_group` if G7 (b) or (c) is chosen; otherwise delete them. Reuse `ALL_ROWS` only under G7 (c); otherwise delete it.
- GROUPS-05 (`todo:29-34`) stays open unless the new reader fixes the index order.

**G9. Purges pending at 65** (X1). Not a gap: `purge_backlog_body("Groups purges pending", pending, oldest, PURGE_SWEEP_SUB.id)` (`gs:119-127`, `gp:211`), as `fad:197-199`.

**G10. `RoleChanged.actor_id`.** `InvokeCall.operator` now carries the login (`cp:474-475`). The `admin.action` record names the actor and `target = player:<id>`, but not the group (`act:576-589`).
- **Recommendation:** keep `actor_kind = "operator"` and `actor_id = ""` as `ge:39-42` documents. That is no event change, and the group is in the event itself.
- Alternative: fill `actor_id` with the login, which changes documented semantics. The owner decides.

**G11. Stale splitproof GR5.** `spm:2574-2598` asserts the retired `/admin/groups?group=` page. It is listed red as `GR5`/`GR5m` "re-pointed with the groups tab (plan step 18)" (`er:20,54`), and the plan rewrites it in 25h4 (`plan:3212`).
- **Recommendation**, as friends' G10: 18t writes `groups_admin_checks.rs` (§9) and GR5's rewrite stays in 25h4, pointed at the Groups tab and the group page. Both `er` lines keep their reasons, re-worded to "re-pointed in 25h4".
- Or 18t deletes GR5 and its two `er` lines.

## 7. Accept-file plan (predicted; the exact text needs a crawl)

- `pages`:
  - Drop `player.groups` from `EXCLUDED_TABS` (`pages:357`).
  - Add `PAGE_MAP` tab entries `player`/`groups` with and without `USER_MENU`, as inventory's (`pages:306,315`).
  - No `EXCLUDED_MENUS` row: groups declares no `RowAction`.
  - `EXCLUDED_MODALS`/`mock_modal` rows for `player.groups` and `player.groups.menu.user`, copied from inventory's (`pages:436-441,615-620`).
  - The promote Confirm has no mockup opener: it needs a modal-inventory decision (not traced, see Not verified).
- **`player.groups.txt`** (new):
  - Shell lines copied from `acc/player.inventory.txt:7-13` and entity-head lines `:14-16`.
  - Tab-bar lines re-derived.
  - The extra `th` and `td` with the promote button: a D6 accept line (Q1).
  - Rows: the mockup's two vs the crawled player's → `data difference` until 24b2 seeds groups (`plan:3133`; demoseed already has groups data, `tools/demoseed/src/tests.rs:28-47`, but whether the crawl's player is in a group was not checked).
- **`player.groups.menu.user.txt`** (new): copy the inventory `.menu.user` file.
- `player.txt`, `player.characters.txt`, `player.wallet.txt`, `player.inventory.txt` (and `player.friends.txt` if 17b lands first) and their `.menu.user` files: one "not ported yet" tab line goes or changes per file.
- **`index.txt`**: no line for Purges pending on a fresh fleet.
- No accept file for `/admin/e/group/…`: there is no mockup host to pair it with (G7).

## 8. The promote-member-to-admin write (Na2)

**What exists:**
- The player contract has no promote op (`ga:121-202`; `decide` admits or kicks, `respond` accepts an invite). The old operator promote lived only in `old:443-536`.
- Every piece it called survives: `lock_group_tx` (`gs:204`), `group_name_tx` (`gs:298`), `membership_tx` (`gs:395-414`), `promote_tx` with the state in the predicate (`gs:420-441`), and `ROLE_CHANGED` (`ge:126`).
- The old flow (`old:443-536`), in one transaction:
  1. Lock the group.
  2. Read its name; a missing group is Stale.
  3. Read the membership; no row or a non-member row is Rejected.
  4. If the player is already admin, roll back and return `already_admin`.
  5. Otherwise `promote_tx(member → member, role admin)`; zero rows is Stale.
  6. `emit_tx(ROLE_CHANGED{…, ACTOR_OPERATOR, ""})`, then commit.

**Port to `Provider::invoke`** (proposal, mail's shape `mad:87-89,574`):
1. The first statement is `input_within_cap(&req.input)`, or `Error::invalid` (`mad:87-89`, `wad:315-317`).
2. `entity` must be `player:<uuid>` (`is_uuid_text`, `UUID_SHAPE_BYTES = 36`, `gc:21`). `item` must be the group uuid, or `Error::invalid`.
3. The old flow, mapped to outcomes:

| Old result | New outcome |
|---|---|
| Stale | `Error::conflict` or `not_found` |
| Rejected | `Error::invalid` |
| `already_admin` | `InvokeOutcome::NoOp{"… was already an admin of …"}` (`cp:505-506`) |
| success | `Ok{message: "… is now an admin of <name>"}` |

- **Idempotency:** natural, by state. A repeat finds `role = admin` and answers NoOp, so `request_id` is unused and `pol:76-79` needs no groups clause.
- Concurrency: the group advisory lock plus the `state = $3` predicate (`gs:420-441`), as all other roster writes do (`gl:5-13`).
- Audit: the portal appends `admin.action{actor, "groups.<action>", target "player:<id>", detail}` for every attempt (`act:576-589`). The durable `group.role_changed` names the group (G10).

**The probe to restore** (`oldc`; deleted in Step 1, `plan:487-490`):
- `conformance_admin_group_rejected(len)` and `conformance_admin_player_rejected(len)` drove `admin::apply_submit` over a dead pool. At the admitted length (36) they reached the store and answered Internal; at any other length they were refused (`oldc`, the "selected group's shape" and "promoted player's shape" fns). Their CapCases are `oldp:574-583` ("groups admin selected group id", "groups admin promoted player id", cap `UUID_SHAPE_BYTES`).
- **Mail's G12 precedent** (5d5e6973; mail's conformance and policy entry only):
  - `mc:153-195` builds an `InvokeCall` (`operator: "conformance"`, `item`, `request_id`) and calls `MailAdmin::invoke` over `dead_pool()`. `refused` = `Err(Status::Invalid)`.
  - Probes `conformance_cp_invoke_input_rejected` (`mc:128-131`), `conformance_test_key_rejected` (`mc:139-143`) and `conformance_outbox_id_rejected` (`mc:149-151`). Consts `CP_INVOKE_INPUT_VALUE_BYTES` (`mc:24-25`), `OUTBOX_ID_SHAPE_BYTES` (`mc:21-22`).
  - CapCases `pol:922-934`.
- **For groups** (proposal, in `gc`):
  - `CP_INVOKE_INPUT_VALUE_BYTES = controlplane::MAX_INPUT_VALUE_BYTES`.
  - `conformance_cp_invoke_input_rejected` (an over-cap input value with a well-formed entity and item; at the cap it reaches the dead pool).
  - `conformance_admin_group_rejected` (item length).
  - `conformance_admin_player_rejected` (entity id length).
  - All three go through `GroupsAdmin::invoke` over the existing `service()`/`dead_pool()` (`gc:27-51`).
  - CapCases in `pol` `groups()` (`pol:583-671`, the input-byte-caps list `pol:616-632`): "cp.invoke groups input value" (mandatory: `chk:239-262` fails a `writes: true` manifest without a CapCase whose name contains `cp.invoke` and the module), "groups admin bound group id" and "groups admin promoted player id".
  - Extend the `cp.invoke req.input.<value>` basis text, which lists every writer (`pol:180`), with a groups clause.
- Policy rows `cp.invoke req.item`/`req.entity.id` are generic (`pol:176-185`); no per-module row is expected (not verified against the input golden).

## 9. Splitproof (18t, on the `sfa` pattern)

Pattern:
- `sfa:11-20`: a doc of what crosses, plus a `Break run` line.
- `sfa:21-26`: the FA0 completion check.
- Checks `sfa:59-119`; cells found by header label (`sfa:144-162`).
- Called from `spm:589` (monolith) and `spm:6184` (split). 17t's file is uncommitted (`git status`: `?? tools/splitproof/src/friends_admin_checks.rs`, ` M tools/splitproof/src/main.rs`, +3 lines).
- Groups fixtures can reuse the GR pass helpers (`spm:2265-2360`, create, join and members through the public API).

Proposal, both topologies, each with a recorded break run (admin-svc without its `groups` peer, `cmd/admin-svc/src/lib.rs:36`):
- **GA1:** A creates group G (admin); B joins G (member). B's entity page slots the Groups panel and its count. The panel draws G's row with the name linked to `/admin/e/group/{G}`, Role `member` and Members `2`. In the split the panel is built in groups-svc (`:9015`): that is the at-risk path.
- **GA2:** promote B through the portal: `POST /admin/_action/groups/<action>` with the csrf and entity `player:B`, item G (form fetch as `wallet_ledger_checks.rs:46-66`). Then the DB row is `role = admin`, one `group.role_changed` row is in the durable log, and a repeat answers NoOp. This is the "one real invoke per writing provider" `plan:3406` assigns to 25h4: the owner picks 18t or 25h4.
- **GA3:** `GET /admin/e/group/{G}` → 200, no error card, with the name in the page under G7 (b) or (c).
- **GA4:** the tab count reads the agreed N (G3).

## 10. Part sizes (estimate)

| Part | Content | Files | Tool calls |
|---|---|---|---|
| 18a1 (Na1 with Na3) | `admin.rs` (tab, count, purges, kind, G7 header), new keyset reader + deletions in `store.rs`, `lib.rs`, `Cargo.toml`, tests, `pages.rs`, todo files | 8-9 | ~55-65 |
| 18a2 (Na2) | invoke + `TableRow` decl, `conformance.rs` probes, `policy.rs` CapCases + basis text, tests | 4-5 | ~35-45 |
| 18t | `groups_admin_checks.rs` + two call sites | 2-3 | ~35-45 |
| 18b | crawls, accept files of §7 | 6-10 | ~20-30 |

## Not verified

- I ran no cargo, `admindiff mock`, crawl, diff, splitproof or conformance. Every accept line in §7 is a prediction.
- `ConfirmTone`/`ReasonReq` variant names, and whether controlplane validation allows `Then::Confirm` on a `TableRow`: the read was refused at the budget.
- How the portal posts a `Then::Invoke` versus a `Then::Confirm` `TableRow` (the `action_submit` body past `rt:1000`), and whether it sends `request_id` for them.
- The `EXCLUDED_MODALS`/`mock_modal` rows a portal-only Confirm needs (validator `pages` not traced).
- The `.agents/skills/mockup-implementation/SKILL.md` text itself (the format follows `s17`).
- Whether 17b has landed `player.friends.txt` before 18b (it is not in `acc` at HEAD).
- Consumers of `group.role_changed` outside `modules/groups` (only `modules/chat` was checked: it does not exist). Note `ge:204-218`: `Membership::role_of` serves chat-like consumers.
- Whether the crawled player belongs to a demoseed group.
- The `cp.invoke` input golden / public-api baselines that a `writes: true` groups manifest and the new `gc` pub fns would change (`docs/reference/public-api-baseline`).
- The characters `EntityLabel`-only kind (`modules/characters/src/admin.rs:273-281`), cited from grep hits only.
- Where friends' and inventory's admin unit tests live (`modules/friends/src/tests.rs:2103-2268` per grep). Part a follows that precedent.

## 11. Owner decisions (2026-11-22)

"ok moze byc zgodnie z rekomendacjami": every gap as recommended.
- **G1:** a new admin keyset reader on the `page_mine` SQL with `JOIN LATERAL (COUNTS_LATERAL)` for Members; keyset `(m.created_at, m.group_id)`, the existing cursor helpers, page size 25.
- **G2:** `Row.entity = None`, `item = group_id`; only the name cell links.
- **G3:** every row the player holds, all states; N = tab count = meta "N"; `PageInfo.total: None`, so no foot on one page.
- **G4:** the real words `admin`/`member`; a pending row draws its state (`invited`/`requested`), as a `Label`.
- **G5:** `memberships.created_at` as `YYYY-MM-DD` from SQL, `CellKind::Text`, its meaning in a doc-comment; no schema change.
- **G6: (a).** A `TableRow` "Promote to admin" on the Groups tab row, only where `state = member` and `role = member`, behind `Then::Confirm`; a D6 accept line.
- **G7: (b).** Kind `group` plus an `EntityHeader` built from `admin_group`; a `docs/todo/admin-second-pass.md` item for the missing mockup.
- **G8:** delete `admin_totals`, `admin_recent_groups` and `ALL_ROWS`; keep `COUNTS_LATERAL`, `admin_group`, `AdminGroupRow`, `row_to_admin_group`; move GROUPS-09 to done.
- **G9, G10:** as written (`actor_id` stays empty for the operator).
- **G11:** 18t writes `groups_admin_checks.rs` (GA1-GA4); GR5's rewrite stays in 25h4.
- **GA2:** the real portal promote runs in 18t, both topologies.
- Before 18a2: a short research of `Then::Confirm` on a `TableRow` (variant names, validation, how the portal posts it and whether it sends `request_id`) and the admindiff modal rows a portal-only Confirm needs.

- **Promote modal (2026-11-22, after §12):** `Then::Confirm` with tone Warn (a portal promote cannot be undone, there is no demote), `prepare: true` so the body names the player and the group ("Promote <player> to admin of <group>?", built by the provider as mail's `cancel` does), confirm label "Promote", reason Optional, no type-to-confirm, no impact lines.

## 12. Research before 18a2: `Then::Confirm` on a `TableRow` (2026-11-22, read-only)

I found what 18a2 needs. `Then::Confirm` on a `TableRow` is allowed by every check I read, and the portal posts it through the modal-form path, which sends a `request_id`. No production module has a working Confirm on a `TableRow` yet. I did not run cargo, splitproof or any build. Grep counts below are lower bounds.

**1. Confirm shape and validation**
- `Then` has four variants, `{ Invoke, Form(FormSpec), Confirm(ConfirmSpec), Link(Link) }` (`G:\Projects\swarog\core\controlplane\src\lib.rs:753-758`). `invokes()` is true for Confirm (`:762-768`).
- `ConfirmSpec` has these fields: `title`, `body`, `tone: ConfirmTone`, `impact: Vec<(String, Cell)>`, `reason: ReasonReq`, `reason_hint: Option<String>`, `type_to_confirm: Option<String>`, `confirm_label: String` (`:1024-1034`).
- `ConfirmTone` is `{ Danger, Warn, Info }` (`:1039-1043`). `ReasonReq` is `{ Required, Optional, None }` (`:1049-1053`).
- How the tone renders (`G:\Projects\swarog\core\admin\src\render\modal.rs:89-111`):
  - Danger draws a danger button and the alert icon.
  - Warn and Info draw a primary button and the info icon.
- No rule ties Confirm to a point kind. `G:\Projects\swarog\tools\admincheck\src\checks.rs` has three related rules:
  - `PanelForm` must be `Then::Invoke` (`:269-273`).
  - `prepare` requires Confirm or Form and is not allowed on `PanelForm` (`:277-286`).
  - `icon_only` requires `TableRow` (`:296`).
- `controlplane`'s own mode check only covers form modes (`lib.rs:1002-1018`).
- A golden fixture already draws a `TableRow` with `Then::Confirm` (Danger), which suggests the template path is exercised: `G:\Projects\swarog\core\admin\src\golden_tests\build.rs:606-618`.

**2. How the portal renders and posts it**
- `hx_action`:
  - **Invoke:** `hx-post="{{a.url}}" hx-vals="{{a.vals}}"`.
  - **Anything else, Confirm included:** `hx-get="{{a.url}}"` into `#modal-root` (`G:\Projects\swarog\core\admin\src\templates\primitives.html.tmpl:98-100`).
  - The row cell is drawn at `:232,238`. `bind` puts `?entity=&item=` on the URL for the modal and `_entity`/`_item` in `vals` for a plain invoke (`G:\Projects\swarog\core\admin\src\render\action.rs:70-80`).
- **Opening the Confirm:** `GET /admin/_action/{module}/{action}?entity=kind:id&item=` goes to `action_modal` (`G:\Projects\swarog\core\admin\src\routes.rs:932-1000`):
  - It runs `admit` and validates entity and item.
  - If `prepare` is set, it fetches the spec from the provider (`:975-991`); otherwise it uses the declared `StoredSpec::Confirm` (`:955-960`).
  - It creates a form row that stores the entity and item.
- **Submitting the modal:** the modal form posts `_csrf`, `_form`, and `_reason` / `_confirm` where the spec asks for them (`G:\Projects\swarog\core\admin\src\templates\modal.html.tmpl:8-20`). It does not send `_return`, entity, item or `request_id`. For a Confirm only `_csrf|_form|_reason|_confirm|_return` are accepted (`G:\Projects\swarog\core\admin\src\engine\actions.rs:386-402`).
- `action_submit` checks CSRF before anything else, then routes by `_form`: present goes to `submit_form`, absent goes to `submit_click` (`routes.rs:1048-1086`).
- **Confirm path (`submit_form`)**, `InvokeCall` built at `routes.rs:1196-1206`:
  - `module` and `entity` come from the stored row, `action` is `decl.id`, `item` is `row.item`.
  - `input` comes from the submission (empty for a Confirm), plus `expected_revision`, `reason` and `operator`.
  - `request_id` is `Some(row.id)`, the form id. **So yes, a Confirm sends a `request_id`, and it is the form instance id.**
  - The submission check enforces a required reason and the type-to-confirm text (`actions.rs:506-512`).
- **Invoke path (`submit_click`, `routes.rs:1215-1267`):**
  - It refuses anything that is not `Then::Invoke` with "must be submitted from its form" (`:1228-1232`).
  - The entity and item come from the posted `_entity`/`_item`.
  - `request_id` is `click_request_id(&row)` from the click row (`:1257`).
- After either path, the redirect target is the outcome's link, else `_return`, else the entity (`actions.rs:715-721`).

**3. Is there a working `TableRow` + Confirm already?**
- Not in any module I grepped. I searched `mail`, `wallet`, `groups`, `accounts`, `friends`, `notifications` and `core/admin`:
  - accounts' `TableRow` actions are `not_implemented`: Unlink and Revoke at `G:\Projects\swarog\modules\accounts\src\admin.rs:421,424`.
  - wallet has no Confirm.
- Closest precedents:
  - **mail `cancel`:** `Then::Confirm`, `prepare: true`, implemented, but it sits on `ActionPoint::QueueRow`, not `TableRow` (`G:\Projects\swarog\modules\mail\src\admin.rs:245,325-328`, prepare at `:499-510`). Same modal and form path, so it is the best runtime precedent.
  - **events `SUB_PAUSE`:** a Warn Confirm without `prepare`, implemented, on `RowAction` (`G:\Projects\swarog\core\admin\src\events_provider.rs:237,249-259,322`). This is the closest to a static, non-danger Confirm.
  - **golden `table_row_actions_case`:** the only `TableRow` + Confirm I found (`build.rs:612-620`).

**4. admindiff rows (`G:\Projects\swarog\tools\admindiff\src\pages.rs`)**
- A portal modal action with no mockup opener goes in `EXCLUDED_MODALS` as `(host, "<module>/<action>", ModalWhy{why})` (`:384-450`). Example: mail `test-send` at `:386-389`.
- What `validate_excluded_modals` (`:926-951`) requires of an entry:
  - It is listed once and has a reason.
  - Its host is a modal host (`is_modal_host`), meaning a `PAGE_MAP` page or tab that is not pending, a menu target of one, or an excluded menu.
  - The action is a valid `module/action` id.
  - It is not also a `PAGE_MAP` modal entry for that host.
- `validate_mock_modals` (`:1042-1060`) holds the "the mockup has no opener" claim against the mockup: every opener the mockup draws on that host must be a `PAGE_MAP` modal entry or a `mock_modal` row of another action.
  - `changeRole` and `suspend` count as "another action": `Absent(LORE)` waits for lore, not groups, and `Disabled("accounts/suspend-header")` names an accounts action.
- `mock_modal(host, MockOpener, PortalModal, why)` is the other direction: a mockup opener the portal does not diff (`:585-631`). `PortalModal::{Action, Disabled, Unseeded, Absent}` is defined at `:535-565`.
- `player.groups` is currently in `EXCLUDED_TABS` (`:357`).
- **Rows 18b would need:**
  - Remove the `player.groups` line from `EXCLUDED_TABS`.
  - Add a `PAGE_MAP` tab entry `player.groups` and a menu entry `player.groups.menu.user` (`menu: Some(USER_MENU)`), copying the wallet entries around `:290-299`.
  - Copy the per-tab `mock_modal` rows from wallet (`:609-614`) to `player.groups` and `player.groups.menu.user` (changeRole/LORE, suspend-header on the tab; suspend, deleteAccount, changeRole, releaseAll on the menu).
  - Copy the `EXCLUDED_MODALS` rows for `wallet/currency.grant` and `wallet/currency.revoke` to host `player.groups.menu.user`, following `:426-433`.
  - Add one new `EXCLUDED_MODALS` row: `("player.groups", "groups/<promote id>", ModalWhy{why: "Q1 extra (D6): the mockup's Groups tab draws no promote element, so the mockup has no modal"})`.
  - No promote row is needed on `player.groups.menu.user`. A `TableRow` action draws only on table rows (`G:\Projects\swarog\core\admin\src\golden_tests\view_tests.rs:369-384`), not in the user menu. This holds as long as groups adds no `RowAction` on `player`.
  - The exact list depends on what the mockup's Groups tab render actually draws, which only the crawl can show.

**Brief for 18a2**
- Declare `membership.promote` as `ActionPoint::TableRow("groups")` with `Then::Confirm(ConfirmSpec{ tone: Warn or Info (not Danger, matching the checklist's non-danger choice), reason: Optional, type_to_confirm: None, confirm_label: "Promote", impact: vec![] })`.
  - Set `danger: false` and `emphasis: Ghost`.
- `prepare: false` is enough unless the body should name the group or player. In that case set `prepare: true` and add a `prepare` arm, as mail does (`G:\Projects\swarog\modules\mail\src\admin.rs:499-510,596-599`).
- No `controlplane` or `admincheck` change is needed.
- The invoke handler receives:
  - `req.entity = player:<id>` and `req.item = group_id` from the stored form row;
  - `req.reason` when one is given;
  - `req.request_id = Some(form id)`, which is stable for one modal instance.
  - Make it idempotent the way checklist §8 says: a repeat after the role is already `admin` answers NoOp, and zero rows from `promote_tx` is Stale.
- For splitproof GA2 (18t), the post must be the two-step form path: GET `/admin/_action/groups/membership.promote?entity=player:B&item=G`, read `_form` and `_csrf` from the HTML, then POST `_csrf` + `_form`. A bare POST with `_entity`/`_item` is refused as "must be submitted from its form" (`routes.rs:1228-1232`).
- Nothing in 18a2 touches admindiff. The rows listed in §4 belong to 18b.

**Open**
- Tone (Warn vs Info), whether `prepare` is wanted for a body that names the group, and the label are the owner's or the plan's call.
- Whether the mockup's Groups tab render draws any other opener is unchecked; only a crawl settles it.

**Not checked**
- `admin.js` modal handling.
- The modal-instance `classify_form`/`settle` semantics on a NoOp outcome.
- The `PAGE_MAP` wallet entry lines `:286-299`, read by line number only.
- The uncommitted `tools/splitproof/src/friends_admin_checks.rs`.

