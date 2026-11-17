# Step 11 (mail, pilot): checklist

Read-only extraction per `.agents/skills/mockup-implementation/SKILL.md` §1. Mockup page:
`UILayout/redesign/mail.html` (boot key `PAGES.boot("mail")`, `mail.html:18`), builder
`P.mail` at `UILayout/redesign/assets/pages-platform.js:53-70`, plus helpers `:47-52`
(attempts bar, per-state action map) and `:71` (cancel confirm). Rows are in
`assets/data.js:130-138`. The dashboard pieces are in `assets/registry.js:27` (nav), `:50`
(tile) and `:61` (attention). Contract: `core/controlplane/src/lib.rs`. Primitive templates:
`core/admin/src/templates/primitives.html.tmpl`.

Abbreviations: `pp` = `UILayout/redesign/assets/pages-platform.js`, `reg` =
`UILayout/redesign/assets/registry.js`, `cp` = `core/controlplane/src/lib.rs`, `tmpl` =
`core/admin/src/templates/primitives.html.tmpl`, `ds` =
`docs/research/2026-10-22-1830-admin-redesign-data-sources.md`.

Comparison scope (it decides which rows `admindiff` can see):
- The skeleton keeps tags, classes and nesting. Text is kept only with `--text`
  (`tools/admindiff/src/skeleton.rs:1-8`), and attributes are never compared
  (`skeleton.rs:32-33`). Header labels and nouns are therefore R3 rows that the default
  diff does not catch, and the review has to check them.
- Only `tbody>tr`, `ul>li`, `.svc`, `.pop-row` and `.uptime>i` collapse
  (`skeleton.rs:126-137`). The `div.q-count` and `span.attempts > i` sibling lists compare
  by position.
- Modals, popovers and the palette sit outside `.app` and are not compared
  (`skeleton.rs:36-38`). The cancel confirm and the test-send form are checked by hand.

---

## 1. Element table: mail outbox page

### Shell frame (each row needs an entry in `mail.txt`)

| Element | Mockup line | Carrier | Status |
|---|---|---|---|
| Sign out form in the sidebar foot | `reg:143` (mock has none) | template literal `shell.html.tmpl` | template choice. Copy `index.txt:5` |
| Theme toggle in the topbar | `reg:158` (devbar) | template literal | template choice. Copy `index.txt:12` |
| Design-system nav section | `reg:13,32-33` | none | declared deviation D3. Copy `index.txt:10` |
| Lore nav section | `reg:10,19-24` | none | not ported yet: no Lore module exists. Copy `index.txt:8` |
| Players nav section | `reg:9,18` | accounts port | not ported yet (Step 13). Copy `index.txt:11` |
| Economy nav section | `reg:11,25` | wallet port | not ported yet. Copy `index.txt:9` |
| API keys / Scheduler / Config / Audit nav entries | `reg:26,28-30` | their ports | not ported yet. Copy `index.txt:7` |
| Mail outbox nav entry, active, `span.count.warn` "312" | `reg:27`, `reg:140` | `NavEntry` + `Point::NavCounter` → `Primitive::Cell` with `Tone::Warn` (`tmpl:363-365`) | ok. Value is REAL (§4) |
| Event plane nav entry, `count.alert` | `reg:31` | core.events (already landed) | ok |
| Breadcrumbs "Platform / Mail outbox" | `pp:54`, `reg:147,151` | `section_crumb` (`core/admin/src/routes.rs:101-105`) | ok (hrefs are attributes and are not compared) |
| Page wrapper: the mock puts the panel directly in `.page` (`pp:68` `P.head(...) + q`). The portal's `Layout::Stack` always wraps panels in `div.stack` (`shell.html.tmpl` page branch, `{% if v.layout == "stack" %}<div class="stack">`) | `pp:68` | `PageDecl.layout` | **accept, template choice (expected)**. The same kind as `index.txt:19`. Gap G9 gives the alternative |

### Page head

| Element | Mockup line | Carrier | Status |
|---|---|---|---|
| H1 "Mail outbox" | `pp:68` | `PageDecl.title` | ok |
| Sub "Durable outbox; each message is retried until sent, cancelled or parked." | `pp:68` | `PageDecl.subtitle` | ok. The real semantics match (`modules/mail/src/lib.rs:61-71`) |
| `button.btn` "Test send" with the `send` icon | `pp:68` | `ActionDecl` `PageHead`, `Emphasis::Default`, `then: Form` (`shell.html.tmpl` page-head → `action_button(a)`, `tmpl:92-102`) | ok. REAL (`Service::enqueue_from_admin`, `modules/mail/src/service.rs:215-227`). Its key needs Gap G6 |

### "Outbox" panel (WorkQueue)

