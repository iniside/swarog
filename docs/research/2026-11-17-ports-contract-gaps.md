# Steps 14–24: cross-port contract gaps, grouped (2026-11-17)

Read-only sweep before the remaining provider ports, so contract gaps are decided once.
Sources: the Step 14 checklist (`docs/plans/2026-11-17-step14-characters-checklist.md`,
G1–G11) and two read-only opus sweeps (Steps 15–20 player tabs; Steps 21–24 platform
pages). Every item is a lead with anchors from those sweeps (targeted reads; grep hits are
lower bounds); nothing was run. Abbreviations as in the Step 14 checklist (`pc`, `pp`,
`reg`, `bjs`, `cp`, `tmpl`, `rr`, `rt`, `md` = modal template).

## K. Contract and renderer gaps

| # | Mockup | Ports | Today | Proposal |
|---|---|---|---|---|
| K1 | `span.mono.muted` (`pc:127,132,141`, `pp:39,142,181`) | 9 (ADMIN2-18), 14, 15, 16, 19, 21 | `Mono` and `Muted` are separate (`tmpl:76,79`) | `CellKind::MonoMuted`; ADMIN2-18 closes and `events.txt` loses its gap lines |
| K2 | accent tone: `span.ico.accent` (`pc:144`, `pp:103`), `badge.accent.plain` (`pp:169`) | 15, 20, 23 | `Tone` is Ok/Warn/Err/Info (`cp:712-717`) | `Tone::Accent` |
| K3 | `td.wrap` (`bjs:161`; `pp:35,64,94,122,180`) | 11 (mail, check), 15, 21, 23, 24 | no wrap on `Column` (`cp:756-764`) | `Column.wrap: bool` |
| K4 | row menu on a row that is not `tr.clickable` (`pc:133,135`, `pp:35,40,80,143`) | 9 (accepted), 16, 17, 21, 22 | `Row.entity` drives both link and menu (`rr:1040-1045`) | portal makes a row clickable only when its kind has an `EntityHeader`; alternative `Row.link: bool` |
| K5 | several values in `span.row` (`pp:39,79,95`) | 21, 22, 23 | `Cell.more` draws bare siblings (`tmpl:88-90`) | `Cell.row: bool` |
| K6 | display toggle `span.toggle(.on)` in a cell (`pp:79,95`) | 22, 23 | toggle only as a form field (`tmpl:249`) | `CellKind::Toggle` |
| K7 | bold name `span[style=font-weight:500]` (`pp:35,39`) | 21 | `Text` is bare (`tmpl:82`) | `CellKind::Strong`, or a contract-gap accept |
| K8 | `a.mono` link (`pp:95`) | 23 | `a > span.mono` (`tmpl:79,85`) | template rule, no contract change |
| K9 | panel chrome: `div.panel-foot` text (`pp:36`), plain badge in tools (`pp:81`) | 21, 22 | foot only for Timeline (`tmpl:433`); note draws `span.muted` (`tmpl:414`) | `PanelBody.foot`, `PanelBody.tag` |
| K10 | timeline: bold title (`pc:144`), badge after text (`pp:82`), actor as a plain tag or linked user (`pc:10`), no day labels and date-only "when" (`pp:82,102-106`, `pc:56`), per-entry restore link (`pp:104-105`) | 20, 22, 23, 24 | `summary` plain, `actor` always an `EntityRef` link, day labels always on, always HH:MM (`rr:1201-1245`) | `TimelineEntry.title`, `.badges`, `.actor: Option<Cell>`, `Timeline.days` + `when` format; restore = contract-gap accept + todo |
| K11 | KPI tiles row inside an entity tab, one tile per currency (`pc:128-129`) | 15 | tab body is `div.stack` / `grid.g-2` (`shell:64`); one tile per contribution | `Point::EntityTabTile` + `Primitive::KpiTiles(Vec)`; alternative: one PropertyList of balances as a deviation |
| K12 | declared read-only with its own reason, `ro-chip`, lock icon on the tab (`pc:138-139`, `reg:73`) | 19 | readonly only from `writes:false` + a write affordance (`pn:85-94`); fixed banner text (`tmpl:420`) | `ContributionDecl.readonly: Option<String>` |
| K13 | error dot on a tab with no count (`reg:69`) | 15 | dot only from a failing count slot (`tmpl:395`) | portal-only `TabView.down` from `source_ok` |
| K14 | row-menu group note "unavailable" + disabled items when the module is down (`reg:101-104`); per-row disabled reason (`pp:11`) | 15, 21 | menu ignores `source_ok` (`rt:265-279`); no per-entity state (D7) | portal-only group availability; per-row reason stays a D7 deviation |
| K15 | page layout: `grid.g-21` then full width (`pp:183`) | 15 | `Layout` is Stack/Grid21/Grid2 (`cp:401-405`) | `Layout::Rows(Vec<Layout>)` |
| K16 | forms: Grant/Revoke `div.seg` (`pp:172`), `input.mono` and placeholder (`pp:25,98,99`), modal title (`pp:24`), reveal extra line (`pp:29`) | 15, 21, 23 | no form modes, `mono` only for Json (`rr:1325-1337`), no placeholder, title = action label (`rr:1824`), reveal hint replaces default text (`md:37`) | `Field.mono`, `Field.placeholder`, `FormSpec.title`, `SecretReveal.extra`; seg = an Enum field + contract-gap accept |
| K17 | search hit that links to a page (`reg:40,41`) | 15, 21 | `SearchHit.target` is an `EntityRef` (`cp:304-309`) | `SearchHit.link: Option<Link>` |
| K18 | singular at 1 ("1 character", "12 friends", "37 items") | 14, 16, 17 | `item_noun` is one plural (`cp:819-820`) | singular/plural pair |
| K19 | another module's name in a cell: friend user cell (`pc:135`), ledger player (`pp:181`), inventory owner character (`pc:132`), operator actor (`pp:122`) | 15, 16, 17, 23, 24 | modules hold uuids; fortress bars imports; `ref_cell` shows the id (`rr:253-267`) | `Point::EntityLabel{kind}` answered by the kind's owner, batched like the list-column `refs`; operators get a rule of their own (X5) |
| K20 | page state in the URL: selected config key, audit topic, apikey→audit link (`pp:15,91,95,116-117`); `select.select` filters (`pp:121`) | 21, 23, 24 | page route forwards only `q`, `state` (`cp:258-263`); filters are chips (`tmpl:190`) | `PageDecl.params` forwarded to `PanelQuery.params`; `FilterKind::Select` |
| K21 | bound on `PanelQuery.refs` | 14, 15 (every column provider) | `Opaque`, unbounded | `controlplane::MAX_PANEL_REFS`; portal never exceeds it, providers refuse before SQL |

