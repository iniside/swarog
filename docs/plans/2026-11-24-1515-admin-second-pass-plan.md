# Admin second pass: remove fakes and dead buttons, make the rest real

Status: Steps 1-10 approved by the owner 2026-11-24; later steps wait for the owner

Plan slug: admin-second-pass. Draft location until approval: C:\Users\lukas\.claude\plans\admin-second-pass.md;
after approval it is copied to docs/plans/2026-11-24-1515-admin-second-pass-plan.md.

## Context

Owner request (2026-11-24): the panel ports left fake data and hard-coded UI that does
nothing; "trzeba zdecydowac czy je zaimplementowac czy pousuwac i poprawic mockup, potem bledy".
Ruling on the grouped report: "Tozsamosci: Last used; sesje: Client i IP - zrobic, reszta mozna
zrobic zgodnie z rekomendacja. poprawic mockup i poprawic odrazu admin portal." Scope limit:
"jak cos to narazie robimy tylko punkt 1a do 1c".

In scope: report sections 1a (fake data), 1b (dead buttons), 1c (mockup elements the portal
does not draw). Each item is fixed in the mockup (UILayout/redesign/) and the portal in the
same step.

Excluded (owner): bugs (report section 2, docs/todo/admin.md) and the rest (section 3).
Excluded by the approved recommendation, stay as they are: service health, its history and
the wallet-down attention (ADMIN2-02, 03, wait for the Weles API); mail Pause sending (13);
Suspend account (27); operator Delete account (28).

Item ids are docs/todo/admin-second-pass.md ADMIN2-NN. Line anchors below were read by the
research agents on HEAD 0b2b8287; an implementer re-reads before editing.

### Rulings applied (approved recommendation)

Remove from portal and mockup: Lore tiles and stale-locks attention (05); notification bell
(04); scheduler "storage.gc failed" attention and Target / Last result / Enabled / duration /
note columns (07); Matches in progress tile (06); event-plane append rate, Dead-lettered KPI,
"skipped by operator", fake head sequence and checkpoint (08, 09), Rewind checkpoint (17);
audit actor options scheduler and apikey:ak_c4a8 (70); API key Last used and Requests 24 h
(59, 62); mail relay-throttled delta, banner and attention (11); sparklines of the mail queue,
cloud saves and active players plus the delta (12, 52, 24); player Country (21); character
Level, Region, Last played (48-50); scheduler + Schedule / Edit / Delete (64, part); Create
account, Edit profile, Link / Unlink identity (26, 29, 30); Delete cloud saves (56); mockup
column chooser (33) and Personal data panel (34).

Make real: config revision history and writer (65, 66); API key Created by (58); deployment
name and build version, process count dropped (01); identity Last used (22); session Client
and IP (23); subscription consumer (10, see D4); Revoke session / Revoke all (31); inventory
Grant / Revoke item (INVENTORY-05); friends Remove friend (FRIENDS-08); scheduler Fire now
(64, part); mail Retry now, Park, Cancel parked (14, 15); Copy payload (16); mail "retrying"
definition (19).

### Decisions this plan takes (owner can overrule at approval)

- D1 Downloads. No download carrier exists in the control plane. Export CSV (25), audit
  Export (69) and the three cloud-save downloads (53-55) are removed from portal and mockup
  now. Owner 2026-11-24: the download carrier is a separate plan; Step 25 records it as one
  TODO item that names the five buttons to bring back with it.
- D2 Session origin carrier. A typed value opsapi::Origin { ip, client } travels next to
  Identity: envelope field in core/edge wire::Request, an argument of Caller::call and
  LocalInvoker, and an rpc-macro trait parameter kept out of the request body, exactly like
  Identity. Why not the request payload: every payload field is client-fillable, so an origin
  there is spoofable. Why not a task-local: none exists, it does not cross tokio::spawn (push
  invoke), and core/opsapi/src/lib.rs:22-28 rejects ambient context. Why not widen Identity:
  the session-creating ops are auth = "none" and Identity means a verified player. No existing
  per-call mechanism carries anything else (no trace id, request id or deadline on the op path).
  The type holds IP and client string only; it is not a growing request context.