| Element | Mockup line | Carrier | Status |
|---|---|---|---|
| Panel title "Outbox" | `pp:59` | `ContributionDecl.title` | ok |
| Panel icon | `pp:59` (none) | `ContributionDecl.icon = ""` | ok |
| Meta "relay smtp.emberline.gg" | `pp:59` | `PanelBody.meta` | **data: decision G8**. `ds:123` says FAKE, but the host is REAL config (`modules/mail/src/config.rs:22,176-215`) |
| `banner warn` "SMTP relay is throttling (421). Retries back off exponentially; nothing is lost." | `pp:60` | `PanelBody.banner` (`Banner{tone: Warn}`), `tmpl:394` | **data: FAKE** (`ds:123`). Gating decision in G8 |
| Tool `btn sm` "Pause sending" (pause icon) | `pp:61` | `ActionDecl` `PanelTool("outbox")`, `implemented: false` | **contract gap G1**: a WorkQueue panel renders no tools (`core/admin/src/render.rs:696-700`) |
| `div.q-counts` with 6 `div.q-count` (n + label; `on` on the selected one) | `pp:57,62` | `WorkQueue.states` / `QueueState{key,label,count,tone: None}` / `selected` (`tmpl:248-250`, `render.rs:1347-1358`) | ok for the carrier. **data: decision G4** on the Failed and Parked tabs. Tone must be `None` on every state, because the mock q-count has no tone class (`pp:62`) |
| q-count "Needs attention" 4 | `pp:57` | `QueueState` | DERIV: retrying + parked (`ds:121`). Default tab when `queue_state` is `None` (`pp:56`) |
| q-count "Pending" 309 | `pp:57` | `QueueState` | DERIV: pending with `attempts = 0`, so the tabs partition. `OutboxStats.pending` (`store.rs:411-419`) counts all pending |
| q-count "Retrying" 2 | `pp:57` | `QueueState` | DERIV: pending with `attempts > 0` (`ds:121`). The count is not covered by an index (see G10) |
| q-count "Failed" 1 | `pp:57` | `QueueState` | **G4** (D4: real `parked` stands for "failed", plan `:2157-2158`) |
| q-count "Parked" 1 | `pp:57` | `QueueState` | REAL `OutboxStats.parked` (`store.rs:413`) |
| q-count "Sent (24 h)" 182,051 | `pp:57` | `QueueState` | REAL `OutboxStats.sent_24h` (`store.rs:414`). Formatted with `thousands` (`render.rs:1353`) |
| Table toolbar: search "Recipient, template or message id", filter chip "Template: any", "+ Filter" | `pp:63`, `bundle.js:155` | none on `WorkQueue` (`cp:861-865`); the queue macro draws no toolbar (`tmpl:263-266`) | **contract gap G2**. Data is NEW per `ds:126` |
| Select column `th.cb` / `td.cb` checkboxes | `pp:63` (`selectable: true`), `bundle.js:157,161` | none on `WorkQueue` | **contract gap G3**. Bulk select is NEW (`ds:126`) |
| Header "Message" | `pp:64` | template literal `Item` (`tmpl:266`) | **contract gap G5** (text only, invisible to the default diff) |
| Header "To" | `pp:64` | template literal `Ref` (`tmpl:266`) | **contract gap G5** |
| Header "Template" | `pp:64` | none | **contract gap G5** |
| Headers "State", "Attempts", "Next attempt", "Last error", "" | `pp:64` | template literals (`tmpl:266`) | ok |
| Message cell `span.mono` id | `pp:65` | `QueueItem.id` (`tmpl:267`) | ok. REAL `OutboxListRow.id` (`store.rs:425-433`). Decision G7 (full uuid vs short) |
| To cell `a > span.user-cell` (avatar + handle) | `pp:65`, `bundle.js:102-107` | `QueueItem.entity` → `ref_cell` user (`render.rs:232-244`, `tmpl:75,82`) | **contract gap G5 / data**: REAL recipient is an email; the avatar/handle is FAKE (`ds:125`). An `EntityRef{kind:"user", id: email}` gives the right shape but a dead entity link |
| Template cell `span.tag` | `pp:65` | none on `QueueItem` (`cp:847-858`) | **contract gap G5**. REAL `kind` (`store.rs:429`) |
| State cell `span.badge <tone>` | `pp:65`, `bundle.js:98-99` | `QueueItem.state` → `status_tone` (`render.rs:160-168,1327-1336`) | ok. `retrying` (DERIV) → warn, `pending` → info, `parked` → none, `sent` → ok, all identical to `bundle.js:98` |
| Attempts `span.attempts` with 5 `i` (`f` / `ok` on the last for sent / empty) | `pp:47,65` | `QueueItem.attempts`/`max_attempts` → squares = `max(max,attempts).min(20)` (`render.rs:1301-1317`) | **contract gap G6a**: the mock always draws 5 squares; the real `max_attempts` is 20 (`config.rs:65`), so the portal would draw 20, and positional `i` lists differ |
| Next attempt `span.dim` ("in 2 min" / "—") | `pp:65` | `QueueItem.next_attempt_at` (`tmpl:267`) | **data: DERIV**. `next_attempt_at` exists (`lib.rs:93`) but `recent_tx` does not select it (`store.rs:468-471`). Format decision in G7 |
| Last error `span.mono` (red for failed) / `span.muted` "—" | `pp:65` | `QueueItem.last_error`, `error_color` (`render.rs:1340-1341`, `tmpl:267`) | ok. REAL `last_error` (`store.rs:432`) |
| Row actions `span.row`: first non-danger labelled `btn sm`, others `btn sm ghost` / `btn sm ghost danger-ghost` icon-only | `pp:48-52,66` | `QueueItem.actions` → `queue_action` (`tmpl:252-261`) | ok for the macro. Bindings in §3. Queue row actions are `PanelTool` actions (`cp:464-466`), which G1 has to split from the panel tool |
| Table foot `tbl-foot`: "7 messages" + pager (25/page select, prev, "1 / 1", next) | `pp:67`, `bundle.js:163` | queue macro foot `<span>{{ n }} items</span>` only (`tmpl:268`) | **contract gap G2b**. The paging data is NEW (`ds:126`, 50-row cap). Also, `table_foot` (`tmpl:172`) lacks the mock's `span` "1 / 1" between the pager buttons. No mapped page has diffed a pager yet, because the events table is `pager: false` (`pp:140`) |
| Per-tab empty: `UI.stateBlock("empty", "Queue is empty", "Nothing in this state.", check)` under the q-counts | `pp:67` | `tmpl:269-270` (also `empty_body`, `tmpl:323`) | ok. See the states row for the banner and tools |

