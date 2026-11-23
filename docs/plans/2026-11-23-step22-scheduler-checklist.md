# Step 22 (scheduler): checklist

Read-only extraction per `.agents/skills/mockup-implementation/SKILL.md` §1, on the layout of `docs/plans/2026-11-23-step21-apikeys-checklist.md`. HEAD at extraction: `f95e1a73`. Mockup page: `UILayout/redesign/scheduler.html` (boots `PAGES.boot("scheduler")`). Scope: `plan:2397-2403` (Step 22: "One a (Na1 with Na3)"), the Na1/Na3 template (`plan:1946-1973`), and the owner decisions to be recorded in §11.

Builder lines scheduler owns:
- Page `P.scheduler` `pp:74-85`. Crumbs Platform / Scheduler (`pp:75`). Head: title "Scheduler", subtitle "Cron and one-shot schedules that call module operations.", **no head action** (third argument `""`, `pp:83`). Layout `div.stack` holds Schedules, then Recent fires (`pp:83`). No `meta.after`.
- Schedules panel (`pp:77-80`): `pg("scheduler","DataTable")`, title "Schedules", meta = schedule count, `flush`, tool `btn sm` `plus` "Schedule" with **no onclick**. Table `pager: false`; no search, filters or total. Columns:
  - Id: `span.mono` with inline `font-weight:500`;
  - When: `span.mono` cron text;
  - Target: `span.tag`;
  - Next fire: `span.dim`;
  - Last: `span.row` = `status(last)` badge + `span.muted` time;
  - Enabled: `span.toggle` / `span.toggle.on`.
- Row button (`pp:80`): **not** `rowMenuBtn`. A `btn sm icon ghost` "more" whose onclick only toasts "Fire now · Edit · Delete". The mockup defines no `schedule` menu and no modal.
- Recent fires panel (`pp:81-82`): `pg("scheduler","Timeline")`, title "Recent fires", `flush`, tool = `span.badge.plain` "24 h" (no button). Entry: `span.when` HH:MM; body `span.mono` schedule id, plus `badge("failed","err")` when the result is not ok; `div.meta` "{dur} · {note}"; icon `check`/`alert`, tone `ok`/`err` (`bj:192-200`). Empty: `stateBlock("empty", "No events", "Nothing recorded in this range.")` (`bj:193`).
- Registry: nav `reg:28` (platform, id scheduler, "Scheduler", icon clock, no counter). Attention `reg:62` (module scheduler, warn, clock, "storage.gc failed at 03:30", "timeout: storage-svc deadline exceeded.", href scheduler.html, action "Logs"). No scheduler search provider in `reg` [grep "schedule" over `UILayout/redesign/assets`].
- Fake rows: schedules `dj:179-186` (six rows: cron strings, one `fail`, one disabled), fires `dj:187-194` (six entries, one `fail`).
- Out of scope, with reasons:
  - `dj:108` services row `scheduler-svc`: portal health FAKE (`fk:51`), another panel.
  - `dj:160` audit row (actor scheduler, action fire): Step 24; `scheduler.fired` is not audited.
  - `dj:201` config row "by scheduler": Step 23.

Abbreviations: `pp` = `UILayout/redesign/assets/pages-platform.js`, `reg` = `…/registry.js`, `dj` = `…/data.js`, `bj` = `UILayout/design-system/components/bundle.js`, `sl` = `modules/scheduler/src/lib.rs`, `st` = `modules/scheduler/src/tests.rs`, `ev` = `api/scheduler/events/src/lib.rs`, `cp` = `core/controlplane/src/lib.rs`, `ord` = `core/controlplane/src/order.rs`, `fk` = `core/admin/src/fakes.rs`, `ppv` = `core/admin/src/portal_provider.rs`, `pan` = `core/admin/src/engine/panels.rs`, `ptpl` = `core/admin/src/templates/primitives.html.tmpl`, `app` = `core/app/src/lib.rs`, `aad` = `modules/apikeys/src/admin.rs`, `al` = `modules/apikeys/src/lib.rs`, `pages` = `tools/admindiff/src/pages.rs`, `pol` = `tools/conformance/src/policy.rs`, `dro` = `tools/admincheck/src/testdata/drawn_orders.txt`, `todo` = `docs/todo/admin-second-pass.md`, `gaps` = `docs/research/2026-11-21-0848-admin-ports-21-24-contract-gaps.md`, `plan` = `docs/plans/2026-10-22-1759-admin-control-plane-redesign-plan.md`.

