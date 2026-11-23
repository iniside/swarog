# Step 20 (notifications): checklist

Read-only extraction per `.agents/skills/mockup-implementation/SKILL.md` §1, on the layout of `docs/plans/2026-11-22-step19-storage-checklist.md` (§0-§7, §9, §10, Not verified). HEAD at extraction: `78b5fc53`. Mockup page: `UILayout/redesign/player.html?tab=inbox` (Inbox tab). Scope: `plan:2345-2351` (20a1, 20a2, the owner decision before 20a2).

Builder lines notifications owns:
- player: `tabRender.inbox` `pc:142-148`. Panel for module `notifications` at `entity.user.tab`, prim Timeline, title "Inbox", meta "4 · 1 unread", `flush`; tool "Send notification" (`btn sm`, send icon) (`pc:142`). Body `timeline([...])` of four entries, each `{t: "12:00", day: "Today"|"Oct 20", html: "<b>…</b> rest", meta: "<kind or template> · unread|read", icon, tone?}` (`pc:143-147`). Only the unread entry has `tone: "accent"` (`pc:144`).
- Registry: tab `reg:74` (`{module: "notifications", id: "inbox", label: "Inbox", prim: "Timeline", count: () => 4}`, no `readonly`); header action `reg:90` ("Notify", icon `send`, run is a toast "Compose notification…"); row-menu group `reg:106` (one item "Send notification…", icon `send`, run is the same toast).
- The Timeline builder: `bj:192-201` (a `tl-day` label at each day change; `li` = `span.when`, `span.ico <tone>`, `div.body > div` html + `div.meta`; empty → stateBlock "No events").
- Out of scope, with reasons:
  - `reg:114-115,154,198-200` the top-bar bell popover (`notification.source`, `notifPop`): a shell popover, already excluded at `pages:587`.
  - `dj:101,118,142,144` (services row, API-key scope `notifications.send`, subscription rows): other pages.

Abbreviations: `pc` = `UILayout/redesign/assets/pages-core.js`, `reg` = `…/registry.js`, `dj` = `…/data.js`, `bj` = `UILayout/design-system/components/bundle.js`, `cp` = `core/controlplane/src/lib.rs`, `ord` = `core/controlplane/src/order.rs`, `tmpl` = `core/admin/src/templates/primitives.html.tmpl`, `tiles` = `core/admin/src/render/tiles.rs`, `pan` = `core/admin/src/engine/panels.rs`, `forms` = `core/admin/src/engine/forms.rs`, `routes` = `core/admin/src/routes.rs`, `ns` = `modules/notifications/src/store.rs`, `nsv` = `modules/notifications/src/service.rs`, `nl` = `modules/notifications/src/lib.rs`, `np` = `modules/notifications/src/projection.rs`, `nc` = `modules/notifications/src/conformance.rs`, `na` = `api/notifications/api/src/lib.rs`, `sad` = `modules/storage/src/admin.rs`, `gad` = `modules/groups/src/admin.rs`, `gc` = `modules/groups/src/conformance.rs`, `wad` = `modules/wallet/src/admin.rs`, `aad` = `modules/accounts/src/admin.rs`, `pages` = `tools/admindiff/src/pages.rs`, `plan` = `docs/plans/2026-10-22-1759-admin-control-plane-redesign-plan.md`, `s19` = `docs/plans/2026-11-22-step19-storage-checklist.md`.

Method: targeted reads with line numbers and `git show b8435c9a^:` reads; ripgrep sweeps are marked **[grep]** (a lower bound). No cargo, `admindiff mock`, crawl, diff or splitproof.

## 0. Required checks