### Cancel confirm (modal, outside `.app`, not diffed)

| Element | Mockup line | Carrier | Status |
|---|---|---|---|
| Title "Cancel {id}?" | `pp:71` | `ConfirmSpec.title` via `prepare: true` | ok |
| Body "The message is removed from the outbox and will never be delivered." | `pp:71` | `ConfirmSpec.body` | ok |
| Impact Recipient, Template | `pp:71` | `ConfirmSpec.impact` (`Cell`s) from `Provider::prepare` (`cp:23-28`) | ok. REAL `recipient`, `kind` read by the bound item id |
| Tone danger, no reason, no type-to-confirm | `pp:71`, `bundle.js:312-314` | `ConfirmTone::Danger`, `ReasonReq::None`, `type_to_confirm: None` | ok |
| Confirm "Cancel message"; done toast "Message cancelled" | `pp:71` | `confirm_label`; `InvokeOutcome::Ok{message}` | ok |

The test-send form has no mockup: the button at `pp:68` has no handler. Its modal is the
portal's generic form modal, a template choice that is reviewed by hand.

### Dashboard (`index.html`) contributions

| Element | Mockup line | Carrier | Status |
|---|---|---|---|
| Tile "Mail queue", value 312 | `reg:50`, `bundle.js:176-186` | `Point::DashboardTile` → `KpiTile{label, value}` | REAL `OutboxStats.pending` (`ds:38`) |
| Tile delta `span.bad-up` "▲ relay throttled" + " · 1 failed" | `reg:50` | `Delta{value, period, tone: Some(Warn)}` → `bad-up` (`render.rs:956-963`, `tmpl:198`) | value is FAKE (`ds:38`); period is REAL parked (D4 word "parked", plan `:2158`) |
| Tile spark `--warn`, 8 points | `reg:50` | `KpiTile.series` + `Palette::Warn` | **data: FAKE** (`ds:38`, `ds:19`), a constant series |
| Tile link to the page | `reg:50` | `KpiTile.link = Link::Page{module:"mail", page}` | ok |
| Attention item: `modal-icon warn`, mail icon, "Mail relay throttling", "312 queued, 2 retrying, 1 failed permanently.", `btn sm` "Outbox" | `reg:61`, `pages-core.js:51` | `Point::DashboardAttention` → `AttentionItem{severity: Warn, title, text, link, action_label}` (`tmpl:300`) | counts REAL, "retrying" DERIV (`ds:43`); the title wording is FAKE (no throttle signal; see G8) |

---

## 2. States row per panel

| Panel | ok | empty | readonly | stale | error | narrowed to zero |
|---|---|---|---|---|---|---|
| Outbox (WorkQueue) | The q-counts are on "Needs attention"; 4 rows (2 retrying, 1 failed, 1 parked); the throttle banner; the Pause sending tool (`pp:56-67`) | Mock: a tab with no rows keeps the panel `ok` and draws the stateBlock under the q-counts, **with** the banner and the tool (`pp:59-67`). Portal: `is_empty(WorkQueue)` = no items (`engine/panels.rs:210`) → state `empty`. That drops the module banner (`panels.rs:75-79`) and keeps only `when_empty` tools (`render.rs:695`). The body still draws the q-counts and stateBlock (`tmpl:323`, flush `tmpl:373`). So: set `when_empty: true` on Pause sending. The banner loss on an empty tab is a residual difference. The default crawl never reaches it because the attention tab is seeded | Unreachable while `writes: true` (`panels.rs:81-91`). The mock's ro-chip and info banner are a forced-state demo (`bundle.js:141,144`) | Needs `ContributionDecl.cache_ttl_s` (recommend `Some(60)`, as core.events does, `events_provider.rs:186-197`). The portal banner matches `bundle.js:140` (`tmpl:391`) | "mail module unavailable" plus Retry (`tmpl:398`, `bundle.js:124-126`) | A search or a non-`any` filter with no rows draws the toolbar, the "No matching items" / "Nothing matches the search or filters." state block, with no foot, the same as an empty table: `foot` is `!items.is_empty() || next_cursor.is_some()` (`render.rs:1419`) (fix 7bb53152, `tmpl:271-273`). Goldens: `queue_search_empty`, `queue_filter_empty`, `queue_unfiltered_empty` (e0431132) |
| Tile "Mail queue" | `reg:50` | Never (`KpiTile` is never empty, `panels.rs:216`) | n/a (no write affordance) | The "stale" badge in the label (`tmpl:197`, `bundle.js:183`) needs `cache_ttl_s` | The portal's tile error state (`bundle.js:180`) | n/a (no search or filters) |
| Attention "mail" | One warn row | An empty `AttentionItems` when nothing is retrying or parked (recommendation). Rows collapse (`skeleton.rs:130`), so this makes no diff | n/a | `cache_ttl_s` | Portal error rendering | n/a (no search or filters) |
| Nav counter | "312" `count.warn` | `Primitive::Empty` when there is no backlog (as `events_provider.rs:349-355` does) → no count | n/a | n/a | No count shown (`tmpl:363-365`) | n/a (no search or filters) |