Method: targeted reads with line numbers; scoped ripgrep marked **[grep]** (a lower bound). No LSP pass, no cargo, `admindiff mock`, crawl, diff, admincheck or splitproof.

## 0. Required checks

**0.1 What the store holds (confirmed).**
- One table: `scheduler.schedules(name text PK, interval_seconds bigint NOT NULL CHECK (> 0 AND <= INTERVAL_SECONDS_CEILING), last_fired timestamptz NOT NULL DEFAULT to_timestamp(0))` (`sl:151-158`; ceiling const `sl:132`).
- Seeded rows (`sl:159-176`): `audit-prune`, `accounts-sessions-prune`, `notifications-prune`, `mail-prune`, `groups-prune` (86400 s each), `player-purge` (300 s). splitproof adds `proof-tick` (2 s) (`tools/splitproof/src/main.rs:5155`). **There is no `storage.gc` schedule.**
- `last_fired` and the interval both exist, so "next fire" = `last_fired + make_interval(secs => interval_seconds)` is computable in SQL with `now()` (no Rust clock). A never-fired row has `last_fired = 1970-01-01` and is due at once.
- No target, last result, duration, note, per-schedule enabled flag, cron expression or fire history column. Each fire only bumps `last_fired` and emits `scheduler.fired{name}` in the same tx (`sl:483-512`); the event carries only `name` (`ev:22-25`), `MinRetention{days: 7}` (`ev:34`).
- Global gate: `SCHEDULER_ENABLED` (default true) read in `init` (`sl:659-664`) into `self.enabled`; per process.
- **Legacy-row hazard:** `make_interval` errors the whole SELECT on an out-of-range row; `DUE_SQL` guards with `interval_seconds BETWEEN 1 AND {CEILING}` (`sl:191-198`), pinned by `st:257-276` and `st:367`. Any page read that computes next fire must carry the same guard (in a `CASE`, so the row still lists).

**0.2 Reads and RPCs today (confirmed).**
- None. The only SQL is the due scan (`sl:291-316`), the re-check (`sl:204-210`) and the bump (`sl:483-512`). No `store.rs`, no `Service`, no `api/scheduler` trait (the api crate is events only: `api/scheduler/events`).
- No `admin.rs`, no `controlplane` dependency, no `PROVIDER_SLOT`, no edge contribution (`modules/scheduler/Cargo.toml`; [grep "edge|admin|controlplane" in `sl`]: comment hits only). The old admin was razed in `b8435c9a` (step 1; `git log -S adminData -- modules/scheduler`).

**0.3 Wiring, both topologies (confirmed by read; nothing run).**
- Pattern from Step 21: `ApiKeys::init` contributes `Arc<dyn controlplane::Provider>` to `controlplane::PROVIDER_SLOT` (`al:307-308`); `modules/apikeys/Cargo.toml:36` adds `controlplane`.
- Monolith: `cmd/server/src/lib.rs:56` hosts `scheduler::Scheduler`; the in-process portal collects the slot.
- Split: `cmd/scheduler-svc/src/main.rs` builds an edge server and passes it to `app::run`; `app:1032-1035` registers the `cp.*` face from `ctx.contributions(PROVIDER_SLOT)` **unconditionally** on every edge process. admin-svc already dials scheduler: `peer_face("scheduler", wiring, "127.0.0.1:9005")` (`cmd/admin-svc/src/lib.rs:31`), `with_peer("scheduler", SCHEDULER_EDGE_ADDR)` (`cmd/admin-svc/src/main.rs:36`).
- So 22a changes no `cmd/*` file: `modules/scheduler/Cargo.toml` (+`controlplane`), `sl` (`mod admin; mod store;` and the contribution in `init`), new `admin.rs`, `store.rs`, `admin_tests.rs`.
- **Stale prose** (lead, not evidence): `cmd/scheduler-svc/src/main.rs:3,27,34`, `cmd/scheduler-svc/src/lib.rs:8-9` and `cmd/scheduler-svc/Cargo.toml:9-10` say scheduler contributes a read-only `admin.adminData` "Schedules" face; nothing does at HEAD. After 22a it serves `cp.*`; the comments should say so (22a or a docs sweep).
- Order constant: `platform_nav::SCHEDULER = 30` (`ord:8`) exists. Attention: `dashboard_attention::PORTAL_SCHEDULER = 50` (`ord:32`).