- D3 Session origin semantics. Stored at family creation only; refresh does not update it and
  copies it to the successor row. Columns client and ip on accounts.refresh_tokens. The player
  QUIC front has no User-Agent: its client value is the constant "QUIC client". The Epic
  callback is an HTTP route of accounts itself, so accounts needs the trusted proxy set: a
  typed field of the accounts config, filled in cmd/server and cmd/accounts-svc from
  TRUSTED_PROXY_CIDRS with httpmw::parse_cidrs (today only core/app/src/lib.rs:1186 and
  core/admin/src/lib.rs:403 read that env; tools/processctl/src/fleet.rs:899 sets it for
  admin-svc only). fleet.rs sets it for accounts-svc too.
- D4 Subscription consumer. SubscriptionSpec's documented id convention is
  <consumer>.<topic-kebab>.v<N> (core/bus/src/lib.rs:683) and all 40+ production ids follow it,
  so the Consumer column is the id prefix, a derived fact, and loses its FAKE marker. No handler
  name exists anywhere (TxHandler has only call), so the handler half is removed from portal
  and mockup. Alternative rejected: a SubscriptionSpec field plus a catalog column, a contract
  change at every on_tx site for a label.
- D5 Identity Last used means the last session created with that identity; a refresh does not
  bump it.
- D6 Mail "retrying" is state = 'pending' AND last_error IS NOT NULL (a failed attempt writes
  last_error, modules/mail/src/store.rs finish_tx Retry arm; Sent clears it). Pending becomes
  pending AND last_error IS NULL. Retry now bumps generation like requeue_parked_tx: an
  in-flight attempt then loses its write and the message can go out twice, which the module's
  at-least-once delivery contract already allows.
- D7 Friends operator removal. The event shape stays: Removed has two party slots
  (api/friends/events/src/lib.rs:114-121) and both must name the edge's players, so the
  operator is not in the event. Like groups (api/groups/events/src/lib.rs:39-41), the acting
  human is named only by the portal's admin.action row. friend.removed gains one additive
  reason value, "operator-removed"; for it actor_id is the edge's requester and other_id the
  addressee, and the doc comment says neither acted. The contract already tells consumers to
  treat an unknown reason as a plain removal, so this is additive: no new version, no DROP.
  The action applies to accepted friendships only; a pending row carries no action, because
  a consumer cannot tell which counter a pending ending decrements without the acting side.
- D8 Scheduler Fire now skips the due re-check, stamps last_fired (so the next interval counts
  from the manual fire), is refused when the scheduler is disabled in the hosting process, and
  answers Conflict when the advisory lock is held.
- D9 Copy payload needs a client-only action kind. controlplane::Then gains Copy (no backend
  call, never admitted or audited); it copies the panel's drawn code block, which is the
  truncated preview. This is an addition to the vocabulary the port skill freezes; the freeze
  is for ports, this plan is not a port.
- D10 API key "Requests by this key" (60) is removed from portal and mockup: no store records
  a request per key, so the link can only show operator actions.
- D11 Eight "mockup element not drawn" items (1c) are deferred, not done: config restore
  link (39), pending-change preview (68), and 35, 57, 61, 63, 67, 71. Each needs a new
  carrier in the contract. Owner 2026-11-24: leave them and keep them in the TODO; portal
  and mockup stay as they are for these.
- D12 Deployment name comes from the env ADMIN_DEPLOYMENT_NAME, read once into core/admin's
  typed config next to ADMIN_OPEN (core/admin/src/lib.rs:1064); unset shows "—". Version is
  env!("CARGO_PKG_VERSION") of the workspace (0.1.0). A dev-seeded API key shows the creator
  "dev seed"; a key written by a non-admin path shows "system".
- D13 admindiff. Owner 2026-11-24: "admindiff is left alone for now"; its lib tests already
  fail (5) and the crawl stops on page-map drift. This plan edits nothing under tools/admindiff
  and runs no crawl or diff. Mockup removals make accept lines stale and turn
  mock_tests.rs:675-683 red (Personal data panel); Step 25 records that as one TODO item.
  Gates used instead: crate tests, core/admin goldens, admincheck, conformance, splitproof.
- D14 Inventory grant is repeat-safe through a new table inventory.operator_grants(request_id
  primary key, at): the grant transaction inserts the request id first and answers NoOp on a
  conflict. Nothing else can hold it: grant_starter is exactly-once only through the delivery
  checkpoint (modules/inventory/src/projection.rs:103-155) and grant_exec adds to the stack on
  conflict (store.rs:151-155). The form declares a RequestId field, which is what fills
  InvokeCall.request_id (core/controlplane/src/lib.rs:491-493, 844-848).