---

## 3. `writes` and every ActionDecl

**`writes: true`**: cancel, resume and test send write (`service.rs:166-227`). This makes
mail a conformance writer: it must carry a `cp.invoke mail …` CapCase at the
`req.input.<value>` cap (`tools/conformance/src/policy.rs:60,172`,
`tools/conformance/src/checks.rs:228-243`; plan errata `:2199-2203`).

Every action has `min_role: Operator` (as `events_provider.rs:221`) and `multi: false`.
Unless a row says otherwise, `trailing_icon` and `prepare` are false.

| id (proposed) | point | label / icon | Emphasis | danger | implemented | then | when_empty | Real operation |
|---|---|---|---|---|---|---|---|---|
| `test-send` | `PageHead("outbox")` | "Test send" / `send` | Default (`pp:68` is `btn`) | false | **true** | `Form(FormSpec{fields:[Field{key:"to", label:"Recipient", kind:String, required:true}], submit_label:"Send test", ..})` | false | `Service::enqueue_from_admin` (`service.rs:215-227`) with kind `admin.test`, the subject and body of the old page (`git show b8435c9a^:modules/mail/src/admin.rs:64-66`), and the key from G6 |
| `pause-sending` | `PanelTool("outbox")`, a queue-level tool (G1) | "Pause sending" / `pause` | Default (`pp:61` `btn sm`) | false | **false** (NEW, `ds:124`; plan `:1795`) | `Invoke` | **true** (see states) | none |
| `retry-now` | queue row (G1) | "Retry" / `refresh` | Default (labelled first, `pp:49`) | false | **false** (NEW, `ds:129`) | `Invoke` | false | none |
| `park` | queue row | "Park" (title "Park: keep but stop retrying") / `park` | Ghost (`pp:51`) | false | **false** (NEW, `ds:129`) | `Invoke` | false | none |
| `cancel` | queue row | "Cancel" / `x` | Ghost | **true** (`pp:52` danger-ghost) | **true** | `Confirm(ConfirmSpec{tone: Danger, reason: None, confirm_label:"Cancel message", ..})`, **`prepare: true`** (impact per item, `pp:71`) | false | `Service::cancel_pending` (`service.rs:194-205`). Its `state = 'pending'` CAS (`store.rs:584-598`) also covers retrying rows. 0 rows → `NoOp` "already moved" |
| `cancel-parked` | queue row | "Cancel" / `x` | Ghost | **true** | **false** (NEW, `ds:127`) | `Invoke` | false | none. `cancel_pending_tx` refuses non-pending rows (`store.rs:592`) |
| `resume` | queue row | "Resume" / `play` | Default (labelled, `pp:50`) | false | **true** | `Invoke` | false | `Service::requeue_parked` (`service.rs:166-177`, CAS `state='parked'`, `store.rs:520-535`). 0 rows → `NoOp` |
| `requeue-all` | **not in the mockup** | n/a | n/a | n/a | REAL (`service.rs:181-192`) | n/a | n/a | **decision G11** |

`QueueItem.actions` per displayed state, matching `MAIL_ACTIONS` (`pp:48`):
- `pending`, `attempts = 0`: `[cancel]`.
- `retrying`, DERIV pending with `attempts > 0`: `[retry-now, park, cancel]`.
- real `parked` (shown as D4 for "failed"): `[resume, cancel-parked]`. The mock's "failed"
  row, `[retry, cancel]` (`pp:48`), has no real counterpart (G4).
- `sent`: `[]`.

`request_id` (plan `:1764-1769`): cancel and resume are idempotent through their state-CAS
legs, so a repeat matches 0 rows. Test send is idempotent through the outbox
`idempotency_key` (G6). This leaves no Leftovers-for-owner double-apply risk.

Every invoke first refuses any `input` value over the shared cap (4096,
`core/admin/src/conformance.rs:14`, `events_provider.rs:24-25,113-117`). That is the
`control_plane_cap_findings` equality the restored probe must execute (§6 G12). An item id
that is not a uuid shape is refused as `Invalid` before SQL, so the `$1::uuid` cast is
never reached. The old rule is `is_outbox_id`, `OUTBOX_ID_BYTES = 36`
(`git show b8435c9a^:modules/mail/src/admin.rs:266-270`).

---

## 4. Dashboard contributions and `order`

From the port template (plan `:1772-1777`) and the mockup order (`reg:26-31,45-54,57-63`):