**0.4 The portal fake to move (confirmed).**
- `fk:178-188`: `// FAKE: the scheduler stores no run failures.` `attention_scheduler()` → one `AttentionItem{Severity::Warn, "storage.gc failed at 03:30", "timeout: storage-svc deadline exceeded.", link page("scheduler","scheduler"), "Logs", since "03:30"}`.
- `ppv:20` const `ATTENTION_SCHEDULER = "attention.scheduler"`; `ppv:93-99` its `ContributionDecl` (`Point::DashboardAttention`, order `PORTAL_SCHEDULER`, title "Scheduler runs", icon `clock`); `ppv:116` its `panel` arm.
- `dro:11` `dashboard_attention | portal | attention.scheduler | 50`: an admincheck golden that changes with the move.
- `todo:52-55` item ADMIN2-07 "Scheduler failure attention item" cites `fakes.rs:183` (title is at `fk:182`); its anchor moves to `modules/scheduler/src/admin.rs`.
- No other reference [grep `attention.scheduler|attention_scheduler|PORTAL_SCHEDULER|Scheduler runs|storage.gc` over `core/admin`, `core/controlplane`, `tools/admindiff`, `tools/splitproof`, `tools/admincheck`]. The `fk` unit test (`fk:222-226`) does not touch it.

**0.5 `writes: false` is safe with disabled actions (confirmed).**
- Na1 template: the provider is `writes: false` (`plan:1947-1949`); scheduler is a non-writer (Na3 folded in, `plan:1972-1973`).
- `is_readonly` (`pan:85-91`) marks a panel read-only only for a `readonly` decl, a `Form` body, or an **implemented** action that `write_refusal` refuses; an `implemented: false` action is refused by `admit` first. So disabled tools and row actions draw no read-only chip or banner.
- `pol:60` requires a `cp.invoke <module>` CapCase only for `writes: true` manifests: no probe and no CapCase for 22.

**0.6 Row kind without an entity header.** As Step 21 §0.7: kind `schedule` with `RowAction("schedule")` actions and no `EntityHeader` draws `td.act` with the menu button and a non-clickable row (O-6, `plan:3124-3125`).

**0.7 Timeline carrier (confirmed).** `TimelineEntry{at, actor, title, verb, target, summary, badges, detail, icon, tone}` (`cp:1293-1308`). The template draws `verb` as `span.mono`, then `summary`, then each badge cell; `detail` in `div.meta` (`ptpl:264`). So id → `verb`, "failed" → `badges: [Badge "failed" Tone::Err]`, "{dur} · {note}" → `detail`. Panel "24 h" → `PanelBody.tag` ("a plain badge in the tools row", `cp:1487-1488`).

## 1. Element table

### Page head and shell

| Element | Mockup line | Carrier | Status |
|---|---|---|---|
| Nav "Scheduler", clock, Platform | `reg:28` | `NavEntry{section "platform", id "scheduler", page PAGE, order platform_nav::SCHEDULER, counter None}` | ok |
| Crumbs Platform / Scheduler | `pp:75` | portal crumbs | hand-check H1 |
| Title, subtitle | `pp:83` | `PageDecl.title/subtitle` | ok; subtitle text: S7 |
| No head action | `pp:83` | none | ok |
| Layout Schedules then Recent fires in `div.stack` | `pp:83` | `Layout::Stack([schedules, fires])` | ok |

### Schedules panel

| Element | Mockup line | Carrier | Status |
|---|---|---|---|
| Title "Schedules", `flush` | `pp:77` | `ContributionDecl.title` | ok |
| Meta "6" | `pp:77` | `PanelBody.meta` = row count | REAL `count(*)`; "0" at zero |
| Tool "+ Schedule" `btn sm` | `pp:77` | `PanelTool("schedules")` `schedule.create`, `implemented: false`, `when_empty: true` | disabled + todo; mockup has no onclick |
| Column Id, mono, weight 500 | `pp:79` | `CellKind::Mono` = `name` | REAL; the inline weight is an attribute (H3) |
| Column When, mono cron | `pp:79` | `CellKind::Mono` = interval text | REAL interval, not cron (S3) |
| Column Target `span.tag` | `pp:79` | `CellKind::Ref` | **data: FAKE** (S4) |
| Column Next fire `span.dim` | `pp:79` | `CellKind::Dim` | REAL, SQL `last_fired + interval` (S5) |
| Column Last: badge + muted time in `span.row` | `pp:79` | `Cell{Badge, value ok/fail, tone, row: true, more: [Muted HH:MM]}` | result **data: FAKE**; time REAL `last_fired` (S6) |
| Column Enabled `span.toggle(.on)` | `pp:79` | `CellKind::Toggle` value `"true"`/`"false"` (`cp:1119-1120`) | **data: FAKE** (plan; S8) |
| Row button "more" | `pp:80` | `Row.entity = schedule:<name>` + `RowAction("schedule")` | ok (§0.6); mockup button only toasts → `EXCLUDED_MENUS` (§7) |
| No pager, no total | `pp:78` | `PageInfo{next_cursor: None, total: None}` | ok; read capped (S9) |
| Row order | data order | `name COLLATE "C"` (as `DUE_SQL`) | rule: real stable order |