- D15 InvokeCall.operator is stored by Steps 9 and 10. Its contract comment says "carried only
  as an audit label" (core/controlplane/src/lib.rs:486, 506); Step 9 rewrites that comment. It
  already round-trips over a loopback edge (core/controlplane-rpc/src/tests.rs:217-274).

### Overlap (why extend, not add)

- Mail: sibling ops of requeue_parked_tx / cancel_pending_tx (store.rs:573, 642); no new state.
- Scheduler: fire / fire_on / lock_key (lib.rs:418-528) reused; only the due re-check is skipped.
- Accounts: kill_family_tx (store.rs:877) reused for Revoke session.
- Inventory: grant_exec, item_exists_exec, lock_player_tx, is_player_purged_tx reused; the
  dev-gated identity-bound Holdings::grant is not.
- Friends: the Removed event gets a reason value, no second topic; view_edge and delete_tx get
  an optional caller bound, not operator copies.
- Accounts Revoke all: delete_player_tx (store.rs:587) already ends a player's sessions inside
  the account deletion; Step 13 extracts that part into the one function both call.
- Config history: the config.changed event log is not the source, because its retention is a
  delivery policy and the panel must show history for as long as the setting exists.
- Clipboard: the existing [data-copy] handler (admin.js:347) is generalised, no second handler.
- X-Forwarded-For trust: httpmw::client_ip and parse_cidrs are used as they are; the gateway
  already holds the parsed set in PushLimits::trusted_proxies.
- Write-action reference pattern for every Phase C step: groups membership.promote
  (modules/groups/src/admin.rs:297-318, one transaction, NoOp on repeat, prepare for Confirm)
  and mail resume (modules/mail/src/admin.rs:339, 584). A module that flips Manifest.writes to
  true needs a CapCase "cp.invoke <module> input value" in tools/conformance/src/policy.rs,
  the rationale mention at policy.rs:180 and input-fields.golden.tsv rows
  (control_plane_cap_findings, checks.rs:229-242).

## Rules for every step

- Mockup and portal change in the same step; the mockup-implementation skill's standing data
  rules apply (no invented element, never-happened value is "—", times computed in SQL).
- Removal follows the dying chain: constant, fake helper, cell, column, test, drawn_orders.txt
  line, splitproof assertion, mockup markup and data.js sample go together. A removed pub
  constant in core/controlplane/src/order.rs or a changed pub item of any api crate also
  changes docs/reference/public-api-baseline/<crate>.txt: regenerate it in the same commit
  with cargo run -p verifyctl -- --bless-public-api and read the diff.
- Tests in the same step, bar "revert and the test fails". core/admin goldens are re-blessed
  only where the step says so (ADMIN_BLESS=1 cargo test -p admin golden) and the diff is read.
- Schema changes are create-only DDL edited in place; rollout is DROP SCHEMA <name> CASCADE and
  a fresh boot.
- cargo fmt is not safe here: hand-format. Comments default none. Stage by explicit path.
- One rollout-bearing command at a time, per the safe-verification skill. Step gate: tests of
  the touched crates, plus cargo run -p admincheck when a manifest changes.
- The step commit moves its closed ADMIN2 / module items to the topic's -done.md file.
- Review: a fresh core-reviewer after each step commit, model at or above the author's tier;
  proof-auditor too when the step touches goldens, conformance, splitproof or contract goldens.

## Phase A: removals

### Step 1: dashboard Lore and Matches tiles (05, 06)
(a) core/admin/src/fakes.rs:108-148,166-175; portal_provider.rs:15-19,59-76,87,103-107;
core/controlplane/src/order.rs:17,18,23,30 and public-api-baseline/controlplane.txt;
tools/admincheck/src/testdata/drawn_orders.txt:12,24-25,30; mockup registry.js:50-51,56,63.
The catalog demos (pages-core.js:385, catalog.js:107) keep their sample tiles: they
demonstrate the tile primitive, not the product.
(b) First: smallest removal, proves the step recipe on core/admin before the golden-wide bell.
(c) Delete the tile and attention contributions and their fakes; dashboard golden re-blessed.
(d) [sonnet] core-implementer.