**0.1 Timeline carrier and its time format.**
- `TimelineEntry{at (RFC 3339 UTC), actor, title (bold, before verb), verb, target, summary, badges, detail, icon, tone}` (`cp:1293-1309`); `Timeline{entries, cursor_before, live, days, date_only}` (`cp:1311-1319`).
- The portal draws `span.when` = `HH:MM` cut from `at`, a day label "Oct 20" (`tiles:128-137`) only when `days` is set (`tiles:162,183`), `b` before the title, and `div.meta` = target · detail (`tmpl:261-266`). "Older" builds from `ctx.query` + `before` (`tiles:184`); the cursor arrives as `PanelQuery.before` (`cp:436-437`).
- An empty timeline is the panel's empty state (`pan:214`).
- **No module provider draws a Timeline yet** ([grep] `Timeline` over the eight `modules/*/src/admin.rs`: no hit). Its golden cases exist (`core/admin/src/golden_tests/cases.rs:439,535-578`).
- **Consequence for the standing data rules:** the mockup's format for this column is time of day under a day label (`pc:144-147`), not a relative age. So `controlplane::ago` (`cp:418-425`) has no column on this page. Times still come from SQL (`ns:34-36`, RFC 3339 rendered by `to_char`).

**0.2 What the store holds.**
- `notifications.messages(id uuid, player_id uuid, kind ≤64B, title ≤200B, body ≤4000B, created_at, read_at, source_event_id)` (`nl:56-68`); the Rust twins are `MAX_TITLE_BYTES = 200`, `MAX_BODY_BYTES = 4000`, `MAX_KIND_BYTES = 64` (`na:19-27`).
- Indexes: `notifications_source_event_idx` (unique, partial), `notifications_inbox_idx`, `notifications_created_at_idx`, `purged_players_pending_idx` (`nl:142-157`).
- **There is a title column** (owner decision §6 N5 maps onto it directly).

**0.3 The operator send path exists and is dead today.**
- `Service::send_operator_mail` (`nsv:370-428`) carries `expect(dead_code, reason = "Step 20a2: the Send notification row action calls it")` (`nsv:383-389`).
- It refuses any key that is not exactly `admin-send-mail-` + 32 hex (`nsv:95-113,392-393`).
- It runs `deliver_on`'s policy (`nsv:395`) and answers `Sent::Appended | Duplicate | KeyReused` (`nsv:199-208`) via `matches_tx` (`ns:347-366`). The byte caps are refused as `Error::invalid` (`nsv:213-244`).
- Fan-in kinds: `wallet.credit`, `account.promoted`, `friend.requested`, `friend.accepted` (`np:9-12`).
- The removed admin form (`git show b8435c9a^:modules/notifications/src/admin.rs`) had fields "Player id", "Title" (`title`), "Message" (`body`) (lines 261-267), `KIND_OPERATOR_MAIL = "operator.mail"` (line 62), and an `OsRng`-minted key (lines 283-294). The tests still use `kind: "operator.mail"` (`modules/notifications/src/tests.rs:120,407`).

**0.4 The portal's `request_id` does not have the key's shape.**
- A form submit sends `request_id: Some(row.id)` (`routes:1202`). `row.id` = `"<action id>:<token>"` (`forms:63-66`).
- So the provider must derive the 32-hex half (§6 N6).
- `sha2 = "0.10"` is a workspace dependency (`Cargo.toml:250`); notifications does not depend on it (`modules/notifications/Cargo.toml`).

**0.5 Wiring today.**
- No `admin.rs` in `modules/notifications/src` (conformance, lib, projection, service, store, tests); no `PROVIDER_SLOT` contribution in `init` (`nl:165-249`); no `controlplane` dependency (`modules/notifications/Cargo.toml`).
- admin-svc already dials notifications: `peer_face("notifications", wiring, "127.0.0.1:9011")` (`cmd/admin-svc/src/lib.rs:34`) and `with_peer("notifications", …)` (`cmd/admin-svc/src/main.rs:39`).
- Order constants exist: `dashboard_attention::NOTIFICATIONS_PURGES = 67` (`ord:39`), `player_tab::NOTIFICATIONS = 80` (`ord:58`), `player_header_action::NOTIFICATIONS_NOTIFY = 20` (`ord:79`), `player_row_action::NOTIFICATIONS = 60` (`ord:89`).
- No contract change, so no Na0.

**0.6 Dead helpers waiting for this step.**
- `InboxRow` (`ns:53-63`) and `recent` (`ns:286-312`), both "Step 20a1: the Inbox tab lists it". `recent` is cross-player and unpaged.
- `pending_purges` → `(count, UTC day)` (`ns:250-267`).
- `player_stats` → `(messages, unread)` for one player, `(0,0)` for a non-uuid (`ns:315-340`).
- `page_by_player` (`ns:72-130`) is live (the player API) and is keyset on `(created_at, id)`.