### Schedule row menu (`schedule`, no mockup menu)

| Element | Mockup line | Carrier | Status |
|---|---|---|---|
| "Fire now" | `pp:80` (toast text) | `schedule.fire`, `RowAction("schedule")`, `implemented: false` | disabled + todo |
| "Edit" | `pp:80` | `schedule.edit`, `implemented: false` | disabled + todo |
| "Delete" | `pp:80` | `schedule.delete`, `danger: true`, `implemented: false` | disabled + todo |

### Recent fires panel

| Element | Mockup line | Carrier | Status |
|---|---|---|---|
| Title "Recent fires", `flush` | `pp:81` | `ContributionDecl.title` | ok |
| Tool "24 h" plain badge | `pp:81` | `PanelBody.tag = "24 h"` | ok |
| Entry time HH:MM | `pp:82` | `TimelineEntry.at` (RFC 3339) | per S2 |
| Schedule id `span.mono` | `pp:82` | `verb` | REAL name (S2) |
| "failed" err badge | `pp:82` | `badges [Badge "failed", Tone::Err]` | **data: FAKE** result |
| Meta "{dur} · {note}" | `pp:82` | `detail` | **data: FAKE** |
| Icon check/alert, tone ok/err | `pp:82` | `icon`, `tone` | follows the FAKE result |
| Timeline flags | n/a | `days: false`, `date_only: false`, `live: false`, `cursor_before: None` | ok |

### Dashboard attention (moved from portal)

| Element | Mockup line | Carrier | Status |
|---|---|---|---|
| "storage.gc failed at 03:30" item, warn, clock, "Logs" | `reg:62` | scheduler's `Point::DashboardAttention` contribution at order 50 → `AttentionItems` | **data: FAKE** moved verbatim (S10); link → the new page id (S1) |

## 2. States per panel, and URLs

| Panel | ok | empty | readonly | stale | error | narrowed to zero |
|---|---|---|---|---|---|---|
| Schedules | rows + meta + tool | `Primitive::Empty` portal default; meta "0"; tool kept (`when_empty`) | n/a: `writes: false` but no implemented action (§0.5) | `cache_ttl_s: None`: n/a | error block | n/a: no search or filter |
| Recent fires | entries + tag | `Primitive::Empty` (mockup "No events / Nothing recorded in this range.") | n/a | n/a | error block | n/a |
| Attention | one item | none in the fixed FAKE | n/a | n/a | the portal's "scheduler module unavailable" row (`ptpl:412`) when scheduler-svc is down: new in split, was portal-local | n/a |

URLs: none. No search, filters, tabs or pager; no `PageDecl.params`. The row menu is fetched from the portal's menu route.

## 3. `writes` and every ActionDecl

`writes: false` (§0.5). Every action `implemented: false` (plan "all writes disabled"); `invoke` answers `Error::forbidden("not implemented yet")` for every id, as `aad:929` and `modules/mail/src/admin.rs:598`. `min_role: Operator` on each.

| id | Point | Label / icon | order | Emphasis | danger | implemented | then |
|---|---|---|---|---|---|---|---|
| `schedule.create` | `PanelTool("schedules")` | "Schedule" / `plus` | 10 | Default | no | **false**, `when_empty: true` | `Form` (no fields designed; S11) |
| `schedule.fire` | `RowAction("schedule")` | "Fire now" / `zap` (S11) | 10 | Default | no | **false** | `Invoke` |
| `schedule.edit` | `RowAction("schedule")` | "Edit…" / `edit` | 20 | Default | no | **false** | `Form` |
| `schedule.delete` | `RowAction("schedule")` | "Delete…" / `trash` | 30 | Default | **yes** | **false** | `Confirm` |