| Contribution | Point | order | Content |
|---|---|---|---|
| `nav.queue` | `Point::NavCounter` | 0 (as core.events, `events_provider.rs:286`) | `Primitive::Cell{kind: Badge, value: thousands(pending), tone: Some(Warn) when retrying+parked > 0, else None}`. `Primitive::Empty` when pending + parked = 0. Renders `span.count.warn` (`tmpl:363-365`) |
| NavEntry `mail` | section `platform` | **20** (apikeys 10, mail 20, scheduler 30, config 40, audit 50; Event plane 60, `events_provider.rs:268`) | label "Mail outbox", icon `mail`, page `outbox`, `counter: Some("nav.queue")` |
| `tile.queue` | `Point::DashboardTile` | **50** (accounts 10, portal 20/30, storage 40, core.events 60, wallet 70, portal 80; `portal_provider.rs:57-59`, `events_provider.rs:287`) | §1 dashboard rows |
| `attention.queue` | `Point::DashboardAttention` | **40** (portal 10/30/50, core.events 20; `portal_provider.rs:60-62`, `events_provider.rs:288`) | icon `mail` on the decl. Severity `Warn`. Text `"{pending} queued, {retrying} retrying, {parked} parked."` (D4). Link to the page, action "Outbox", `since: ""` (not drawn, `tmpl:300`) |
| `outbox` | `Point::Page("outbox")` | 10 | WorkQueue panel. `PageDecl{id:"outbox", title, subtitle, head_actions:["test-send"], tiles:[], layout: Stack(["outbox"])}` |

---

## 5. Seed needs

A fresh dev fleet has an **empty outbox**:
- No module produces `mail.send_requested`. The only references are mail's own consumer,
  tests and topiccheck: `modules/mail/src/lib.rs:251-253`, `tools/topiccheck/src/main.rs:204`,
  `golden.rs:288`. This is a lower-bound grep.
- The fleet runs `MAIL_PROVIDER=log` (`weles/fleet.monolith.toml:33`,
  `weles/fleet.split.toml:177`, `tools/processctl/src/fleet.rs:118`). `LogSender` always
  returns `Ok` (`modules/mail/src/providers.rs:131-149`), so any due pending row is
  `sent` within about a second (`worker.rs:51`), and nothing ever retries or parks
  (`worker.rs:124-144`).

The designed state (attention tab, `pp:56-58`; `data.js:130-138`) needs:

| Designed state | Rows | Constraint |
|---|---|---|
| 2 retrying | `pending`, `attempts` 3 and 2, `last_error` "421 4.7.0 Try again later (relay throttled)", `next_attempt_at` well in the future | The future time keeps the log drain from claiming them (claim predicate `next_attempt_at <= now()`, `store.rs:294`) before the crawl |
| 1 "failed" (D4 → parked) | `parked`, `attempts` 5, "550 5.1.1 Mailbox unavailable" | A parked row at 5 < 20 attempts is the real `Rejected` path (`worker.rs:131-133`) |
| 1 parked | `parked`, `attempts` 5, "recipient banned · parked by policy" | The text is FAKE: no policy parking exists |
| Pending backlog (Pending tab, nav count, tile) | ≥1 `pending`, `attempts` 0, `next_attempt_at` in the future | The same drain constraint. These rows are also Step 12's cancel target (plan `:1804`): a fresh test send would be sent before an operator could cancel it |
| Sent (24 h) | ≥1 `sent`, `sent_at` within 24 h, `attempts` 1 | — |
| Recipients | e.g. demoseed's `demo-NN@demo.local` (`tools/demoseed/src/lib.rs:292`) | — |

**demoseed cannot produce any of this.** Mail has no public API: no `mailapi`, no HTTP or
RPC route (`modules/mail/src/lib.rs:4-7`, `Cargo.toml` comment). Even with a producer, the
log provider would only ever produce `sent` rows. Test send through `cp.invoke` gives a
pending row only for about a second.

**Mail has no operator CLI.** `tools/` holds admincheck, adminctl, admindiff, archcheck,
checkmodules, conformance, csharp-client-gen, demoseed, devctl, edgeca, eventctl,
opscatalog-gen, playercli, processctl, requirecheck, routecheck, rpc-contract-model,
rpc-macro(-tests), splitproof, testdb, topiccheck and verifyctl. There is no `mailctl`.

~~So part `a` must add the seed itself (decision G13). Under the skill's Seed rule
(`SKILL.md` §5), a `data difference` accept entry is not allowed while a module-owned tool
can produce the data, and one can.~~ **Overruled by G13 (owner, 2026-11-17):** no seed
tool and no `mailctl`. Each designed state above that a fresh fleet lacks is a
`data difference` accept entry until plan Step 24b.

---

## 6. Contract gaps and decisions for part `a`

**G1. A queue-level tool versus per-item queue actions.**
- Problem: the portal draws **no** panel tools for a WorkQueue (`render.rs:696-697`).
  Every `PanelTool(c)` action is a row action bound per `QueueItem` (`cp:464-466`,
  `render.rs:1318-1323`). "Pause sending" (`pp:61`) would vanish.
- Options:
  - (a) A new `ActionPoint::QueueRow(contribution)` for per-item actions. `PanelTool` then
    always draws in the tools row, and `render.rs:697` changes.
  - (b) An `ActionDecl` flag (`row: bool`).
  - (c) Draw the PanelTool actions that no item lists. This is fragile: when a tab has no
    such rows, row actions pop into the tools.
- **Recommend (a).** It keeps "where it renders" in the point, as everywhere else. A
  golden case for the new variant is a top-tier gate test.
- **Decided (owner, 2026-11-17): (a).** `ActionPoint::QueueRow` for per-item actions; a
  `PanelTool` on a WorkQueue draws as a panel tool.

**G2. WorkQueue table toolbar (search + filters) and the foot/pager.**
- Problem: `WorkQueue` has no `search_placeholder`, `filters` or `PageInfo` (`cp:861-865`),
  and the macro draws neither a toolbar nor a pager (`tmpl:263-268`).