### Step 2: notification bell (04)
(a) fakes.rs:92-102; routes.rs:31,139-150; render/shell.rs:91,111;
templates/shell.html.tmpl:40,79-81; golden_tests/build.rs:276; all nine screen_*.html goldens;
mockup registry.js:143-148,183,227-229,257.
(b) After Step 1 so only one step at a time re-blesses the shared goldens.
(c) Remove the bell and its popover from the shell view model and template; re-bless; the
golden diff must show only the bell markup going away.
(d) [sonnet] core-implementer; proof-auditor on the golden diff.

### Step 3: scheduler fakes and Schedule / Edit / Delete (07, 64 part)
(a) modules/scheduler/src/admin.rs:27-57,122-175,190-230,259,276,365-408 and admin_tests.rs;
core/controlplane/src/order.rs:32 (dashboard_attention::SCHEDULER) and
public-api-baseline/controlplane.txt;
drawn_orders.txt:14,76-77; tools/splitproof/src/scheduler_admin_checks.rs:13,23,132-174;
mockup pages-platform.js:77-82, data.js:189-202, registry.js:65.
(b) Before Step 15 (Fire now), which builds on the cleaned manifest.
(c) Columns left: name, interval, last fired, next due, all read from scheduler.schedules.
Fire now stays declared and disabled until Step 15.
(d) [sonnet] core-implementer; proof-auditor (splitproof assertions change).

### Step 4: event plane (08, 09, 10, 17)
(a) core/admin/src/fakes.rs:178-214; events_provider.rs:50,62,283,305,336-338,448-455,524,
613,690-715,774; drawn_orders.txt:81; mockup pages-platform.js:134-143, data.js:151-156.
(b) Independent of 1-3; placed here to finish core/admin removals before module steps.
(c) Head KPI shows no fake sequence or rate; the subscription row shows the real cursor
(asyncevents::admin::SubInfo.cursor, lag_events) where the fake checkpoint was; Dead-lettered
KPI and "skipped by operator" go; SUB_REWIND goes with its invoke arm; consumer keeps the id
prefix without a FAKE marker and the handler line goes (D4). Mockup shows the cursor triple
and the consumer only.
(d) [independent] core-implementer (core/admin provider logic, not a pure deletion).

### Step 5a: audit removals (70, 69)
(a) modules/audit/src/admin.rs:51-53,129-140,381 and its invoke arm, admin_tests.rs:283-295,
370,392-393; mockup pages-platform.js (audit actor options and the Export button).
(b) Independent of every other step.
(c) Actor options come only from stored rows; the audit page loses Export (D1).
(d) [sonnet] core-implementer.

### Step 5b: API key removals (59, 60, 62)
(a) modules/apikeys/src/admin.rs:70-73,426-436,465,494,722-723,815,882 and
admin_tests.rs:255-261,339-424; mockup pages-platform.js (keys table, revoke modal, row menu).
(b) Before Step 9, which edits apikeys again.
(c) Keys table loses Last used and Requests, the revoke modal loses its impact rows, the row
menu loses "Requests by this key" (D10).
(d) [sonnet] core-implementer.

### Step 6: mail fakes and the retrying definition (11, 12, 19)
(a) modules/mail/src/admin.rs:31,64,148-152,304-310,351-405,470-480,574;
store.rs:431-433,480-482; admin_tests.rs; core/controlplane/src/order.rs:31 and
public-api-baseline/controlplane.txt; drawn_orders.txt:13; mockup registry.js:53,64,148,
pages-platform.js:60, data.js:111,141-142.
(b) Before Step 16 (mail actions), whose row guards use the new definition.
(c) D6 predicate in the tab predicates, the stats count and shown_state together. The tile has
no delta and no sparkline; the throttle banner and the fake attention item go. A test claims a
row for its first send and asserts it is not retrying.
(d) [independent] core-implementer.

### Step 7a: accounts fake data (21, 24)
(a) modules/accounts/src/admin.rs:41,45-47,157-160,598,685 (Country, the active-players
sparkline and delta) and their tests in admin_tests.rs; mockup pages-core.js:100,104,
data.js:12-17, registry.js:49.
(b) Before 7b and Steps 12, 13 and 23c, which all edit accounts admin.rs.
(c) Players table loses Country; the active-players tile has no sparkline and no delta.
(d) [sonnet] core-implementer.