## 1. Element table

### Player detail, Inbox tab

| Element | Mockup line | Carrier | Status |
|---|---|---|---|
| Tab "Inbox", eighth, count 4, no lock | `reg:74` | `EntityTab{player, "inbox", "Inbox", 1, []}` order 80 + `EntityTabCount{player, inbox}` (as `gad:379-397`) | REAL `player_stats().0` (N3) |
| Panel title "Inbox", `flush` | `pc:142` | `ContributionDecl.title`; flush per SKILL §3 | ok |
| Meta "4 · 1 unread" | `pc:142` | `PanelBody.meta` = `"{n} · {u} unread"` | REAL `player_stats` (N3); `0 · 0 unread` at zero (a count is never absent) |
| Tool "Send notification" `btn sm` + send icon | `pc:142` | `PanelTool("inbox")`, `Then::Form`, `when_empty: true` | N7 |
| Day label "Today" / "Oct 20" | `pc:144-147` | `Timeline.days: true` → `tl-day` (`tmpl:263`) | the portal draws "Nov 22", never "Today" (N4, H4) |
| Time "12:00" | `pc:144` | `at` = `created_at` RFC 3339 (`ns:35`), cut to `HH:MM` UTC (`tiles:130`) | REAL |
| Icon per entry | `pc:144-147` | `TimelineEntry.icon` from `kind` | N2 |
| Accent tone on the unread entry | `pc:144` | `TimelineEntry.tone` when `read_at == ""` | N2 |
| `<b>title</b> rest` | `pc:144` | `title: Some(title)`, `summary: body` | REAL (N2) |
| Meta "template season_reward · unread" | `pc:144-147` | `detail: Some("{kind} · unread|read")` | REAL; D4: the real kind replaces "template …"/"broadcast …" |
| Older pager | (none: 4 entries) | `cursor_before` = `encode_cursor` of the last row when a 26th exists | N1 |
| Empty inbox | none drawn | `Primitive::Empty` portal default (`pan:214`) | rule: no hand-made text |

### Player entity head (every `player*` page)

| Element | Mockup line | Carrier | Status |
|---|---|---|---|
| "Notify" `btn` + send icon, between Change Lore role and Suspend | `reg:88-92` | `HeaderAction("player")`, order 20, `Then::Form` | REAL send (N5-N8); the mockup's toast vs the portal's modal is an `EXCLUDED_MODALS` row (§7) |

### Player row menu (every `.menu.user` host)

| Element | Mockup line | Carrier | Status |
|---|---|---|---|
| Group "notifications", item "Send notification…", send icon, enabled | `reg:106` | `RowAction("player")`, order 60, `Then::Form` | REAL; same form (N7) |

### Dashboard

| Element | Mockup line | Carrier | Status |
|---|---|---|---|
| Notifications purges pending (no mockup line) | none | `DashboardAttention` order 67 → `controlplane::purge_backlog_body("Notifications purges pending", pending, oldest, PURGE_SWEEP_SUB.id)` (`cp:390-414`, as `sad:248-249`, `gad:333`) | REAL `pending_purges` |

### Send notification modal (no mockup design: owner decision 2026-11-22)

| Element | Source | Carrier | Status |
|---|---|---|---|
| Modal title "Send notification" | owner | `FormSpec.title` | N5 |
| Title (required, ≤200 bytes) | owner "title" → `title` column | `Field{key "title", FieldKind::String}` | N5 |
| Message (required, ≤4000 bytes) | owner "message" → `body` column | `Field{key "body", FieldKind::Text}` | N5 |
| Request id (read-only, mono) | wallet precedent `wad:455` | `FieldKind::RequestId` (`cp:836-840`) | N6 |
| Submit "Send" | owner | `FormSpec.submit_label` | N5 |

Shell frame on `player.inbox` and `player.inbox.menu.user`: copy the frame and entity-head lines from `tools/admindiff/accept/player.saves.txt` / `player.saves.menu.user.txt`; re-derive the tab-bar lines (Inbox now drawn and selected).