## T. Tooling blind spots

| # | Blind spot | Ports | Proposal |
|---|---|---|---|
| T1 | entity tab panels never crawled or diffed (crawl follows the default tab only) | 14–20 | a tab view of a `FirstRow` page: `?tab=<id>` on both sides, own accept file |
| T2 | modals never compared (harness captures the page body; crawl never opens action URLs) | 9, 11, 15, 21, 23 | a modal target list: harness runs the mock's JS opener, crawl GETs the action's `hx-get` |
| T3 | row-menu bodies only status-checked | 13–21 | crawl saves menu fragments; harness renders `REG.userRowActions` / page menus |
| T4 | the mockup draws wallet only down or stale (demo states); the portal draws ok | 15 | harness option `force: ok` (`bjs:131,177`) + a declared deviation for hard-coded demo markup + goldens for ok shapes |
| T5 | `select > option` lists not collapsed | 24 | collapse `option*` |
| T6 | client-side default selection (config) | 23 | a JS prelude per page plus a matching portal route param |
| T7 | every K item needs a golden case (drift guard, plan Step 28) | all | one top-tier test-author step per contract batch |
| T8 | text and attributes never compared (post-mortem Q2) | all | hand-check sections per checklist (skill §1) |

## X. Cross-cutting decisions

- X1. **Purges pending** in the 7 purging modules (characters, wallet, inventory, friends, groups, storage, notifications; each has `pending_purges()`). One point, wording and order band for all; fix the stale `docs/reference/event-plane-ops.md:117-122`. Step 14 G9 recommends a conditional `DashboardAttention` item.
- X2. **Order bands** decided up front: tabs (wallet 30 … notifications 80), facts, list columns (Embers 30), header actions (Notify below Suspend), row-menu groups, nav (apikeys 10, scheduler 30, config 40, audit 50).
- X3. **FAKE helper**: one `controlplane` deterministic helper vs a copied hash per module (eleven ports add fakes).
- X4. **Dead admin readers** per module: each port reuses or deletes its own before Step 24's clippy gate.
- X5. **Operator as actor** (audit, dashboard activity, config, apikeys): operators are `admin.users`, not entities; a user cell linked to the audit page filtered by actor, `system`/`apikey:<name>` as plain tags.
- X6. **`storage.gc` attention FAKE** moves from the portal to the scheduler provider.
- X7. **Sequencing** with the untracked purge-race-locks plan, which touches all seven purging modules.
- X8. **Writers and `request_id`**: wallet (`idempotency_key`), groups, notifications, apikeys, config; action ids that read like the mockup (`key.create`, `flag.update`).
- X9. The shared "nav entries … arrive with their ports" accept line in five files: one wording, swept at Step 24.

## R. Recurring data rules (proposed as standing decisions in the skill)

- R-a. Identifiers in full (mail G7, accounts, characters G2).
- R-b. Times and relative times computed in SQL, never from a Rust clock; keep the mock's per-column format.
- R-c. Real stable order; a sort arrow only on the real sort column (D9).
- R-d. Singular at 1 everywhere (needs K18).
- R-e. A count is always a `Cell`, `0` included.
- R-f. An element the mockup draws only in a demo state gets the nearest designed sibling's ok shape, declared in the hand-check.
- R-g. Empty states use the portal default; a filter narrowed to zero keeps the toolbar.
- R-h. Truthful constants beat FAKE (G12); exact `count(*)` with a comment naming a missing index (G6).
- R-i. A never-happened sentinel shows "—".
- R-j. D4 vocabulary: real currencies, roles, topics, schedule intervals instead of the mock's.

## P. Per-port decisions that stay with the port

- 14 characters: G2/G3/G7/G8/G10 of its checklist.
- 15 wallet: which currency is "Embers"; global ledger FAKE; spent-24h scan; create-currency placement (D6).
- 16 inventory: count meaning; Grant item `implemented:false` as tool and menu item.
- 17 friends: pending direction, meta "12 · 1 pending".
- 18 groups: group link without a groups entity; promote-to-admin placement; groups probe.
- 19 storage: slot label, version format, FAKE checksum and quota, tile scan.
- 20 notifications: title/body mapping, "accent" meaning, undesigned Notify form.
- 21 apikeys: `apikey` kind, Create/Revoke/Change role shapes, Roles panel, reveal hint, create-retry risk.
- 22 scheduler: Next fire DERIV, FAKE target/status/fires.
- 23 config: per-key revisions NEW, value cap, change note, pending-diff block.
- 24 audit: per-topic actor/action/target, actor options, dashboard tone rule.