### Step 7b: accounts dead buttons and mockup-only elements (25, 26, 29, 30, 33, 34)
(a) modules/accounts/src/admin.rs:358,404-441,486-500,760-761 and admin_tests.rs:14,367;
mockup pages-core.js:119-122 plus the column chooser and the Personal data panel.
(b) After 7a (same file).
(c) Remove Export CSV (D1), Create account, Edit profile, Link / Unlink identity and their
forbidden arms; Suspend, Delete account, Revoke session and Revoke all stay declared. Mockup
also loses the column chooser and the Personal data panel.
(d) [sonnet] core-implementer.

### Step 8a: characters removals (48-50)
(a) modules/characters/src/admin.rs:35-39,146-154,201-203,219-221, admin_tests.rs,
tools/splitproof/src/characters_admin_checks.rs:141; mockup pages-core.js:163.
(b) Independent of every other step.
(c) Characters tab matches the mockup's four columns (Name, Class, Created, Character id).
(d) [sonnet] core-implementer; proof-auditor (splitproof assertion changes).

### Step 8b: storage removals (52, 53-56)
(a) modules/storage/src/admin.rs:38,219-250,318-326, admin_tests.rs:127-135,194,367,
tools/splitproof/src/storage_admin_checks.rs:111; core/controlplane/src/order.rs:95 and
public-api-baseline/controlplane.txt; mockup registry.js:52 and the download buttons.
(b) Last removal; no later step depends on it.
(c) Storage loses the sparkline, Delete and the three downloads (D1).
(d) [sonnet] core-implementer; proof-auditor (splitproof assertion changes).

## Phase B: data made real

### Step 9: API key Created by (58)
(a) modules/apikeys/src/lib.rs:63-85 (keys DDL), admin.rs:128,421,455,929, store read, dev
seed, admin_tests.rs; mockup unchanged.
(b) After Step 5b (same file). First step that stores InvokeCall.operator (D15).
(c) Column created_by text NOT NULL, written from req.operator by the create invoke; dev seed
writes "dev seed" (D12). The operator contract comment in core/controlplane/src/lib.rs:486,506
is rewritten (D15). Rollout: DROP SCHEMA apikeys CASCADE.
(d) [independent] core-implementer.

### Step 10: config revision history and writer (65, 66)
(a) modules/config/src/lib.rs:108-156,318,338 (DDL, Service::set, set_tx), store.rs:116,128,
admin.rs:164,189,299,307,345,464,621, tests; mockup unchanged (it already draws real-looking
history).
(b) Independent; after Step 9 so the operator-carrier proof is in.
(c) New table config.setting_history(namespace, key, revision, value, operation, by, at),
one row written in the same transaction as every create, update and delete of a setting; the
revision is the one the settings_stamp trigger assigned. Admin writes pass req.operator;
Service::set / set_tx write "system". Revisions panel reads the newest N rows for the key and
drops fake_by / fake_prior. Invariant test: after k writes of a key the history holds k rows
with strictly increasing revisions and the newest equals config.settings. A delete's row
carries the next global revision (greater than the deleted row's revision, equal to the
config.changed revision) and operation delete; operations are insert, update, delete. The history records writes
made through the module; a hand edit in psql is outside it, like every other invariant.
Rollout: DROP SCHEMA
config CASCADE.
(d) [independent] core-implementer.

### Step 11: deployment name and version (01)
(a) core/admin/src/fakes.rs:8,11, lib.rs:1064 (config read), routes.rs:139,425, shell goldens;
mockup registry.js top bar and the dashboard subtitle.
(b) After Step 2 (same goldens).
(c) D12. Subtitle "<name> · Swarog <version>"; no process count, no topology. Golden harness
passes a fixed name.
(d) [sonnet] core-implementer.

### Step 12: identity Last used (22)
(a) modules/accounts/src/lib.rs:164-171 (identities DDL), 279-300 (issue_session_tx), 360,
916, 938 (callers), store.rs:327-332,602,1108, admin.rs:163-166,681, epic_oauth.rs:388, tests.
(b) After Step 7b; before Phase D, which changes the same session-creation funnel.
(c) Column last_used_at timestamptz NULL. issue_session_tx takes the identity key and stamps
it in the caller's transaction, so every session-creating path (register, login, federated,
guest, Epic) stamps and refresh does not (D5). Never used shows "—"; the relative time is
computed in SQL. Rollout: DROP SCHEMA accounts CASCADE.
(d) [independent] core-implementer.

## Phase C: actions made real