## 2. States per panel, and URLs

| Panel | ok | empty | readonly | stale | error |
|---|---|---|---|---|---|
| Inbox tab | entries + meta + tool | no rows: portal Empty (`pan:214`), tool kept (`when_empty`) | n/a: `writes: true`, no `readonly` | `cache_ttl_s: None`: n/a | error block |
| Tab count | `N` | `0` (a count is a cell) | n/a | n/a | count absent per portal |
| Purges pending | warn item while backlog | none at 0 (`cp:397-399`) | n/a | n/a | per point |

Narrowed to zero: n/a, no search or filter. URLs: only the "Older" link (`tiles:184`), built from `ctx.query` (stripped of `cursor`/`before`) plus `before`. Modal open/submit are the portal's form routes.

## 3. `writes` and every ActionDecl (proposal)

`writes: true`. All three share one `FormSpec` builder (N7) and one invoke arm.

| id | Point | Label / icon | order | Emphasis | danger | implemented | then |
|---|---|---|---|---|---|---|---|
| `notify` | `HeaderAction("player")` | "Notify" / `send` | 20 | Default | no | **true** | `Form(send_form())` |
| `send` | `RowAction("player")` | "Send notification…" / `send` | 60 | Default | no | **true** | `Form(send_form())` |
| `inbox.send` | `PanelTool("inbox")` | "Send notification" / `send` | 0 | Default | no | **true**, `when_empty: true` | `Form(send_form())` |

`min_role: Operator`. Ids ≤64 bytes. No `implemented: false`, so no new `admin-second-pass.md` item, unless N4 adds one for "Today".

## 4. Contributions and `order`

| Contribution id | Point | order | min_role | readonly | cache_ttl_s |
|---|---|---|---|---|---|
| `inbox` | `EntityTab{player, inbox, "Inbox", 1, []}` | 80 | Viewer | None | None |
| `inbox.count` | `EntityTabCount{player, inbox}` | 0 | Viewer | None | None |
| `attention.purges` | `DashboardAttention` | 67 | Viewer | None | None |

Wiring as storage: `controlplane` (+ `sha2`, N6) dependencies, `admin.rs`, `PROVIDER_SLOT` in `Notifications::init` (`nl:165`). admin-svc unchanged (§0.5).

## 5. Hand-check section

| # | Element | Check |
|---|---|---|
| H1 | Tab "Inbox" last, count = messages, no lock | live look |
| H2 | Meta text `"{n} · {u} unread"` | unit test of the meta builder |
| H3 | Entry: bold title, body after it, `div.meta` "{kind} · read/unread" | provider unit test + live look |
| H4 | Day label "Nov 22" where the mockup shows "Today" (N4) | live look; declared in the step report |
| H5 | Unread entry tone accent, read entries none; icon per kind | unit test |
| H6 | Time is UTC `HH:MM` | live look |
| H7 | Notify, row item and panel tool open the same modal with Title, Message, Request id | live look of all three |
| H8 | Over-long title/body shows a field error, not a 400 toast (N8) | unit test |
| H9 | A double submit of one form instance writes one row (Duplicate) | unit test against the store |
| H10 | Older link appears only past 25 entries | unit test |
| H11 | Purges pending (67) after deleting a player with inbox rows | golden or live look |

## 6. Gaps and decisions for the owner

**N1. Inbox reader and paging.**
- (a) **Recommended:** `page_by_player(player, before, 26)` (live, keyset `(created_at, id)`, `ns:72-130`), page 25. Decode `PanelQuery.before` with `decode_cursor` (`nsv`, a rejected cursor is `Invalid`). Set `cursor_before = encode_cursor(last.created_at, last.id)` only when a 26th row came back.
- (b) `recent(limit)`: cross-player and unpaged (`ns:286-312`); wrong scope for a per-player tab.