- Options:
  - (a) Add `search_placeholder: Option<String>`, `filters: Vec<FilterDef>` and
    `page: PageInfo` to `WorkQueue`, and reuse `table_toolbar` and `table_foot`
    (`tmpl:163-173`).
  - (b) Accept them as contract gaps.
- **Recommend (a)** for fidelity (R3).
- Data sub-decision: `ds:126` labels search, the template filter and paging **NEW**. R1
  calls a new read over existing columns DERIV (plan `:45-49`). Filtering by `kind` and
  matching `id` or `recipient` qualify, as reads with no new index.
  - (i) Implement them as DERIV, with a 50-row cap and `next_cursor` for paging. This
    needs the owner's nod, because the research table says NEW.
  - (ii) Render the toolbar and pager inert (NEW).
  - **Recommend (i).** It makes the To-cell link of G5 useful.
- Also add the "1 / 1" `span` that `table_foot` lacks (`tmpl:172` against `bundle.js:163`),
  and a noun for the total ("N messages", `pp:67`). The table foot's
  `total` text is built in `render.rs:893-916`.
- **Decided (owner, 2026-11-17): (a).** WorkQueue gains the search box, a filter and the
  pager foot.

**G3. Selectable column.**
- Problem: there is no `selectable` on `WorkQueue`, and bulk actions are NEW (`ds:126`).
- Options:
  - (a) `WorkQueue.selectable: bool`, drawn like `DataTable.selectable` (`tmpl:178`): inert
    checkboxes.
  - (b) Accept it as a contract gap.
- **Recommend (a).**
- **Decided (owner, 2026-11-17): (a).** A selectable checkbox column.

**G4. Failed versus Parked (D4).**
- Problem: there is no real `failed` state; `parked` covers both permanent rejections and
  exhausted retries (`worker.rs:124-144`, `store.rs:11-20`). The mock has both tabs and
  both row shapes (`pp:48,57`). D4 already decided "mail `parked` for 'failed'" (plan
  `:2157-2158`).
- Options:
  - (a) Five tabs (Needs attention, Pending, Retrying, Parked, Sent (24 h)), with real
    parked rows in the mock-"parked" row shape `[resume, cancel-parked]`. This needs two
    D4 accept entries: the 6th positional `div.q-count`, and the mock-only "failed" `tr`
    shape.
  - (b) Six tabs, with "Failed" listing real parked rows and "Parked" showing 0
    (operator-park is NEW). This contradicts D4's vocabulary.
- **Recommend (a).** D4 is a recorded decision, and research does not overrule it.
- **Decided (owner, 2026-11-17): (a).** Five tabs; the mockup's Failed tab and failed row
  are D4 accept entries.

**G5. WorkQueue columns: labels, To and Template.**
- Problem: the queue headers are literals "Item" and "Ref" (`tmpl:266`). There is no
  template column, and the ref column only renders an `EntityRef` (`render.rs:1326`), so
  an email recipient becomes a dead `/admin/e/user/<email>` link.
- Options:
  - (a) `WorkQueue.columns: Vec<Column>` plus `QueueItem.cells: Vec<Cell>`, drawn between
    the id and the state, together with `WorkQueue.item_label`. The To cell is then a
    `Cell{kind: User, value: email, link: Link::Page{mail, outbox, {q: email}}}`, which
    renders `a > span.user-cell` (`tmpl:75,82`), and Template is `Cell{kind: Ref, value: kind}`.
  - (b) Keep `entity` (`kind: "user"`, id email) and accept the Template column as a gap.
- **Recommend (a).** Avatar and initials derive from the value (`render.rs:239-240`),
  which R1 allows as a deterministic fake. `payload_preview` stays unused.
- **Decided (owner, 2026-11-17): (a), with To as a plain email.** Declared extra columns
  with cells per item (Template, To).

**G6. Test-send idempotency key and the attempts bar.**
- (a) Key: `FormSpec.hidden` is static in the manifest, so the old render-time minting
  (`mint_test_key`, `b8435c9a^:modules/mail/src/admin.rs:254`) has no home. The portal's
  form `request_id` is `"<prefix>:<base64url token>"` (`core/admin/src/engine/forms.rs:63-66`,
  `core/admin/src/lib.rs:823-827`; it is the `request_id` at `routes.rs:1060`). Mail's
  authority admits only `admin-send-test-` + 32 hex (`service.rs:115-125`).
  - (i) Derive `admin-send-test-` + 32 hex from a digest of `request_id`. Mail has no sha2
    dependency today, so a new dependency is needed.
  - (ii) Widen `is_test_key` to `admin-send-test-` + the `request_id` verbatim, within
    `MAX_IDEMPOTENCY_KEY_BYTES`.
  - Refuse when `request_id` is `None` in either case.
  - **Recommend (ii).** It is the plan's option 2a, "use `InvokeCall.request_id` as its
    idempotency key" (plan `:1764-1766`), with no new dependency. The shape check stays at
    the authority.
- (b) Attempts bar: the mock draws a fixed 5 squares (`pp:47`). The portal draws
  `max(max_attempts, attempts)` capped at 20 (`render.rs:1302,1310`), and the real
  `max_attempts` is 20 (`config.rs:65`).
  - (i) Fix the portal bar at 5 squares, filled `min(attempts, 5)`, keeping the real
    "N attempts" title. This changes the constant in `render.rs`, which is part `a`
    because part `b` may not touch `render.rs` (SKILL §4).
  - (ii) Have the provider send a FAKE `max_attempts: 5`.
  - **Recommend (i).** The field then stays honest.