Each step: declare implemented, flip writes where needed, one transaction per write, NoOp on a
repeat (except Fire now, D8: a repeat fires again), the mockup's matching control and toast
updated in the same step, prepare for Confirm actions, unit tests for success / NoOp / refusal, conformance
CapCase for a module that becomes a writer.

### Step 13: accounts Revoke session and Revoke all (31)
(a) modules/accounts/src/admin.rs:306-325,443-444,452,689,758, store.rs:877 and 587
(the session-ending part of delete_player_tx extracted as kill_player_families_tx, called by
both), conformance.rs, admin_tests.rs; tools/conformance/src/policy.rs and
input-fields.golden.tsv.
(c) Revoke session: kill_family_tx, 0 rows is NoOp "no longer active". Revoke all: Confirm,
danger; deletes refresh_tokens and sessions by player_id in one transaction; a login racing it
either commits before and is revoked or after and survives, never half. No event.
(d) [independent] core-implementer; proof-auditor (conformance).

### Step 14: inventory Grant item and Revoke holding (INVENTORY-05)
(a) modules/inventory/src/lib.rs:54-104 (DDL), admin.rs:190,331-340, store.rs:135,248,297 plus
new delete_holding_exec and an item list read (id and kind; item_kinds at :289 returns kinds,
grant_exec takes an item id), conformance.rs, admin_tests.rs; conformance policy and golden;
mockup: the grant form (three fields, built like the config set form) replaces the bare toast
at UILayout/redesign/assets/registry.js:134.
(c) Grant: Form with character (the player's, from inventory.character_owners), item id and
quantity within MAX_HOLDING_QTY, plus a RequestId field; one transaction inserts the request
id (D14, conflict is NoOp), then lock_player_tx, is_player_purged_tx, item_exists_exec,
grant_exec. Revoke: Confirm, danger; deletes the whole holding row; missing row is NoOp.
Inventory emits no event today and none is added. Rollout: DROP SCHEMA inventory CASCADE.
(d) [independent] core-implementer; proof-auditor (conformance).

### Step 15: scheduler Fire now (64 part)
(a) modules/scheduler/src/lib.rs:418-528,660-680, admin.rs:374,405, conformance.rs,
admin_tests.rs; conformance policy and golden.
(c) D8. SchedulerAdmin gets the bus; a force flag on the fire path skips only
FIRE_RECHECK_SQL and checks the row exists; lock, dedicated connection and the
last_fired + FIRED emit transaction are the existing ones. fire_on and fire_locked return
Ok(()) for lock held, not due and fired alike (lib.rs:469-472, 500-502): they get an outcome
type (fired, not due, lock held, missing) that the tick ignores and the action maps to its
answer.
(d) [independent] core-implementer; proof-auditor (conformance).

### Step 16: mail Retry now, Park, Cancel parked (14, 15)
(a) modules/mail/src/admin.rs:158,331-341,593-601, store.rs (three ops beside 573 and 642),
service.rs:181-220, admin_tests.rs. Mail already has writes: true and its CapCase.
(c) Retry now: pending AND last_error IS NOT NULL to next_attempt_at = now(), generation + 1
(D6). Park: pending to parked. Cancel parked: Confirm, parked to cancelled, body kept. Each
answers NoOp on 0 rows. Pause sending stays disabled.
(d) [sonnet] core-implementer. Waits for the owner ruling on what a resume from parked does with last_error (Step 6 review).

### Step 17a: friend.removed reason operator-removed
(a) api/friends/events/src/lib.rs (reason constant, Removed doc, golden_samples),
docs/reference/public-api-baseline/friendsevents.txt, contract golden;
modules/audit/src/store.rs:143-169 and store_tests.rs.
(b) Before 17b: the contract and its one consumer land before anything emits the value.
(c) D7. Additive reason value; audit summarises it as "removed by operator" naming both
players. Nothing emits it yet. No DDL, no DROP.
(d) [sonnet] core-implementer; proof-auditor (contract golden, public-api).

### Step 17b: friends Remove friend (FRIENDS-08)
(a) modules/friends/src/service.rs:590, store.rs:213,367, admin.rs:153,249-263,
conformance.rs, tests; conformance policy and golden; mockup friends row menu.
(b) After 17a.
(c) D7. view_edge and delete_tx take an optional caller bound; one transaction deletes an
accepted edge and emits friend.removed with reason operator-removed and both players. A
pending row has no action; the invoke refuses a pending edge. Confirm, danger, prepared with
both handles.
(d) [independent] core-implementer; proof-auditor (conformance).

### Step 18: Copy payload (16)
(a) core/controlplane/src/lib.rs:762-780 (Then and Then::invokes),
public-api-baseline/controlplane.txt, core/admin/src/render/action.rs:58,
templates/primitives.html.tmpl:247,444-448, admin.js:347-356, events_provider.rs:332,774,
core/controlplane-rpc wire for Then, admincheck rules, goldens.
(c) D9. Then::Copy renders a button bound to the panel's code block; the [data-copy] handler
reads a target instead of assuming .secret-value; POISON_COPY becomes implemented with
Then::Copy and its forbidden arm goes. The secret-reveal modal keeps working (golden).
(d) [independent] core-implementer for the contract and provider, then [sonnet]
mockup-implementer for admin.js and the template, then [test-author] top tier for the golden
case that draws the new variant (Then is a variants! drawn enum), as parts a, b and c.

### Step 19a-19d: split proof of the new writers, one task per module
(a) 19a tools/splitproof/src/accounts_admin_checks.rs (after Step 13); 19b
inventory_admin_checks.rs (after Step 14); 19c scheduler_admin_checks.rs (after Step 15); 19d
friends_admin_checks.rs (after Step 17b). Each has its own commit and review.
(c) Drive the action through admin-svc to the module's process over cp.invoke and assert the
effect and the repeat's NoOp (Fire now: the second fire's new last_fired). Previously-wrong
branch: writes false / forbidden arm. At-risk topology: split.
(d) [test-author] top tier; proof-auditor.