**N2. One timeline entry.** **Recommended:**
- `at = created_at`, `title = Some(title)`, `summary = body` (full: it is text, capped at 4000 bytes, not an identifier).
- `detail = "{kind} · unread"` or `"· read"` (read state is a state word, not a never-happened time, so not "—").
- `tone = Accent` iff unread (`pc:144`); `actor/verb/target/badges` empty.
- Icon by kind: `wallet.credit` → `gift`, `friend.*` → `heart`, `operator.mail` → `mail`, `account.promoted` and any other → `bell`.
- Alternative: one icon for all (loses the mockup's per-entry icon).

**N3. `player_stats`.** **Recommended: reuse** it for the meta and the `EntityTabCount` (two exact counts over `notifications_inbox_idx`, `ns:315-340`). **Delete** `recent` and `InboxRow` (`ns:53-63,286-312`): no element on this page lists messages across players. Alternative: count from the page (wrong past 25).

**N4. "Today" day label.** The portal labels every day "Mon D" (`tiles:128-137`); the controlplane vocabulary is frozen (SKILL Standing decisions).
- **Recommended:** accept as a hand-check (H4) plus a `docs/todo/admin-second-pass.md` item.
- Alternative: `days: false` (drops the labels, which removes an element; not allowed).

**N5. Form fields (owner decision 2026-11-22: "title, message").**
- **Decided:** Title → `title` (`FieldKind::String`, required, help "at most 200 bytes"). Message → `body` (`FieldKind::Text`, required, "at most 4000 bytes"). Plus Request id (`RequestId`, mono).
- `kind` is the fixed const `operator.mail` (the removed form's `KIND_OPERATOR_MAIL`). No Player field: the player is the entity, as wallet's row form (`wad:461-462`).
- Modal title "Send notification", submit "Send".

**N6. Dedup key.** `send_operator_mail` admits only `admin-send-mail-` + 32 hex (`nsv:110-113`); the portal sends `"<action>:<token>"` (§0.4).
- (a) **Recommended:** `OPERATOR_DEDUP_PREFIX` + the first 32 hex chars of `sha256(request_id)`, using the workspace `sha2`. It is deterministic, so a retry of one instance is a `Duplicate`; a missing `request_id` is `Error::invalid` (as `wad:228-232`).
- (b) `SELECT md5($1)` in SQL: 32 hex with no new dependency, but one extra round trip.
- (c) Loosen `is_operator_dedup_key`: not recommended; it is the guard that keeps operator keys from claiming a real event's row (`nsv:373-377`).

**N7. One form for all three openers.** **Recommended: yes.** Notify (header), Send notification… (row) and the Inbox tool all declare `Then::Form(send_form())` and invoke one arm with `entity = player:<id>`. The panel tool exists in the mockup (`pc:142`), so omitting it would drop an element.

**N8. Outcomes.** **Recommended:**
- Title/body over the cap → `InvokeOutcome::FieldErrors` keyed `title`/`body`, checked before the service. Otherwise `validate_new`'s `Invalid` reaches the operator as a toast.
- `Appended` and `Duplicate` → `Ok{message: "Notification sent"}`.
- `KeyReused` → refusal "this form was already sent with different text".
- `PLAYER_DELETED` (`nsv` tombstone path) → refusal passed through.

**N9. Wiring and split.** **Recommended:** as storage (`s19` §4, owner correction §11): no admin-svc change, since the peer exists (§0.5).

**N10. Purges pending.** **Recommended:** order 67, title "Notifications purges pending", sweep `projection::PURGE_SWEEP_SUB`.

**N11. The probe (`plan:2347-2348`).** `b8435c9a^:modules/notifications/src/conformance.rs` has no admin probe (only title/body/kind/dedup/cursor caps, lines 47-101), so nothing is restored. **Recommended:** a new `conformance_cp_invoke_input_rejected` through `NotificationsAdmin::invoke` over the dead pool (`nc:118` pool helper), on `gc:143-188`, plus its `tools/conformance` CapCase row.

**N12. Bell popover (`reg:114,198-200`).** **Recommended:** out of scope (`pages:587`).

## 7. Accept-file plan (predicted; exact text needs a crawl)

- `pages`:
  - Remove `("player.inbox", …Step 20)` from `EXCLUDED_TABS` (`pages:410`).
  - Add two `PAGE_MAP` entries `player`/`tab: Some("inbox")`, without and with `USER_MENU`, copied from the saves pair (`pages:358-371`).
  - `EXCLUDED_MOCK_MODALS`: copy the six `player.saves*` rows to `player.inbox*` (`pages:720-725`).
  - `EXCLUDED_MODALS`:
    - copy the wallet grant/revoke rows to host `player.inbox.menu.user` (`pages:518-525` pattern);
    - add `notifications/send` on every user-menu host (`players.menu.user`, `player.menu.user`, `player.{characters,wallet,inventory,friends,groups,saves,inbox}.menu.user`);
    - add `notifications/notify` on every player page/tab host (`player`, `player.{characters,wallet,inventory,friends,groups,saves,inbox}`);
    - add `notifications/inbox.send` on `player.inbox`;
    - `ModalWhy`: "the mockup's Notify / Send notification only toast 'Compose notification…' (registry.js:90,106; pages-core.js:142), so the mockup has no modal".
  - No `EXCLUDED_MENUS` row.
- **`player.inbox.txt`, `player.inbox.menu.user.txt`** (new): frame and entity-head lines from `player.saves*.txt`; data difference if the crawled player has fewer or more than 4 rows (Seed rule).
- **Every `player*.txt`**: delete the `div.page-actions … button.btn#0d6aae` Notify half (`player.groups.txt:16`, `player.characters.txt:15`, `player.friends.txt:16`, `player.saves.txt:16`, and the others) so that only Change Lore role remains. Delete the `button.tab#e4bdfb` Inbox line (`player.groups.txt:19` and siblings).
- **Every `.menu.user` file**: re-derive the `div.menu-group#6c5cf7`, `div.menu-sep` and `div.menu-item#211ef0` counts (`player.characters.menu.user.txt:11-13` and siblings), dropping "Send notification … arrive with their ports".
- Part `b` sweep first: `rg -n -i "inbox|notif|step 20|notify|0d6aae|e4bdfb|6c5cf7|211ef0" tools/admindiff/accept` (my sweep was cut at 6 KB).

## 9. Splitproof (20t, on the groups/storage admin-checks pattern)

`tools/splitproof/src/notifications_admin_checks.rs`, called from both topologies. Break run: admin-svc missing its `notifications` peer.
- **NA1:** player B; POST the `notifications/notify` form (title, body) for `player:B`; B's Inbox tab shows one entry with the bold title, `operator.mail · unread`, and tab count 1. In the split the provider runs in notifications-svc: this is the path most at risk.
- **NA2:** re-POST the same form instance → still one row (Duplicate).
- **NA3:** the row action and the panel tool render and open the same form.
- **NA4** (optional, unit test preferred): Purges pending after deleting B.

## 10. Part sizes (estimate)

| Part | Content | Files | Tool calls |
|---|---|---|---|
| 20a1 | `admin.rs` read side (manifest, tab, count, meta, timeline, purges), `lib.rs` slot, `Cargo.toml`, store deletions (N3), tests | 4-5 | ~40 |
| 20a2 | form, invoke arm, key derivation (N6), outcomes (N8), conformance probe + CapCase (N11), tests | 3-4 | ~35 |
| 20a3 | `pages.rs` (§7) | 1 | ~20 |
| 20t | splitproof NA1-NA3 | 2-3 | ~35 |
| 20b | crawls, accept files | 12-16 | ~35 |

## Not verified

- No cargo, crawl, `admindiff mock`, diff, splitproof or admincheck; every §7 line is a prediction.
- Whether `Tone` has an `Accent` variant (N2).
- How admincheck treats a `HeaderAction` or `RowAction` with `Then::Form` and `implemented: true` (wallet's row form is the precedent, `wad:960-969`; header-action forms have no precedent).
- Whether the crawled demoseed player has inbox rows.
- The `tools/conformance` CapCase table layout.
- `crate::new_token`'s exact shape (only that it follows `<action>:` per `forms:65`).
- The full accept sweep (output cut at 6 KB).
- `docs/todo/notifications.md` and `docs/todo/admin-second-pass.md` current items.

## 11. Owner decisions (2026-11-22)

Owner, 2026-11-22: the mockup does not design the Notify form; its fields are title and message (N5). Owner, 2026-11-23: every other §6 decision (N1-N4, N6-N12) as recommended.
