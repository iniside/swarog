# Step 24 (audit): checklist

Read-only extraction per `.agents/skills/mockup-implementation/SKILL.md` (Standing decisions, §1-§3), on the layout of `docs/plans/2026-11-23-step22-scheduler-checklist.md`. HEAD at extraction: `f8707ca9`. Mockup pages: `UILayout/redesign/audit.html` (`P.audit`) and `UILayout/redesign/index.html` (`P.dashboard`, "Recent operator activity" panel only). Scope: `plan:2454-2477` (Step 24, parts 24.a1, 24.a2, 24.b). 24.a3 (admincheck peer finding), 24.t (its fixture) and 24.a4 (dead-code sweep, X9) are later parts and are not in this checklist.

Owner rulings applied: admindiff is out of this plan (2026-11-24), so there is no accept plan, no crawl and no `PAGE_MAP` work (§7); what admindiff would have caught is a row in §5. The primitive vocabulary of `core/controlplane` is frozen: anything it cannot carry is a gap in §6 with a proposed decision.

Builder lines:
- Page `P.audit` `pp:112-126`. Crumbs Platform / Audit log (`pp:113`). Head (`pp:118`): title "Audit log", subtitle "Append-only record of operator, key and system actions.", head action `btn` `download` "Export" (no onclick). Layout: one panel, **no title** (`pp:119`: `panel({c, flush, body})`, no `title` → no `.panel-head`, `bj:146`).
- Table (`pp:120-124`): search placeholder "Target or detail contains…", width 260 (`pp:120`); filters (`pp:121`): Topic select ("Topic: all" + topics derived from the rows, `pp:115`), Actor select ("Actor: anyone" + fixed `marta.k, jonas.l, aiko.tanaka, system, scheduler, apikey:ak_c4a8`), a chip `{k:"Time", v:"today"}`; then the toolbar's own "+ Filter" chip (`bj:155`). Columns (`pp:122`): Time (sorted desc), Actor, Topic, Action, Target, Detail (`wrap`). Cells (`pp:123`): Time `span.mono` HH:MM:SS; Actor `P.actor` (`pc:10`: a known handle → `userCell`, `bj:102-108`, an `a` to player.html around `span.user-cell`; otherwise `span.tag`); Topic `span.tag`; Action `span.mono` inline `font-weight:500`; Target `span.mono` inline `font-size:11.5px`; Detail `span.dim`. Foot (`pp:124`, `bj:166`): "{n} of 1,204 entries today", page "1 / 49", the 25/50/100 select. No row menu, no clickable rows.
- Dashboard panel `pc:53-57`: `{module: "audit", point: "dashboard.panel", prim: "Timeline"}`, title "Recent operator activity", meta "audit", `flush`; tool `a.btn.sm.ghost` "Audit log" + `right` icon after the label (`pc:55`); body `timeline(D.audit.slice(0, 7))` (`pc:56`): `when` = HH:MM, html = `P.actor(actor)` + `span.mono` action, meta = "{target} · {detail}" plain text, icon by topic (`lore*` repo, `wallet` coins, `events` zap, else activity), tone `err` when the action contains "force" or "paused". Placed in `div.grid.g-21` after "Needs attention" (`pc:65`). Empty: `bj:216` "No events" / "Nothing recorded in this range.".
- Registry: nav `reg:33` (platform, id audit, "Audit log", icon list). No audit search provider, no audit attention item.
- Fake rows `dj:159-170` (ten rows; actors `marta.k`, `jonas.l`, `aiko.tanaka` operators, `system`, `scheduler`, `apikey:ak_c4a8`).
- Out of scope, with reasons: apikey menu "Requests by this key" (`pp:15`; Step 21 landed it as a static link, ADMIN2-60); Lore "Audit" tab and "Audit trail" (`pl:21-22`, `pl:91`, `pl:133`: no Lore module); `dj:118` `audit-svc` health row (portal FAKE, another panel).