## Phase D: session Client and IP (23)

### Step 20a: Origin type and the edge envelope
(a) core/opsapi/src/lib.rs:64 (the type, next to Identity); core/edge/src/wire.rs:10-21,
client.rs:113-175, server.rs:104,517-528 and the edge tests.
(b) First: the value and its wire form exist before any trait names them.
(c) D2. The envelope field is serde-default and skipped when none; the edge client sends none
and the edge server drops what it reads. Test: an envelope without the field decodes, one with
it round-trips. No trait changes, no behaviour change.
(d) [independent] core-implementer.

### Step 20b: Caller and LocalInvoker take Origin
(a) core/opsapi/src/lib.rs:106,510; core/remote/src/lib.rs:300,369,466,1116,1166,1270,1302;
core/edge client.rs and server.rs (pass the value through); tools/rpc-macro/src/lib.rs:589,849;
modules/gateway/src/backend.rs:63,101.
(b) After 20a. A trait signature changes in one commit, so the production impls go together.
(c) Every caller passes none; no behaviour change. contracts.txt rpc-arg and rpc-body lines
show a zero diff here and in Steps 21 and 23.
(d) [independent] core-implementer.

### Step 20c: test Caller impls
(a) The twelve test impls: core/remote tests.rs:1080,1538,2112,2242; push_backplane_tests.rs:25,
84; gateway tests.rs:1143,1241,1720; api/accounts/rpc tests.rs:20; api/apikeys/rpc tests.rs:18;
tools/rpc-macro-tests/tests/roundtrip.rs:38.
(b) Same commit as 20b (the workspace does not compile between them); a separate task so
neither subagent holds both file sets.
(c) One added argument per impl; one core/remote test asserts a non-empty Origin reaches the
far side of a Pool call.
(d) [sonnet] core-implementer.

### Step 21a: rpc-macro Origin parameter
(a) tools/rpc-macro/src/lib.rs:496-521,552-677,828-851; tools/rpc-macro-tests.
(b) After 20b/20c.
(c) A trait parameter of type Origin is excluded from the request struct, the HTTP decode and
the manifest args, fed by the server adapter and the local invoker, forwarded by the client.
Round-trip test: a client-supplied body field cannot set it.
(d) [independent] core-implementer.

### Step 21b: generators skip the Origin parameter
(a) tools/rpc-contract-model, csharp-client-gen, opscatalog-gen, routecheck.
(b) After 21a, before Step 23a puts the parameter on a real trait.
(c) Each tool skips the parameter the way it skips Identity; a test per tool feeds a trait
method with an Origin parameter and asserts the generated output is unchanged.
(d) [sonnet] core-implementer.