- **Decided (owner, 2026-11-17): (a)(ii) and (b)(i).** The key is `admin-send-test-` +
  `request_id` and `is_test_key` widens to match; the bar draws a fixed 5 squares.

**G7. Message id display and next-attempt format.**
- Id: `QueueItem.id` is both the displayed text and the bound `item` (`render.rs:1322`).
  - (i) Show the full uuid (REAL; no contract change).
  - (ii) Add `QueueItem.label`.
  - **Recommend (i).**
- Next attempt: the mock is relative ("in 2 min", `data.js:132`), but the provider reads no
  clock (`events_provider.rs:98-100`).
  - (i) Compute the seconds in SQL (`next_attempt_at - now()`, the DB clock) and format
    them "in 40 s" / "in 2 min". `None` for parked and sent.
  - (ii) Absolute `day_time`.
  - **Recommend (i).** It needs a new `recent_tx` column; `store.rs:468-471` does not
    select it today.
- **Decided (owner, 2026-11-17): (i) and (i).** The full uuid; relative next attempt from
  SQL.

**G8. Meta, banner and attention title (throttling).**
- Problem: there is no throttle signal (`ds:123`).
- Meta options:
  - (a) REAL from config: `relay <MAIL_SMTP_HOST>` for smtp, `provider log` for log
    (`config.rs:22,183-215`). "A real field always wins over a fake" (`ds:5-6`).
  - (b) FAKE "relay smtp.emberline.gg".
  - **Recommend (a).** It means passing the config into the provider (`lib.rs:120-132`).
- Banner and attention title options:
  - (a) Show the mock text when the retrying count is above 0, else no banner and no
    attention item.
  - (b) Always show it (FAKE, unconditional).
  - **Recommend (a)**, with a `// FAKE:` comment on the "421 / throttling" wording.
- ~~Under (a), the seed's retrying rows make both appear for the crawl.~~
- **Decided (owner, 2026-11-17): (a) and (a).** Meta from `MAIL_SMTP_HOST`; the banner and
  the attention item show only while a row is retrying. With no seed (G13), the crawl
  sees neither.

**G9. Single-panel page wrapper.**
- Problem: `Layout::Stack` wraps the panel in `div.stack`; the mock has none (`pp:68`).
- Options:
  - (a) A template-choice accept entry, like `index.txt:19`.
  - (b) A `Layout::Single` variant.
- **Recommend (a).** The difference is only the wrapper.
- **Decided (owner, 2026-11-17): (a).**

**G10. Retrying and pending counts are not index-only.**
- Problem: `stats_tx` is deliberately four partial-index counts (`store.rs:406-410,
  440-447`). Splitting pending by `attempts > 0` needs a heap read per pending row, and R1
  forbids new indexes (plan `:48-49`).
- Options:
  - (a) Accept it: one bounded `count(*) FILTER` over `state = 'pending'` via
    `mail_outbox_due_idx`, within `ADMIN_BUDGET` (`service.rs:107`).
  - (b) Show only the total pending count, which drops the mock's partition.
- **Recommend (a)**, with a comment naming the cost.
- **Decided (owner, 2026-11-17): (a).** No new index (R1).

**G11. "Requeue all parked" has no element in the mockup.**
- Problem: the plan lists it as REAL (plan `:1794`), but R3 and the skill forbid invented
  elements (SKILL `:16-18`), and D6 covers only the owner's Q1 list (plan `:2138-2141`).
- Options:
  - (a) Leave it off the page; `requeue_all_parked` stays as an unused store function.
  - (b) Add it as an extra element, which needs a D6-style owner approval and a portal-only
    accept entry.
- **Recommend (a), and ask the owner.** The plan text and R3 conflict here.
- **Decided (owner, 2026-11-17): (a).** Not built; the plan's REAL list loses it.

**G12. "Restore the mail probe deleted in Step 1" (plan `:1796`).**
- What was deleted (`git show b8435c9a -- modules/mail/src/conformance.rs
  tools/conformance/src/policy.rs`):
  - `conformance_test_key_rejected` / `TEST_KEY_BYTES`;
  - `conformance_outbox_id_rejected` / `OUTBOX_ID_SHAPE_BYTES`;
  - their two CapCase rows in `policy.rs` `mail()`;
  - all of these drove the deleted `admin::apply_submit` against a dead pool, which is
    recoverable from `b8435c9a^`.
- The old input key `admin.adminSubmit params.<value>` no longer exists. The new contract
  requires a `"cp.invoke mail input value"` CapCase at `CORE_EVENTS_INPUT_VALUE_BYTES`
  (4096) (`policy.rs:60,172,372-374`). The row's `basis` text must be extended to name mail.
- **Recommend:**
  - restore the dead-pool probes, re-targeted at the new provider's `invoke`: the test-key
    shape (G6) and the item uuid shape;
  - add the mandatory cp.invoke CapCase;
  - put the restore in part `a` per the template (plan `:1762-1763`).
- **Decided (owner, 2026-11-17):** restore the probe from `b8435c9a^` with its `cp.invoke`
  CapCase.