Abbreviations: `pp` = `UILayout/redesign/assets/pages-platform.js`, `pc` = `…/pages-core.js`, `pl` = `…/pages-lore.js`, `reg` = `…/registry.js`, `dj` = `…/data.js`, `bj` = `UILayout/design-system/components/bundle.js`, `al` = `modules/audit/src/lib.rs`, `cp` = `core/controlplane/src/lib.rs`, `cpe` = `core/controlplane/src/events.rs`, `ord` = `core/controlplane/src/order.rs`, `act` = `core/admin/src/engine/actions.rs`, `adl` = `core/admin/src/lib.rs`, `rt` = `core/admin/src/routes.rs`, `tbl` = `core/admin/src/render/table.rs`, `cel` = `core/admin/src/render/cell.rs`, `til` = `core/admin/src/render/tiles.rs`, `ptpl` = `core/admin/src/templates/primitives.html.tmpl`, `shl` = `core/admin/src/templates/shell.html.tmpl`, `app` = `core/app/src/lib.rs`, `sp` = `tools/splitproof/src/main.rs`, `red` = `tools/splitproof/expected-red.txt`, `ds` = `docs/research/2026-10-22-1830-admin-redesign-data-sources.md`, `gaps` = `docs/research/2026-11-21-0848-admin-ports-21-24-contract-gaps.md`, `plan` = `docs/plans/2026-10-22-1759-admin-control-plane-redesign-plan.md`, `todo` = `docs/todo/admin-second-pass.md`.

Method: targeted reads with line numbers; scoped ripgrep marked **[grep]** (a lower bound). No LSP pass, no cargo, no `admindiff mock`, no fleet.

## 0. Required checks

**0.1 Who owns `audit.log` (confirmed).**
- `audit` owns schema `audit` and touches no other module's tables (`al:1-2`). One table `audit.log(id bigserial PK, topic text, payload jsonb, at timestamptz DEFAULT now(), event_id text)` (`al:105-113`); one index `log_at_idx ON audit.log(at)` (`al:245-246`). Nothing indexes `topic` or any payload key.
- Writers: only audit. `RecordHandler` inserts the raw event JSON under its topic on the delivery tx (`al:127-156`, insert `al:144`); `PruneHandler` deletes rows older than `AUDIT_RETENTION_DAYS` (default 30) on `scheduler.fired{audit-prune}` (`al:194`).
- Logged topics: the 16 of `DURABLE_TOPICS` (`al:47-64`), `admin.action` among them (`al:55`, subscription `audit.admin-action.v1` `al:77`). `scheduler.fired` is subscribed only for the prune, never logged.
- Readers in production code: **none** [grep `audit\.log` over `core`, `modules`, `tools`, `cmd`]: only `modules/audit/src/tests.rs` and splitproof direct SQL (`sp:1924`, `:3356`, `:4720`, `:5113`, `:5139-5145`, `:5307-5313`, `:6970`).

**0.2 Who owns `admin.action` (confirmed).**
- Contract in core: `controlplane::events::AdminAction{actor, action, target, detail}` (`cpe:20-26`), `ACTION = define("admin.action", 1, MinRetention{30 d})` (`cpe:38-39`).
- Emitted only by the portal (`core/admin`): every invoke attempt (`act:586-592`: actor = operator login, action = `{module}.{action}`, target = `kind:id` or empty, detail = outcome then ` — reason: …`, `act:597+`), through `emit_action` in its own tx (`adl:629-638`); logins `login-locked` (`adl:748-752`), `login-succeeded` (`adl:782-786`, detail `ip:<ip>`), `logout` (`adl:1010-1014`).
- Only `admin.action` carries an actor; the other 15 payloads carry ids (§6 A3).

**0.3 Today's audit admin surface (confirmed).**
- None. `al` has no `PROVIDER_SLOT` and no `EDGE_SLOT` contribution (whole file read, 307 lines); `modules/audit/Cargo.toml` has `controlplane` only under `[dev-dependencies]`. Step 1 razed the old inline admin (`plan:500-513`, `b8435c9a`); `api/audit` no longer exists.
- **Stale prose** (leads only): `cmd/audit-svc/src/main.rs:1-5,21-24,28-30`, `cmd/audit-svc/src/lib.rs:8-10` and `cmd/audit-svc/Cargo.toml` (comments) say audit contributes an `admin.adminData` face; `docs/todo/audit.md` AUDIT-02 cites `admin_data` at `modules/audit/src/lib.rs:264`, which no longer exists. 24.a1 corrects the comments to the `cp.*` face and moves AUDIT-02 to `audit-done.md` as obsolete (`b8435c9a`).