### Step 22a: gateway HTTP and push pass Origin
(a) modules/gateway/src/lib.rs:808,1297,1921-2009, push_ws.rs:107,1364-1417,1817, backend.rs,
gateway tests.
(c) HTTP: handle resolves the IP with httpmw::client_ip over the trusted set (hoisted from
PushLimits to a FrontDoor field, still injected by cmd/*) and the User-Agent capped at 256
bytes. Push: User-Agent captured at upgrade, passed with the slot IP.
(d) [independent] core-implementer.

### Step 22b: player QUIC passes Origin
(a) modules/gateway/src/lib.rs:828,856; core/edge/src/player.rs:108-131,283,537 and
player_tests.rs; cmd/server/src/lib.rs:45, cmd/gateway-svc/src/lib.rs:73.
(c) PlayerHandler gains the validated peer IP; client is "QUIC client" (D3).
(d) [independent] core-implementer.

### Step 23a: accounts takes and stores the origin
(a) api/accounts/api/src/lib.rs:183-209 (register, login, login_federated, create_guest);
docs/reference/public-api-baseline/accountsapi.txt; modules/accounts/src/lib.rs:185-193
(DDL), 279-346 (issue_session_tx), 586, 793-938, store.rs:335-341,774 (rotation), tests.
(b) After 21b and 22a/22b.
(c) D3. Invariant: every token row of a family carries the family's creation client and ip;
a refresh copies them. The Epic path passes none until 23b. contracts.txt rpc-arg and
rpc-body lines show a zero diff. Rollout: DROP SCHEMA accounts CASCADE.
(d) [independent] core-implementer; proof-auditor (public-api baseline).

### Step 23b: Epic callback resolves its origin
(a) modules/accounts/src/epic_oauth.rs:228,388; the accounts typed config and its two cmd
roots (cmd/server, cmd/accounts-svc); tools/processctl/src/fleet.rs:899-901.
(c) D3. The callback resolves IP with httpmw::client_ip over the configured trusted set and
the User-Agent capped at 256 bytes. Test: a forwarded address is used only from a trusted peer.
(d) [independent] core-implementer.

### Step 23c: Sessions panel shows Client and IP (23)
(a) modules/accounts/src/store.rs:1133 (live_families), admin.rs:168-188,693-694,
admin_tests.rs; mockup unchanged.
(c) fake_client and fake_ip go; unknown shows "—".
(d) [sonnet] core-implementer.

### Step 24: split proof of the origin
(a) tools/splitproof/src/accounts_admin_checks.rs.
(c) Log in through gateway-svc with a known User-Agent, then assert the admin Sessions panel
shows it and a non-empty IP. Previously-wrong branch: origin dropped at the remote backend.
(d) [test-author] top tier; proof-auditor.

## Phase E: close

### Step 25: verify and record
(a) docs/todo/admin-second-pass.md and -done.md, module todo files, UILayout/redesign/README.md.
(c) Run cargo run -p verifyctl -- --all (--fast skips the public-api and contract-golden
stages this plan changes), one rollout at a time. Expected red, and only this: admindiff's
lib tests (5 failing before the plan, plus mock_tests.rs:675-683 after Step 7b), per D13. Any
other red stage is a finding. Re-check every ADMIN2 item against the code; add one
item listing the admindiff accept lines and tests made stale by the mockup edits (D13) and one
item for the download carrier plan with its five buttons (D1).
(d) [docs] docs-writer for the prose; the verification runs are the lead's.

## Owner rulings (2026-11-24)

1. D1: buttons removed now; the download carrier is a separate plan, recorded in the TODO.
2. D9: Then::Copy is added.
3. D7: agreed as written.
4. D10 / D11: 60 removed; the eight 1c items stay and stay in the TODO.
5. D13: agreed; verify stays red on admindiff only.
6. D3: agreed.
7. D2: agreed; the transport work is cut into smaller steps (20a, 20b, 20c, 22a, 22b). If a
   step still proves too large when dispatched, it is cut further before it starts.
8. D14: agreed.
9. ADMIN2-19 stays in the plan.

Released so far: "zrob kroki 1-5" (Steps 1, 2, 3, 4, 5a, 5b); "kroki 6-10" (Steps 6, 7a, 7b, 8a,
8b, 9, 10).
Lane changes (owner, 2026-11-24, "ok to zmien"): Steps 11, 16, 17a and 21b moved from
[independent] to [sonnet]; every step keeps its opus core-reviewer.
