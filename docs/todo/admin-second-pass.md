# Admin second pass

Last updated: 2026-11-17

The admin redesign ships mockup elements the backend cannot back as deterministic fakes
(`// FAKE: <why>`, plan R1) and not-yet-possible actions as `implemented: false`, rendered
disabled (R2). This is the one list of them, so a second pass can replace each with a
real feature. Found with Grep over `*.rs` (`// FAKE:`, `implemented: false` and the
`.decl(.., false)` calls), which is a lower bound: a marker written another way is missed.
A port adds its own markers here in its part `a` commit (mockup-implementation skill);
a port that writes a new `contract gap` accept entry adds its item under Accepted contract gaps.
Rule source: docs/plans/2026-10-22-1759-admin-control-plane-redesign-plan.md:45-53.

## In progress

_None._

## Open

### Fake data

- [ ] **Deployment name, version and process count**
  ADMIN2-01. Mockup: top-bar deployment and the dashboard subtitle ("emberline-prod ·
  Swarog 0.42.1 · 20 processes (split topology)"). Fake: core/admin/src/fakes.rs:12,15
  (`DEPLOYMENT`, `DASHBOARD_SUBTITLE`): the portal has no deployment name and no
  version or process-count source. Replaced by: a deployment name in config and the
  fleet's version and process count from Weles.
- [ ] **Service health, probe interval, down/degraded notes and the wallet-down attention
  item**
  ADMIN2-02. Mockup: Service health panel (state, version, uptime, latency, note), top-bar
  health popover, "wallet-svc unavailable" attention. Fakes: core/admin/src/fakes.rs:18
  (`HEALTH_PROBE_S`, and its `health_note`/`health_probe` strings), :35 (`services()`),
  :159 (`attention_services()`): no health probe exists until Weles exposes its API.
  Replaced by: real per-process probes from the Weles API.
- [ ] **Service health history bar**
  ADMIN2-03. Mockup: the 48-bucket uptime bar per service. Fake:
  core/admin/src/fakes.rs:81 (`history`): no health history is stored. Replaced by: a
  stored probe history (needs ADMIN2-02 first).
- [ ] **Notification bell**
  ADMIN2-04. Mockup: the top-bar bell's five entries. Fake: core/admin/src/fakes.rs:98
  (`notifications()`), used at core/admin/src/routes.rs:140: no operator feed exists.
  Replaced by: an operator feed fed by health, the event plane and audit.
- [ ] **Lore tiles and stale-locks attention**
  ADMIN2-05. Mockup: "Lore seats" and "Lore store used" tiles, "2 stale Lore locks"
  attention. Fakes: core/admin/src/fakes.rs:113 (`lore_seats`), :127 (`lore_store`), :171
  (`attention_lore`): there is no Lore module. Replaced by: a Lore module, or dropping the
  elements by owner decision.
- [ ] **Matches in progress tile**
  ADMIN2-06. Mockup: "Matches in progress" KPI. Fake: core/admin/src/fakes.rs:141
  (`matches_in_progress`): `match.matches` holds finished matches only. Replaced by:
  in-flight match tracking in the match module.
- [ ] **Scheduler failure attention item**
  ADMIN2-07. Mockup: "storage.gc failed at 03:30". Fake: core/admin/src/fakes.rs:183
  (`attention_scheduler`): the scheduler stores no run failures. Replaced by: recorded
  run outcomes in the scheduler.
- [ ] **Event plane head sequence, append rate and checkpoints**
  ADMIN2-08. Mockup: Head sequence KPI with its rate, and each subscription's Head and
  Checkpoint numbers. Fakes: core/admin/src/fakes.rs:195 (`HEAD_SEQUENCE`), :198
  (`HEAD_RATE`), :205 (`checkpoint`, used at events_provider.rs:442): the event log has a
  (generation, xid, tie) position, not a sequence number, and no append rate is measured.
  Replaced by: a display ordinal for the log position and a measured append rate.
- [ ] **Dead-lettered count and who skipped**
  ADMIN2-09. Mockup: "Dead-lettered (7 d)" KPI, "all skipped by operator". Fakes:
  core/admin/src/fakes.rs:201 (`DEAD_LETTERED`), core/admin/src/events_provider.rs:697:
  skipped events are not kept. Replaced by: a stored record of skipped events with the
  operator.
- [ ] **Subscription consumer module and handler name**
  ADMIN2-10. Mockup: the Consumer column and "module · handler" line. Fakes:
  core/admin/src/fakes.rs:211 (`consumer`, first dotted segment of the id), :216
  (`handler`, hash-picked): neither is stored. Replaced by: the subscription registry
  recording its module and handler.

### Fake data and actions of the mail port

modules/mail/src/admin.rs landed in 544dd4e5; the line numbers are its committed ones.

- [ ] **Relay throttle signal**
  ADMIN2-11. Mockup: the "▲ relay throttled" tile delta, the "Mail relay throttling"
  attention item and the SMTP 421 banner. Fakes: modules/mail/src/admin.rs:336, :366,
  :441: no throttle signal exists; a retrying row stands for one. Replaced by: the mail
  drain recording relay throttling (SMTP 421) as a state.
- [ ] **Queue-length history**
  ADMIN2-12. Mockup: the Mail queue tile's sparkline. Fake: modules/mail/src/admin.rs:351:
  the outbox keeps no queue-length history. Replaced by: sampled queue length.
- [ ] **Pause sending**
  ADMIN2-13. Mockup: the outbox "Pause sending" tool. Action `PAUSE_SENDING`,
  `implemented: false`: modules/mail/src/admin.rs:289-300, refused at :568; the drain has
  no pause switch. Replaced by: a persisted pause flag the drain honours.
- [ ] **Retry now**
  ADMIN2-14. Mockup: the per-row "Retry". Action `RETRY_NOW`, `implemented: false`:
  modules/mail/src/admin.rs:302-303; the drain owns every retry time. Replaced by: a
  per-row "due now" operation on the outbox.
- [ ] **Park and cancel parked**
  ADMIN2-15. Mockup: "Park: keep but stop retrying" and the parked row's "Cancel".
  Actions `PARK` and `CANCEL_PARKED`, `implemented: false`: modules/mail/src/admin.rs:304-306
  and :313; no operator park exists (resume parked is real) and `cancel` refuses a row
  that is not pending. Replaced by: an operator park transition and a cancel for parked rows.

- [ ] **Define mail "retrying" so a healthy first send is not one**
  ADMIN2-19. "Retrying" is `state = 'pending' AND attempts > 0`
  (modules/mail/src/store.rs:436 and :484, modules/mail/src/admin.rs:138), and the claim
  bumps `attempts = attempts + 1` (store.rs:300), so a healthy first send counts as
  retrying while its lease is held. In that window the FAKE throttling banner, the Mail
  queue tile delta and the dashboard attention item flash (modules/mail/src/admin.rs:336,
  :366, :441; the tile's `stats.retrying > 0` test at :334). Owner (2026-11-17): define it
  differently or make the threshold configurable. Options: `attempts >= 2`; a typed
  config threshold on mail; require a recorded `last_error` (a failed attempt writes it,
  store.rs:369). Whichever is chosen must change the count, the Retrying tab and the
  admin.rs:138 tab key together. Source: review of commits b03a62fa..5d5e6973 (2026-11-17).

### Not-implemented actions

- [ ] **Copy payload**
  ADMIN2-16. Mockup: the poison-event "Copy payload" tool. Action `POISON_COPY`,
  `implemented: false`: core/admin/src/events_provider.rs:315-316; copying to the
  clipboard is a client action no backend call performs. Replaced by: a client-side
  copy handler in `admin.js`, or an owner decision to drop the button.
- [ ] **Rewind checkpoint**
  ADMIN2-17. Mockup: the subscription row's "Rewind checkpoint…". Action `SUB_REWIND`,
  `implemented: false`: core/admin/src/events_provider.rs:320-322; no checkpoint rewind
  exists. Replaced by: a rewind operation in the event plane (a fenced, danger-level
  write).

### Fake data and actions of the accounts port

modules/accounts/src/admin.rs, as committed in admin-control-plane-redesign step 13 part a.

- [ ] **Player country**
  ADMIN2-21. Mockup: the Profile panel's Country. Fake: modules/accounts/src/admin.rs:188
  (`fake_country`, a hash of the player id): no country is stored for an account.
  Replaced by: a country recorded at registration or login.
- [ ] **Identity last used**
  ADMIN2-22. Mockup: the Linked identities Last used column. Fake:
  modules/accounts/src/admin.rs:193 (`fake_last_used`): no identity records its last use.
  Replaced by: a last-used time written on each login through the identity.
- [ ] **Session client and IP**
  ADMIN2-23. Mockup: the Active sessions Client and IP columns. Fakes:
  modules/accounts/src/admin.rs:199 (`fake_client`) and :214 (`fake_ip`), hashes of the
  refresh family id, never the token: a refresh family stores neither. Replaced by: the
  client and address recorded when a family is born.
- [ ] **Active players delta and sparkline**
  ADMIN2-24. Mockup: the "Active players (24 h)" tile's "▲ 6.1% vs prev. day" and its
  sparkline. Fakes: modules/accounts/src/admin.rs:478 and :484: no daily history of active
  players is kept. Replaced by: a sampled daily active-player count.
- [ ] **Export CSV**
  ADMIN2-25. Mockup: the Users & players "Export CSV" head action. Action `EXPORT_CSV`,
  `implemented: false`: modules/accounts/src/admin.rs:406; no export exists. Replaced by:
  an export of the filtered list.
- [ ] **Create account**
  ADMIN2-26. Mockup: the "Create account" head action. Action `CREATE_ACCOUNT`,
  `implemented: false`: modules/accounts/src/admin.rs:408; an account is created only by
  the player's own registration. Replaced by: an operator create path.
- [ ] **Suspend account**
  ADMIN2-27. Mockup: the row menu's "Suspend account…" and the player header's "Suspend".
  Actions `SUSPEND` and `SUSPEND_HEADER`, `implemented: false`:
  modules/accounts/src/admin.rs:410 and :413-421; no suspension exists, so every account's
  status is the constant `active`. Replaced by: a suspension state the session checks honour.
- [ ] **Delete account by an operator**
  ADMIN2-28. Mockup: the row menu's "Delete account…". Action `DELETE_ACCOUNT`,
  `implemented: false`: modules/accounts/src/admin.rs:412; deletion is the player's own
  two-step flow. Replaced by: an operator path into the same deletion.
- [ ] **Edit profile**
  ADMIN2-29. Mockup: the Profile panel's "Edit". Action `EDIT_PROFILE`,
  `implemented: false`: modules/accounts/src/admin.rs:423; the display name is immutable
  (ACCOUNTS-09). Replaced by: editable profile fields.
- [ ] **Link and unlink identity**
  ADMIN2-30. Mockup: the Linked identities "Link" tool and each row's "Unlink". Actions
  `LINK_IDENTITY` and `UNLINK_IDENTITY`, `implemented: false`:
  modules/accounts/src/admin.rs:425 and :427; linking is the player's own flow and no
  unlink exists. Replaced by: operator link and unlink, refusing the last identity.
- [ ] **Revoke sessions**
  ADMIN2-31. Mockup: the Active sessions "Revoke all" tool and each row's "Revoke".
  Actions `REVOKE_ALL` and `REVOKE_SESSION`, `implemented: false`:
  modules/accounts/src/admin.rs:429-430; no operator revocation exists. Replaced by: an
  operator call into the family revocation (`Store::kill_family_tx`).
- [ ] **Players total at scale**
  ADMIN2-32. The players page total and the nav counter run an exact `count(*)` over
  `accounts.players` on every load (`Store::player_count`, modules/accounts/src/store.rs),
  and each list page sorts every player, with no index on `created_at` (plan R1). Owner
  decision 2026-11-17 (step 13 G6): exact for now. Replaced by: a `pg_class.reltuples`
  estimate past a size threshold, and an index if the keyset paging needs one.

### Guard gaps

- [ ] **Unlisted page-head action renders nowhere**
  ADMIN2-20. The page head draws only the ids in `PageDecl.head_actions`
  (core/admin/src/routes.rs:487-493), so a `PageHead(p)` action that page `p` does not
  list renders nowhere. admincheck checks each listed id (tools/admincheck/src/checks.rs:135-152)
  but does not refuse an unlisted `PageHead` action. Closed by: an admincheck finding for
  it. Source: the 593010bf hand-off.

### Contract gaps admindiff cannot see

- [ ] **Per-page toolbar search width** (low priority, owner 2026-11-17)
  ADMIN2-35. Mockup: `searchWidth: 280` for players (UILayout/redesign/assets/pages-core.js:98),
  260 for the mail outbox (pages-platform.js:120), 220 elsewhere. The portal's `table_toolbar`
  hard-codes `style="width:220px"` (core/admin/src/templates/primitives.html.tmpl:189), so the
  players box is 60 px and the outbox box 40 px narrower than the mockup. admindiff compares
  no attributes, so no accept entry covers it. Found in step 13 part b. Closed by: a
  `DataTable` search-width field in `core/controlplane` that the template reads, set by
  accounts and mail.

## Accepted contract gaps

One item per `contract gap` entry in `tools/admindiff/accept/*.txt`; remaining mockup
gaps go to the next iteration.

- [ ] **Muted mono value (Event plane Head cell)**
  ADMIN2-18. Accept lines: tools/admindiff/accept/events.txt:15 and :16 (the plain and
  the `tr.sel` row), `td.num: mock span.mono.muted portal span.mono | contract gap: no
  CellKind gives a muted mono value, so the Head cell is not muted`. Mockup: the
  Subscriptions Head value in dimmed mono. Closed by: a muted variant in
  `core/controlplane` (a `CellKind` or a `Cell` flag) that `core/admin/src/render.rs`
  maps to `span.mono.muted`; then delete both accept lines.

- [ ] **Players list column chooser ("Columns: 9 of 12")**
  ADMIN2-33. Accept entry: the `contract gap` line for the toolbar's "Columns:" label and
  "9 of 12" button in tools/admindiff/accept/players.txt (written by step 13 part b).
  Mockup: pages-core.js:100. No column chooser exists and `DataTable` has no toolbar-right
  carrier (owner decision 2026-11-17, step 13 G5). Closed by: a column chooser in
  `core/controlplane` and the portal; then delete the accept line.
- [ ] **Personal data panel**
  ADMIN2-34. Accept entry: the `contract gap` mock-only Personal data panel in
  tools/admindiff/accept/player.txt (written by step 13 part b). Mockup: pages-core.js:120,
  a panel denied by the permission `accounts.pii.read`. No PII store exists, and the owner
  declined a `Forbidden` → denied panel mapping (2026-11-17, step 13 G9), so accounts
  contributes no such panel. Closed by: a PII store and a permission-denied panel state;
  then delete the accept line.

## Mail nav counter with only parked rows

With only parked rows the mail nav counter shows a warn badge reading "0". That matches
the Step 11 checklist (the count is queued pending plus retrying; the warn tone comes
from retrying plus parked), but it reads oddly. Option: show the parked count when
nothing is queued. Owner design decision. Source: b5eb40d5.

## Blank filter value reads as a filter on an empty outbox

`filter_views` (`core/admin/src/render.rs:881`) maps only a missing param to `"any"`, but
the providers treat both `""` and `"any"` as unset (`modules/mail/src/admin.rs:402`,
`core/admin/src/events_provider.rs:432`). A hand-typed `?template=` on an empty outbox
shows "No matching items" with a blank chip instead of "Queue is empty". Fix: map an empty
value to `"any"` at `render.rs:881` (a one-liner), with a golden case. Source: review of
7bb53152, 2026-11-17.

## Data a dev fleet never produces

`data difference` accept entries in `tools/admindiff/accept/*.txt` that are real data
the fleet lacks, not fakes, for the second pass to decide on (seed, or accept for good).
At the time of writing no entry in `index.txt` or `events.txt` has that kind: they are
`not ported yet`, `template choice`, `contract gap` or a declared deviation, and no mail
accept file exists yet. A port's `data difference` entries are listed here in its part
`b` or `a` commit (plan Step 24b is where seeding is decided).

_None._