**G13. The seed vehicle (from §5).**
- Options:
  - (a) A new `tools/mailctl seed-mockup` (eventctl precedent, SKILL `:179-181`). It is
    idempotent: fixed `idempotency_key`s, rows reset on each run. archcheck's
    foreign-schema scan covers only `modules/*/src` (`tools/archcheck/src/main.rs:2215-2216`),
    so a tool may write `mail.outbox`. Better still, the SQL lives in a
    `mail::conformance`-style `pub` fn in the module, and the tool is thin.
  - (b) An env-gated `MAIL_DEV_SEED` in mail's `migrate`, on the APIKEYS/WALLET
    precedent (`modules/apikeys/src/lib.rs:264-283`, `modules/wallet/src/lib.rs:112-206`).
    It seeds at boot and needs the new key in `weles/fleet.*.toml` and in processctl's
    sized `MAIL_OVERRIDEABLE_ENV: &[&str; 10]` (`tools/processctl/src/fleet.rs:36-39`).
    Boot-time rows also age before the crawl.
- ~~**Recommend (a).** It matches the skill's "module's own operator tool", and its re-run
  resets rows that a test-cancel or resume moved.~~
- ~~The run order then gains `target/debug/mailctl seed-mockup` (SKILL §7 placeholder line).~~
- ~~This is a new workspace member under Dev tooling scope (`.agents/shared/gamebackend.md:594-610`).~~
- **Decided (owner, 2026-11-17): neither.** No seed tool (plan Step 10b item 5 **Ports**,
  as superseded); missing data is a `data difference` accept entry until Step 24b.

**Gap count: 13 decisions (G1–G13).** The contract-shape gaps that change
`core/controlplane` or `render.rs` are G1, G2, G3, G5 and G6(b). The rest are data,
module or tooling decisions.

---

## 7. Accept-file plan

**`not ported yet` entries the mail port resolves. They go STALE and must be deleted:**
- `tools/admindiff/accept/index.txt:6`: `frame … nav.nav > div.nav-section: mock-only a.nav-item#848c49 | not ported yet: the Mail outbox nav entry …`
- `tools/admindiff/accept/index.txt:18`: `frame … div.g-tiles.grid: mock-only [tile "Mail queue"] | …`
- `tools/admindiff/accept/index.txt:24`: `mock-only tile "Mail queue" | …`
- `tools/admindiff/accept/events.txt:6`: the same nav entry as `index.txt:6`.

The mail attention item has no entry to remove. `.pop-row` collapses
(`skeleton.rs:130`), so its absence was never a difference.

Unverified until the crawl: whether `index.txt:7` / `events.txt:7` (`a.nav-item#ecbb9a`,
the four unported Platform entries) still matches once a sibling with a count sits among
them. Its hash is of the entry's own shape, so it should stay, but sibling alignment in the
nav list is positional.

**New `tools/admindiff/accept/mail.txt`. Copy these shell-frame entries from `index.txt`,
with the header comment naming `/admin/p/mail/outbox` and `mail.html`:**
- `index.txt:5` (Sign out form, template choice)
- `index.txt:7` (API keys / Scheduler / Config / Audit nav entries, not ported yet)
- `index.txt:8` (Lore section, no Lore module exists)
- `index.txt:9` (Economy section, wallet port)
- `index.txt:10` (Design-system section, D3)
- `index.txt:11` (Players section, accounts port)
- `index.txt:12` (theme toggle, template choice)
- **not** `index.txt:6`: mail's own nav entry lands.
- **not** `index.txt:13-26`: dashboard tiles and panels (SKILL `:209-210`).

**Expected page-specific entries, depending on the G decisions:**
- G9(a): the `div.stack` wrapper, template choice. The exact `<difference>` text comes from
  the first `diff` `NEW` lines.
- G4(a): the 6th `div.q-count` and the mock-only "failed" row shape, `declared deviation D4`.
  The failed row is `tr#d4bd69`: D4, never a data difference.
- G13 (no seed): the other two mock-only row shapes (`tr#34fee4`, `tr#83c6e0`) and the
  throttle banner, `data difference`.
- Not predicted at first, found by the first diff:
  - the toolbar `span.input-icon` / `form.input-icon` pair, `template choice`, copied from
    `events.txt:12-13`;
  - `frame … div.page: mock-only [panel "Outbox"]`, `template choice` (G9: the panel sits
    inside the `div.stack` wrapper).
- If G1, G2, G3 or G5 is accepted instead of fixed: one `contract gap` line each.
- The mock-only "1 / 1" pager span, if G2 reuses `table_foot` unfixed.

Part `b` writes the exact lines from the orchestrator's first crawl and diff (SKILL §6).

---

## 8. admindiff page-map change (part `a`)

`tools/admindiff/src/pages.rs:32`:

```
Page { page: "mail", route: Route::Pending, search: "" },
→ Page { page: "mail", route: Route::Mapped("/admin/p/mail/outbox"), search: "" },
```

The route form is `/admin/p/<module>/<page>` (`core/admin/src/render.rs:86-88`), so it
follows the `PageDecl.id` chosen in §4 (`outbox`). `pages_tests.rs:66-70` uses `player` as
the pending sample and is unaffected.

---

## Not verified

- No `cargo` command was run: no `admindiff mock --page mail`, crawl or diff. Shapes are
  read from the source, so every "expected" accept line is a prediction.
- I did not trace mail-svc serving the `cp.*` edge face in the split. admin-svc dials
  `mail` (`cmd/admin-svc/src/main.rs:40`, `lib.rs:35`) and the split toml sets
  `MAIL_EDGE_ADDR` (`weles/fleet.split.toml:297`).
- I did not check whether admincheck needs any per-module list change for a new provider host.