New `todo` items in 22a (under ### Fake data / Not implemented, as the file's sections hold them):
- the four disabled actions (one item, or one each per the file's convention);
- FAKE target, last result, per-schedule enabled, fire result/duration/note (one item each or one "scheduler run outcomes" item; S12);
- ADMIN2-07 re-anchored to `modules/scheduler/src/admin.rs` (not closed: still FAKE).

## 4. Contributions, kinds and `order`

| Contribution id | Point | order | min_role | readonly | cache_ttl_s |
|---|---|---|---|---|---|
| `schedules` | `Page(PAGE)` | 10 | Viewer | None | None |
| `fires` | `Page(PAGE)` | 20 | Viewer | None | None |
| `attention.runs` (S10) | `DashboardAttention` | `dashboard_attention::SCHEDULER` = 50 (rename of `PORTAL_SCHEDULER`, `ord:32`) | Viewer | None | None |

- Entity kind `schedule` ("Schedule", `clock`), no `EntityHeader`.
- No search, nav counter, tile, purges (scheduler subscribes to nothing).
- Nav: `platform_nav::SCHEDULER` (`ord:8`).
- Removed from portal: `ATTENTION_SCHEDULER` (`ppv:20,93-99,116`), `attention_scheduler` (`fk:178-188`). The `Link::Page` helper `fk:150-152` stays (other fakes use it).

## 5. Hand-check section

| # | Element | Check |
|---|---|---|
| H1 | Crumbs "Platform / Scheduler" | live look |
| H2 | Schedules meta = row count | provider unit test |
| H3 | Id cell's inline `font-weight:500` (attribute; admindiff never compares attributes) | declared in the step report |
| H4 | When text ("every 24 h" vs cron) | live look; data difference |
| H5 | Next fire for a never-fired / overdue row, and with `SCHEDULER_ENABLED=false` (S5) | unit test (SQL against local PG) + live look |
| H6 | Last badge tone: ok → ok, fail → err (`STATUS_TONE`, `bj:98`); never fired → "—" | unit test |
| H7 | FAKE cells stable across reloads (`fake_hash`) | unit test |
| H8 | Disabled tool and menu items carry the "Not implemented yet" hint | live look |
| H9 | Dashboard attention item unchanged in text and order 50 after the move, in both topologies | live look; `dro:11` golden |
| H10 | A legacy out-of-range `interval_seconds` row lists without erroring the panel | unit test (as `st:367`) |
| H11 | Recent fires empty state text | live look (fresh DB) |

## 6. Gaps and decisions

See **Open gaps for the owner** at the end (S1-S13). Plan-settled (not re-opened):
- All writes disabled: every action `implemented: false` with a todo item.
- Target, last result, per-schedule enabled and fire history are FAKE via `controlplane::fake_hash` (`cp:213`), each marked `// FAKE:`.
- "Next fire" is `last_fired + interval` in SQL; no Rust clock.
- The `storage.gc` attention FAKE moves into the scheduler provider at order 50 with its todo item (X6).
- The row menu is kind `schedule` (O-6).

## 7. Accept-file plan (predicted; exact text needs a crawl)

`pages.rs` rows to change (22a):
- `pages:421`: `Page{page "scheduler", route Route::Pending, …}` → `Route::Mapped("/admin/p/scheduler/<PAGE>")` (S1).
- No `menu` entry: the mockup defines no `schedule` menu (the row button only toasts), so nothing goes to `EXCLUDED_MOCK_MENUS` (`pages:719-722` holds only Lore).
- `EXCLUDED_MENUS` (`pages:462-471`): add `("scheduler", "schedule", "the mockup draws no schedule menu: its row button only toasts 'Fire now · Edit · Delete' (pages-platform.js:80)")`, as the events `subscription` entry (`pages:463-467`).
- `EXCLUDED_MODALS` (`pages:481-…`), **if** the crawl counts a disabled `Form`/`Confirm` action as a drawn modal action (precedent: `mail/test-send` and the disabled wallet menu items are listed, `pages:482-486,507-…`; verify in 22a):
  - `("scheduler", "scheduler/schedule.create", "the mockup's + Schedule tool has no onclick, so the mockup has no modal (pages-platform.js:77)")`;
  - `("scheduler.excluded-menu.schedule", "scheduler/schedule.edit" | "scheduler/schedule.delete", "the mockup draws no schedule menu: its row button only toasts (pages-platform.js:80)")`.
- No `EXCLUDED_MOCK_MODALS` entry: the mockup page opens no modal.

Accept files (part b):
- **`scheduler.txt`** (new):
  - frame lines copied from `mail.txt:5-11` (Sign out form, Lore nav leftovers, Design-system section D3, theme toggle), counts and reasons re-derived; Platform now pairs Scheduler, so its "not ported yet" list keeps Config & flags and Audit log only;
  - Schedules rows: data difference (a fresh fleet has six seeded schedules, plus `proof-tick` after a splitproof run; names, intervals and fake values differ from `dj:179-186`); the "+ Schedule" disabled `btn` if its markup differs (template choice);
  - Recent fires entries: data difference (count and content per S2); a `span.badge.err` only where the FAKE says failed;
  - the "24 h" `span.badge.plain` vs the portal `tag` markup, if they differ (template choice);
  - `td.act` button: the mockup's has no `aria-label` / `data-menu-url`: attributes, not compared.
- **Every existing accept file with the `#ecbb9a` nav line** [grep "Scheduler" over `tools/admindiff/accept`: 14 files]: `index.txt:6`, `mail.txt:6`, `apikeys.txt:11`, `economy.txt`, `events.txt`, `players.txt:6`, `player.txt:6`, `player.characters.txt:7`, `player.inventory.txt:8`, `player.wallet.txt:8`, `player.friends.txt:10`, `player.groups.txt:10`, `player.inbox.txt:10`, `player.saves.txt:10`. Re-derive the `mock-only ×N a.nav-item#ecbb9a` line: Scheduler now pairs; drop it from the reason. `×{3,2}` / `×3` will change; the exact counts need the crawl.
- `index.txt`: no attention line exists for the scheduler item [grep "attention" in `index.txt`: none]; the moved item keeps order 50, so no new line is expected. A changed module order inside the attention list would show in the crawl.
- Part b sweep first: `rg -n "ecbb9a|Scheduler|scheduler" tools/admindiff/accept`.

## 8. Other files the move touches (22a)

- `ord:32` `PORTAL_SCHEDULER` → `SCHEDULER` (the constant is named by its contributing module, `ord:1-2`).
- `dro:11` → `dashboard_attention | scheduler | <id> | 50` (S10 names the id).
- `todo:52-55` ADMIN2-07: Fake anchor → `modules/scheduler/src/admin.rs`; text unchanged.
- `cmd/scheduler-svc` doc comments (§0.3): correct the `admin.adminData` claim to the `cp.*` face.

## 9. Splitproof

- No splitproof check asserts on a scheduler admin page or route [grep `p/scheduler|scheduler/|"Schedules"|"scheduler"` and `-i attention` over `tools/splitproof/src`: none]; `tools/splitproof/expected-red.txt` has no scheduler line. Nothing to re-point (unlike Step 21 D5).
- Scheduler splitproof today is event plumbing only: `[SC0]/[SC1]` (`main.rs:5152-5170`), `[SP1]/[SP2]` (`:5172-5186`), `[ML6]` (`:1595-1648`), the group/player purges (`:2544-2573`, `:2634-2667`). They write `scheduler.schedules` directly; 22a does not change the table, so they are not at risk.
- The plan names no `22t` (S13).

## 10. Part sizes (estimate)

| Part | Content | Files | Tool calls |
|---|---|---|---|
| 22a | `store.rs` reads (list with next fire, count), `admin.rs` (manifest, two panels, kind, nav, attention, FAKE cells, disabled actions, `invoke` refusal), `sl` slot + `Cargo.toml`, `admin_tests.rs`, portal removal (`fk`, `ppv`, `ord`, `dro`), `pages.rs`, todo items, svc comments | 10-11 | ~55 |
| 22b | crawls (monolith, split), `scheduler.txt`, nav-line sweep of 14 files | 15 | ~35 |

22a exceeds the Na1 "at most 7 files" target (`plan:1955`) because the move adds four core files; still one part by the plan's call (S13).

## Not verified

- No cargo, crawl, `admindiff mock`, diff, splitproof or admincheck. Every §7 line is a prediction.
- Whether a disabled (`implemented: false`) `Form`/`Confirm` action counts as a drawn modal action for `EXCLUDED_MODALS` (inferred from `mail/test-send` and wallet entries, not from the crawl code).
- Whether admincheck accepts a `writes: false` manifest that declares actions (only `readonly_writes` was located, `tools/admincheck/src/checks.rs:189-193`).
- How the portal formats `TimelineEntry.at` (HH:MM vs date) and a `Toggle` cell's markup vs the mockup `span.toggle`.
- The exact portal markup of a disabled `PanelTool` vs the mockup `btn sm`.
- Whether reading `scheduler.fired` rows from the durable log (S2 option c) has any sanctioned read API.
- LSP was not used; anchors come from reads and scoped grep.

## 11. Owner decisions

Owner 2026-11-23 ("ok zaczynaj"): every recommendation below is taken.

- S1 (a): `PAGE = "schedules"`, route `/admin/p/scheduler/schedules`; the moved attention link points there.
- S2 (b): one Recent fires entry per schedule whose `last_fired` is within 24 h (SQL `now()`), newest first; time and name real, result/duration/note FAKE.
- S3 (a): When = `every <n> <unit>` from `interval_seconds`, mono; data-difference line.
- S4 (a): Target = `fake_hash("scheduler.target", name)` over a fixed op-like list.
- S5 (a) + (i): Next fire = "due" when ≤ `now()`, else UTC `YYYY-MM-DD HH24:MI`, all in SQL; "—" for an out-of-range row; a warn `PanelBody.banner` when the provider's scheduler is disabled (declared, hand-check).
- S6 (a): Last = real HH:MM UTC from SQL, never-fired → `span.muted "—"` without badge; FAKE result from `fake_hash("scheduler.last", name)`, the same value S2 uses.
- S7 (b): subtitle "Interval schedules; each fire emits scheduler.fired for the modules that react to it."
- S8 (a): per-schedule Enabled FAKE via `fake_hash`, mostly on.
- S9 (a): no pager; read `LIMIT 101`, draw 100, foot note when more exist.
- S10 (a): the attention item moves verbatim (id `attention.runs`, "Scheduler runs", icon `clock`, order 50); deriving it from S6 is noted in ADMIN2-07.
- S11 (alternative): every disabled action (Fire now, Edit…, Delete…, + Schedule) is `Then::Invoke`, `implemented: false`; no form/confirm spec, no `EXCLUDED_MODALS` line. Check each icon exists; use an existing one otherwise.
- S12 (a): one "Scheduler run outcomes" item merged into ADMIN2-07, one "Scheduler writes" item.
- S13 (a): a 22t test step (below).

Parts (orchestrator, to keep each implementer task small):
- **22a1** (core-implementer, opus): scheduler store reads + `admin.rs` provider (manifest `writes: false`, Schedules and Recent fires panels, nav, kind `schedule` menu, disabled actions), slot registration + `Cargo.toml`, unit tests, `pages.rs` Mapped route + `EXCLUDED_MENUS` line, todo items, `cmd/scheduler-svc` stale comments.
- **22a2** (core-implementer, opus): the attention move: `core/admin/src/fakes.rs`, `core/admin/src/portal_provider.rs`, `core/controlplane/src/order.rs`, `tools/admincheck/src/testdata/drawn_orders.txt`, the scheduler provider's attention contribution, ADMIN2-07 anchor.
- **22t** (test-author, opus): page and attention checks in both topologies; proof-auditor audits.
- **22b** (mockup-implementer): templates only if needed, `scheduler.txt`, the nav-line sweep, after the orchestrator's crawls.

## Gaps as proposed (decided in §11)

**S1. Page id and route.** The portal FAKE links `page("scheduler","scheduler")` (`fk:184`).
- (a) `PAGE = "schedules"`, route `/admin/p/scheduler/schedules` (as apikeys `keys`, mail `outbox`); the moved attention link uses it.
- (b) `PAGE = "scheduler"`, keeping the fake's current link.
- **Recommended: (a)**, the same noun-per-page rule as K1 of Step 21.

**S2. Recent fires source.** Plan: fire history is FAKE. The store keeps each schedule's last fire (`last_fired`); the durable log keeps `scheduler.fired{name}` for at least 7 days (`ev:34`) but in the app-owned plane's table.
- (a) Fully FAKE entries from a fixed list (mockup-like names).
- (b) One entry per schedule whose `last_fired` is within 24 h (SQL `now()`), newest first: time and name REAL; result, duration and note FAKE via `fake_hash(name)`.
- (c) Real fire history from the durable log: a read outside the module's schema; not recommended without an owner decision on that coupling.
- **Recommended: (b)**: names that exist, no clock in Rust; still FAKE where the plan says. Owner to confirm it fits "fire history FAKE".

**S3. When column.** The store has an interval, not a cron.
- (a) `every <n> <unit>` from `interval_seconds` (`every 24 h`, `every 5 min`, `every 2 s`), mono, formatted in Rust from the number.
- (b) The raw seconds.
- **Recommended: (a)**; a data-difference line and H4.

**S4. FAKE target values.** Plan: FAKE.
- (a) `fake_hash("scheduler.target", name)` picks from a fixed list of op-like names (e.g. the mockup's `leaderboard.reset`, `mail.enqueue_batch`, `accounts.purge`…).
- (b) A fixed per-name table for the six seeded names (e.g. `audit-prune` → `audit.prune`), FAKE for others.
- **Recommended: (a)** (one rule, no hand list); the todo item says no target exists because a fire carries only the name.

**S5. Next fire text.** SQL: `CASE WHEN interval_seconds BETWEEN 1 AND <ceiling> THEN last_fired + make_interval(secs => interval_seconds) END`.
- Format: (a) `"due"` when ≤ `now()`, else `to_char(… AT TIME ZONE 'UTC', 'YYYY-MM-DD HH24:MI')`; (b) relative text as the mockup ("tomorrow 07:00") computed in SQL.
- Out-of-range legacy row: `"—"`.
- With `SCHEDULER_ENABLED=false` nothing fires: (i) a `PanelBody.banner` warn "Scheduler disabled on this process: no schedule fires" from the provider's own `enabled`; (ii) nothing.
- **Recommended: (a) + (i)**: deterministic text, one honest banner (not in the mockup → H5 / declared).

**S6. Last column.** Time is REAL (`last_fired`, HH:MM UTC via SQL `to_char`); result FAKE.
- Never fired (epoch): (a) `span.muted "—"` with no badge; (b) a FAKE result anyway.
- FAKE result: `fake_hash("scheduler.last", name)` → "ok" mostly, "fail" for a small share.
- **Recommended: (a)**, and the same hash feeds S2's entry result so the table and the timeline agree.

**S7. Subtitle.** "Cron and one-shot schedules that call module operations." is untrue (interval-only, emits an event, calls nothing).
- (a) Keep the mockup text. (b) "Interval schedules; each fire emits scheduler.fired for the modules that react to it."
- **Recommended: (b)**; hand-check H1 row, admindiff does not compare text.

**S8. Enabled toggle.** Plan: per-schedule enabled is FAKE.
- (a) `fake_hash` → mostly on. (b) All rows show the process's real `SCHEDULER_ENABLED` (truthful, uniform).
- **Recommended: (a)** per plan; (b) only if the owner prefers truth over shape.

**S9. Row cap.** Mockup `pager: false`; the table is code-seeded (six rows) plus test rows.
- (a) No pager; read `LIMIT 101`, draw 100, and a `foot` "first 100 schedules" when more exist. (b) Keyset pager on `name` (mockup has none).
- **Recommended: (a)**; no unbounded read.

**S10. The moved attention item.**
- Contribution id `attention.runs`, title "Scheduler runs", icon `clock` (unchanged from `ppv:93-99`).
- Content: (a) moved verbatim (`storage.gc`, a schedule that does not exist); (b) derived from S6's FAKE: the first schedule whose fake last result is "fail", none otherwise.
- **Recommended: (a)** for this step (the plan says "moves"; index diff stays as is); (b) noted in ADMIN2-07.

**S11. Disabled action shapes.** No mockup design beyond "Fire now · Edit · Delete" and "+ Schedule".
- Labels "Fire now", "Edit…", "Delete…", "Schedule"; icons `zap`/`edit`/`trash`/`plus` (check each exists in the icon set); `then` Invoke / Form (no fields) / Confirm (minimal `ConfirmSpec`) / Form.
- Alternative: every disabled action `Then::Invoke`, so no form or confirm spec is needed and no `EXCLUDED_MODALS` lines appear.
- **Recommended: the alternative** (disabled controls never open; least invented surface). Each todo item names the real `then` the second pass needs.

**S12. Todo granularity.**
- (a) One item "Scheduler run outcomes" (target, last result, enabled, fire result/duration/note), merged into ADMIN2-07, plus one "Scheduler writes" item for the four actions. (b) One item per FAKE and per action.
- **Recommended: (a)**: one missing feature (recorded runs) backs all the FAKEs.

**S13. Test part.** The plan lists only "One a" for Step 22; no existing splitproof check to re-point.
- (a) A `22t` `scheduler_admin_checks.rs`: GET the page 200 with both panels in both topologies (break run: admin-svc without its `scheduler` peer), and the dashboard still draws the attention item from scheduler-svc in the split.
- (b) No 22t; the live crawl (22b) in both topologies is the proof.
- **Recommended: (a)**: the move changes which process draws the attention item in the split, which is the at-risk topology.