**0.4 Where the provider lives, both topologies (confirmed by read; nothing run).**
- `modules/audit/src/admin.rs` (provider) + `modules/audit/src/store.rs` (readers), contributed to `controlplane::PROVIDER_SLOT` in `Audit::init`, as apikeys (`modules/apikeys/src/lib.rs:308`) and scheduler (`modules/scheduler/src/lib.rs:681`). `modules/audit/Cargo.toml` moves `controlplane` to `[dependencies]`. `controlplane` is a core crate: module → core is the allowed direction (`.agents/shared/gamebackend.md` hard constraints 1-2); no module→module import, no `api/*` crate needed. The provider reads only schema `audit` and parses payloads in SQL (`payload->>'actor'`), keeping audit's zero-payload-coupling design (`al:4-8`).
- Monolith: `cmd/server/src/lib.rs:55` hosts `audit::Audit`; the in-process portal collects the slot.
- Split: `audit-svc` serves an edge (`cmd/audit-svc/src/main.rs`); `app:1030-1035` registers the `cp.*` face from `PROVIDER_SLOT` unconditionally; admin-svc already dials audit (`cmd/admin-svc/src/lib.rs:30` `peer_face("audit", …, "127.0.0.1:9004")`, `cmd/admin-svc/src/main.rs:35`). No `cmd/*` code change.
- Cross-schema rule: the provider must not read `admin.users` (schema `admin`, core admin's), so the actor list comes from `audit.log` itself (A5).

**0.5 Carriers the page needs exist (confirmed).**
- `Point::DashboardPanel` (`cp:665`), placed by the portal in the attention `g-21` grid (`rt:428`, `shl:48`) = the mockup's place (`pc:65`). `ActionDecl.trailing_icon` (`cp:704`) + `Emphasis::Ghost` for "Audit log →" (`plan:1456-1459` names this port as its first user). `Link::Page{module, page, params}` (`cp:373-376`). `FilterKind::Select` (`cp:1198-1207`), `PageDecl.params` (`cp:609`), `Column.sorted`/`wrap` (`cp:1163-1165`), `PageInfo.total` (`cp:1192`).
- **No `order::dashboard_panel` module** (`ord` holds platform_nav, dashboard_tile, dashboard_attention, dashboard_wide, player_*): 24.a2 adds it (plan:1915-1918, one constant per contributor), a pub item of `controlplane` → public-API baseline in the same commit. Nav order exists: `platform_nav::AUDIT = 50` (`ord:10`).

**0.6 Renderer facts the mapping relies on (confirmed).**
- User cell with `Cell.link` and no `inner_link` draws `<a href style="color:inherit"><span class="user-cell">…` (`ptpl:83,91`), the mockup's `userCell` shape (`bj:107`); avatar colour and initials from the value (`cel:92-93`).
- Select filter draws `<option value="">{label}: all</option>` (`ptpl:217`); any other kind a chip "{label}: {value or any}" (`ptpl:218`); "+ Filter" is always drawn (`ptpl:219`); search input width is fixed 220 (`ptpl:216`).
- Foot: without `item_noun` "N of M" (`tbl:212`); with one "M entries · showing 1–N" (`tbl:214-215`); page label "1 / ⌈M/size⌉" (`tbl:83-90`).
- Timeline time = `at[11..16]` HH:MM (`til:125-136`); `target` draws as a linked text cell before " · detail" (`til:145-160`, `ptpl:264`); empty timeline = "No events / Nothing recorded in this range." (`ptpl:392`) = `bj:216`.
- The dashboard passes the 1 h / 24 h / 7 d range to each panel (`rt:420-424`, `PanelQuery.range` `cp:442`).

## 1. Element table

### Page head and shell

| Element | Mockup line | Carrier | Status |
|---|---|---|---|
| Nav "Audit log", list icon, Platform | `reg:33` | `NavEntry{section "platform", id "audit", page PAGE, order platform_nav::AUDIT, counter None}` | ok |
| Crumbs Platform / Audit log | `pp:113` | portal crumbs | hand-check H1 |
| Title, subtitle | `pp:118` | `PageDecl.title/subtitle` | ok; text H1 |
| Head action "Export" `btn` download | `pp:118` | `ActionDecl` `PageHead(PAGE)`, `implemented: false` | plan-settled; disabled + todo (§3) |
| One untitled panel, `flush` | `pp:119` | `Layout::Stack([log])`, `ContributionDecl.title` "" | ok if an empty title draws no head (Not verified; A11) |

### Audit log table

| Element | Mockup line | Carrier | Status |
|---|---|---|---|
| Search "Target or detail contains…" | `pp:120` | `DataTable.search_placeholder`; `PanelQuery.search` | REAL, unindexed `ILIKE` over `payload->>'target'`/`'detail'` (comment names the missing index); width 260 vs 220: H4 |
| Topic select | `pp:121` | `FilterDef{key "topic", label "Topic", Select(…)}` | options A4 |
| Actor select | `pp:121` | `FilterDef{key "actor", label "Actor", Select(…)}` | options A5; "Actor: anyone" vs "Actor: all" H5 |
| Time chip "Time: today" | `pp:121` | `FilterDef{key "time", label "Time", Range}` chip | A6 |
| "+ Filter" chip | `bj:155` | template literal (`ptpl:219`) | ok |
| Column Time ↓ `span.mono` | `pp:122-123` | `CellKind::Mono`, `Column.sorted: Desc` | REAL `at`, SQL `to_char` UTC (A9) |
| Column Actor | `pp:123` | operator: `CellKind::User` + `Cell.link` X5; else `CellKind::Ref` | REAL for `admin.action`; per A3 otherwise |
| Column Topic `span.tag` | `pp:123` | `CellKind::Ref` | REAL `topic` (A3, D4 vocabulary) |
| Column Action `span.mono` weight 500 | `pp:123` | `CellKind::Mono` | REAL / derived (A3); inline weight H6 |
| Column Target `span.mono` 11.5px | `pp:123` | `CellKind::Mono` | REAL / derived (A3); inline size H6 |
| Column Detail `span.dim`, wrap | `pp:122-123` | `CellKind::Dim`, `Column.wrap: true` | REAL / derived (A3) |
| Foot "n of 1,204 entries today" | `pp:124` | `PageInfo.total` exact `count(*)` + `item_noun` (A7) | plan-settled count; text H7 |
| Pager "1 / 49", 25 per page | `pp:124` | `PageInfo{size 25, next_cursor}` keyset on `(at, id)` DESC | REAL |
| Row order | data order | `ORDER BY at DESC, id DESC` (`at` indexed) | rule: real stable order |

### Recent operator activity (dashboard)

| Element | Mockup line | Carrier | Status |
|---|---|---|---|
| Title "Recent operator activity", `flush` | `pc:54` | `ContributionDecl.title`, `Point::DashboardPanel` | ok |
| Meta "audit" | `pc:54` | `PanelBody.meta = "audit"` | ok |
| Tool "Audit log →" ghost | `pc:55` | `ActionDecl` `PanelTool(<id>)`, `Then::Link(Link::Page{audit, PAGE, {}})`, `Emphasis::Ghost`, `trailing_icon: true`, icon `right` | ok (§3) |
| Seven entries | `pc:56` | `Timeline.entries`, `LIMIT 7` | row source A12 |
| Time HH:MM | `pc:56` | `TimelineEntry.at` RFC 3339 from SQL | ok (`til:125-136`) |
| Actor | `pc:56` | `TimelineEntry.actor` = X5 user cell | REAL |
| Action `span.mono` | `pc:56` | `TimelineEntry.verb` | REAL |
| Meta "target · detail" | `pc:56` | `TimelineEntry.detail` = "{target} · {detail}", `target: None` | A13 |
| Icon by topic, `err` tone | `pc:56` | `TimelineEntry.icon` / `tone` | A14 |
| Timeline flags | n/a | `days: false`, `date_only: false`, `live: false`, `cursor_before: None` | ok |

## 2. States per panel, and URLs

| Panel | ok | empty | readonly | stale | error | narrowed to zero |
|---|---|---|---|---|---|---|
| Audit log | rows + toolbar + foot | `Primitive::Empty` portal default (fresh DB still has the login rows, so rare) | n/a: `writes: false`, only a disabled head action | `cache_ttl_s: None`: n/a | error block; in the split when audit-svc is down | search, topic or actor (incl. the FAKE actors, A5) with no match: toolbar stays; golden `table_empty_search` covers the search case (name from the skill, Not verified); the select case is H8 |
| Recent operator activity | 7 entries + tool | `ptpl:392` "No events" (= `bj:216`); tool hidden unless `when_empty` (mockup hides tools outside ok/stale, `bj:145`): `when_empty: false` | n/a | n/a | error block in the attention grid | n/a |

URLs (audit log panel): `PageDecl.params = ["actor", "topic", "time"]` (search travels as `q`, `PanelQuery.search`). Search, select and pager URLs must build from `Ctx.query` without `cursor`/`before` (`tbl:275` search_url uses `ctx.query`; the select URL "without this filter's key", `tbl:50`). A select change keeps `q` and the other filter, drops the cursor; the pager keeps all three filters and `q`. X5 link: `Link::Page{module "audit", page PAGE, params {actor: <login>}}` from both panels. The dashboard panel builds no URL of its own besides the tool link.

## 3. `writes` and every ActionDecl

`writes: false`. `invoke` answers `Error::forbidden("not implemented yet")` for every id (as scheduler).

| id | Point | Label / icon | order | Emphasis | danger | implemented | then |
|---|---|---|---|---|---|---|---|
| `log.export` | `PageHead(PAGE)` | "Export" / `download` | 10 | Default | no | **false** | `Invoke` (A10) |
| `log.open` | `PanelTool(<dashboard id>)` | "Audit log" / `right` | 10 | Ghost | no | true (a link) | `Link(Link::Page{audit, PAGE, {}})`, `trailing_icon: true` |

New `todo` items in 24.a1 (`### Not-implemented actions` / a new `### Fake data and actions of the audit port`): "Audit log export"; the FAKE actor options (A5); a styling item for the inline styles H4/H6 (standing fidelity decision).

## 4. Contributions, kinds and `order`

| Contribution id | Point | order | min_role | readonly | cache_ttl_s |
|---|---|---|---|---|---|
| `log` | `Page(PAGE)` | 10 | Viewer | None | None |
| `activity` | `DashboardPanel` | `dashboard_panel::AUDIT` = 10 (new module in `ord`) | Viewer | None | None |

- No entity kind, no row menu, no search provider, nav counter, tile or purges (audit tombstones nothing).
- Nav: `platform_nav::AUDIT` (`ord:10`).

## 5. Hand-check section

| # | Element | Check |
|---|---|---|
| H1 | Crumbs "Platform / Audit log"; title and subtitle text | live look |
| H2 | Export disabled with "Not implemented yet" | live look |
| H3 | Page panel has no head (empty title) | unit test on the manifest + live look |
| H4 | Search placeholder text; width 260 (mockup) vs 220 (`ptpl:216`) | declared in the step report (attribute) |
| H5 | "Topic: all" matches; "Actor: anyone" vs "Actor: all" (`ptpl:217`) | declared (text) |
| H6 | Action `font-weight:500`, Target `font-size:11.5px` inline styles | declared (attribute) |
| H7 | Foot text vs "n of 1,204 entries today"; page label "1 / N" | unit test on `PageInfo`; live look |
| H8 | Actor select narrowed to zero (a FAKE actor) keeps the toolbar | live look |
| H9 | Sort arrow on Time only | unit test (`Column.sorted` only on Time) |
| H10 | X5: an operator actor links to `/admin/p/audit/PAGE?actor=<login>`, and that URL narrows the table | unit test (link) + live look |
| H11 | Per-topic derivation (A3) for each of the 16 topics, and a non-object or missing-key payload draws "—" without failing the panel | unit test (SQL against local PG) |
| H12 | Time format (A9) | unit test |
| H13 | Dashboard: seven newest operator rows, HH:MM, meta text, icon and tone rules | unit test + live look |
| H14 | "Audit log →" ghost with trailing icon, hidden on empty | live look |
| H15 | Both topologies: page and dashboard panel draw through admin-svc → audit-svc | live look (split) or §9 |

## 6. Gaps and decisions

Plan-settled (not re-opened): Export `implemented: false` + todo (St1/O-3); actor filter and total are an exact unindexed `count(*)` over `audit.log` with a comment naming the missing index (`plan:2465-2467`, `gaps:95-98`); the mockup's `scheduler` and `apikey:…` actors have no source and are FAKE; operators are X5 user cells linked to the actor-filtered page, `system`/`scheduler`/`apikey:<name>` plain tags (`plan:1926-1927`); "Recent operator activity" is a `DashboardPanel` (`plan:1919`).

**A1. Page id and route.** (a) `PAGE = "log"`, `/admin/p/audit/log` (noun-per-page, as `keys`, `schedules`). (b) `PAGE = "audit"`. The apikeys "Requests by this key" link already names an audit page (`modules/apikeys/src/admin.rs:796`, Not verified which id). **Recommended: (a)**, and 24.a1 re-points the apikeys link if it names another id.

**A2. Rows the page lists.** (a) Every `audit.log` row (all 16 topics), as the subtitle says and `ds:156-163` lists. (b) `admin.action` only. **Recommended: (a)**: the ledger is the page; AU3 already expects domain topics on it (`sp:5148-5153`).

**A3. Columns for the 15 non-operator topics** (payload fields from `api/*/events/src/lib.rs`). Topic = stored `topic` (real vocabulary, D4). For `admin.action`: Actor = `actor` (X5 user cell), Action = `action`, Target = `target`, Detail = `detail`. Otherwise:
- Actor: (a) the `system` tag on every non-operator row (the module emitted it); (b) the acting player as a `player:<id>` tag where the payload names one (`creator_id`, `actor_id`, `requester_id`).
- Action: the topic's part after the first dot (`created`, `member_joined`, `changed`).
- Target: the payload's subject as `kind:id` (`player:<player_id>`, `character:<character_id>`, `group:<group_id>`, `match:<match_id>`, `friendship:<edge_id>`, config `<namespace>/<key>`).
- Detail: a short SQL summary per topic (wallet `{delta:+} {currency} · {reason}`, config `{operation} · rev {revision}` **never the value**, group role, friend handles, match winner/loser), "—" when a key is missing.
- **Recommended: (a)** for Actor (truthful, keeps the filter list small) and the Action/Target/Detail rules above, all in SQL (one `CASE topic`), each pinned by H11.

**A4. Topic select options.** (a) Static: the 16 `DURABLE_TOPICS` (`al:47-64`). (b) `SELECT DISTINCT topic` (unindexed). **Recommended: (a)**: real vocabulary, no scan; a drift test against `DURABLE_TOPICS` is free (same crate).

**A5. Actor select options and the FAKE actors.** Options: `SELECT DISTINCT payload->>'actor' … WHERE topic='admin.action'` (unindexed, bounded `LIMIT 50`, comment names the missing index), then `system`, then `scheduler` and `apikey:<name>` as `// FAKE:` options (no row has them, so they narrow to zero: H8). FAKE apikey name: a constant (`apikey:ak_c4a8` is mockup vocabulary; real keys are named by `name`, `ds:20-24`). (a) As described, one todo item. (b) Drop the two FAKE options (declared deviation). **Recommended: (a)**, per the plan's "are FAKE"; no fake ledger rows are ever drawn.

**A6. "Time: today" chip and "entries today".** The portal draws a non-Select filter as a chip "Time: any" until the URL carries `time` (`ptpl:218`). (a) A `Range` chip that narrows nothing; the table lists all retained rows (≤ 30 d) and the total counts them; "today" text declared. (b) Default to today (`at >= date_trunc('day', now())`) with the chip still "any": the chip would lie. **Recommended: (a)**; H7.

**A7. Foot count wording.** The standing rule gives `item_noun` a `Noun{entry, entries}` → "1,204 entries · showing 1–25" (`tbl:214-215`); without it "25 of 1,204" (`tbl:212`), nearer the mockup but a bare count. **Recommended: `item_noun`** (standing rule), wording declared (H7).

**A8. Count cost.** Exact `count(*)` per filter combination over ≤ 30 days of rows, unindexed except on `at`. Settled by the plan; recorded so the review does not re-raise it.

**A9. Time format.** Mockup HH:MM:SS (a "today" view). Rows span the retention window. (a) `HH24:MI:SS` when the row's UTC day is today, else `Mon DD HH24:MI`, both in SQL. (b) Always `YYYY-MM-DD HH24:MI:SS`. **Recommended: (a)**: the mockup's format where it applies, unambiguous elsewhere.

**A10. Export shape.** **Recommended:** `Then::Invoke`, `implemented: false` (scheduler S11 precedent: a disabled control never opens); the todo item names the real shape (a download carrier, O-3).

**A11. Untitled page panel.** The mockup panel has no head (`pp:119`, `bj:146`). `gaps:79-81` says the 13e6 wrappers fit "one bare panel"; whether an empty `ContributionDecl.title` suppresses `.panel-head` is Not verified. (a) Empty title, if the renderer draws no head. (b) If it draws an empty head: declared deviation, not a contract change (frozen vocabulary). **Recommended: (a)**, checked first thing in 24.a1 (H3).

**A12. Dashboard rows.** (a) `admin.action` rows only, newest 7 ("operator activity"; `ds:45`). (b) Every topic, as the page. **Recommended: (a)**. The dashboard range (`rt:420-424`) is ignored: the mockup shows the latest seven whatever the range.

**A13. Dashboard meta line.** The mockup draws "target · detail" as plain text; `TimelineEntry.target` draws a linked cell (`til:145-160`) and needs an `EntityRef`, while `admin.action` targets are free strings (`session`, `ip:…`, `user:<login>`, empty). **Recommended:** `target: None`, `detail = "{target} · {detail}"` (just `detail` when the target is empty).

**A14. Dashboard icon and tone.** Mockup rules are keyed on its invented topics. (a) Icon from the action's module prefix: `wallet` → `coins`, `core.events` → `zap`, `login-*`/`logout` → `user` if the icon set has it (else `activity`), else `activity`; tone `Err` when `detail` starts with `failed:`, `unknown:` or `rejected:`, or the action is `login-locked`. (b) Copy the mockup literally ("force"/"paused" in the action). **Recommended: (a)**; check each icon exists in `ptpl`'s icon set.

**A15. Splitproof part.** See §9. **Recommended:** a short `24t` (owner decides).

## 7. Accept-file plan

Out of this plan (owner, 2026-11-24): no accept file, no crawl, no `PAGE_MAP` change (`tools/admindiff/src/pages.rs:439` stays `Route::Pending`).

## 8. Other files the parts touch

- 24.a1: `modules/audit/Cargo.toml` (`controlplane` to `[dependencies]`), `al` (`mod admin; mod store;`, slot in `init`), new `modules/audit/src/admin.rs`, `store.rs`, `admin_tests.rs`; `todo` items; `docs/todo/audit.md` / `audit-done.md` (AUDIT-02 obsolete); `cmd/audit-svc` comments (§0.3); the apikeys link if A1 changes it.
- 24.a2: `ord` (new `dashboard_panel` module), `docs/reference/public-api-baseline/controlplane.txt`, `tools/admincheck/src/testdata/drawn_orders.txt` (a `dashboard_panel | audit | activity | 10` line; the label is `drawn_orders.rs:47`), the dashboard contribution and its reader in `admin.rs`/`store.rs`, `admin_tests.rs`.

## 9. Splitproof (proposal; owner decides)

- Today: `[AU3]` GETs the retired `/admin/audit-log` (`sp:5148-5153`) and is expected red (`red:34`); the plan re-points it in 25h1 (`plan:3322`). `[AD5]` checks the trail lands in `audit.log` (`sp:5111-5117`). Nothing asserts the dashboard panel.
- Proposed `24t` (`[opus]` test-author, proof-auditor audits), `audit_admin_checks.rs`, split first:
  - AU3 re-pointed now: the session GETs `/admin/p/audit/<PAGE>` through gateway → admin-svc → audit-svc: 200, the panel is not the error block, and the body holds a `character.created` or `player.registered` row; its `red` line is removed in the same commit.
  - X5 filter: `?actor=proofadmin` (the AD6a login) answers rows whose actor is only `proofadmin`, and its total is ≥ 1.
  - Dashboard: `/admin` draws "Recent operator activity" with a `proofadmin` entry.
  - Break run: admin-svc without its `audit` peer draws the audit panel's error block, page still 200.
- Alternative: leave AU3 to 25h1 and rely on H15 live looks.

## 10. Part sizes (estimate)

| Part | Content | Files | Tool calls |
|---|---|---|---|
| 24.a1 | store readers (page list with A3 derivation, count, actor options), `admin.rs` (manifest `writes: false`, nav, page, Export disabled, `invoke` refusal), slot + `Cargo.toml`, `admin_tests.rs`, todo items, AUDIT-02, svc comments, apikeys link (A1) | 9-11 | ~60 |
| 24.a2 | `dashboard_panel` order constant + public-API baseline, `drawn_orders.txt`, activity reader + contribution + tool link, tests | 6 | ~35 |
| 24.b | no template change expected (generic macros); only if H3 or H13 show a template need; otherwise no commit | 0-2 | ~10 |

24.a1 is at the Na1 ceiling (`plan:1955`, 7 files): split it into 24.a1a (store + derivation + tests) and 24.a1b (provider, slot, todo, comments) if the orchestrator wants smaller tasks.

## Not verified

- The audit page id the apikeys link names (`modules/apikeys/src/admin.rs:796`, cited by `todo` ADMIN2-60, not read).
- Whether an empty `ContributionDecl.title` draws no `.panel-head` (A11).
- Whether a `FilterKind::Range` chip ever shows a value and how `PanelQuery.params` carries it (`rt:632-639` cited from `gaps`, not read).
- That golden case `table_empty_search` exists (named by the skill).
- Whether the icon set has `download`, `right`, `coins`, `zap`, `user`.
- Whether `drawn_orders.txt` also lists nav entries (only `drawn_orders.rs:47` was read).
- `modules/audit/src/tests.rs` contents beyond the grep hits; whether a provider test can share its `testdb` setup.
- Login failure events other than `login-locked` (only the four emit sites were read).
- LSP was not used; anchors come from reads and scoped grep. No cargo, fleet, splitproof or admincheck run.

## Owner decisions

Decided 2026-11-24 (owner: "ok z rekomendacja"), all as recommended:

1. A1: page id `log`. The apikeys link already names `log` (`modules/apikeys/src/admin.rs:816-819`): no re-point.
2. A2: every `audit.log` topic on the page.
3. A3: (a) `system` actor tag; topic verb, payload subject and per-topic SQL summary; config value never shown.
4. A4: the 16 static `DURABLE_TOPICS`.
5. A5: (a) operator logins + `system` + FAKE `scheduler` and `apikey:…` options, one todo item.
6. A6: (a) inert Time chip, no "today" narrowing.
7. A7: `item_noun` foot.
8. A9: (a) `HH:MM:SS` today, `Mon DD HH:MM` older.
9. A10: Export `Then::Invoke`, `implemented: false`.
10. A11: (a) empty title, checked first in 24.a1a.
11. A12-A14: (a) as recommended.
12. A15: `24t` as in §9 (AU3 re-pointed now, X5 filter, dashboard, break run).
13. Parts: 24.a1 is split into 24.a1a (store readers, A3 derivation, their tests) and 24.a1b (provider, slot, `Cargo.toml`, todo items, AUDIT-02, svc comments).
