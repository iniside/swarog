# Admin second pass

Last updated: 2026-11-17

The admin redesign ships mockup elements the backend cannot back as deterministic fakes
(`// FAKE: <why>`, plan R1) and not-yet-possible actions as `implemented: false`, rendered
disabled (R2). This is the one list of them, so a second pass can replace each with a
real feature. Found with Grep over `*.rs` (`// FAKE:`, `implemented: false` and the
`.decl(.., false)` calls), which is a lower bound: a marker written another way is missed.
A port adds its own markers here in its part `a` commit (mockup-implementation skill).
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

### Fake data and actions of the mail port (Step 11, landing)

modules/mail/src/admin.rs is uncommitted at the time of writing; the line numbers are
its current ones.

- [ ] **Relay throttle signal** (Step 11, landing)
  ADMIN2-11. Mockup: the "▲ relay throttled" tile delta, the "Mail relay throttling"
  attention item and the SMTP 421 banner. Fakes: modules/mail/src/admin.rs:336, :366,
  :441: no throttle signal exists; a retrying row stands for one. Replaced by: the mail
  drain recording relay throttling (SMTP 421) as a state.
- [ ] **Queue-length history** (Step 11, landing)
  ADMIN2-12. Mockup: the Mail queue tile's sparkline. Fake: modules/mail/src/admin.rs:351:
  the outbox keeps no queue-length history. Replaced by: sampled queue length.
- [ ] **Pause sending** (Step 11, landing)
  ADMIN2-13. Mockup: the outbox "Pause sending" tool. Action `PAUSE_SENDING`,
  `implemented: false`: modules/mail/src/admin.rs:289-300, refused at :568; the drain has
  no pause switch. Replaced by: a persisted pause flag the drain honours.
- [ ] **Retry now** (Step 11, landing)
  ADMIN2-14. Mockup: the per-row "Retry". Action `RETRY_NOW`, `implemented: false`:
  modules/mail/src/admin.rs:302-303; the drain owns every retry time. Replaced by: a
  per-row "due now" operation on the outbox.
- [ ] **Park and cancel parked** (Step 11, landing)
  ADMIN2-15. Mockup: "Park: keep but stop retrying" and the parked row's "Cancel".
  Actions `PARK` and `CANCEL_PARKED`, `implemented: false`: modules/mail/src/admin.rs:304-306
  and :313; no operator park exists (resume parked is real). Replaced by: an operator
  park transition and a cancel for parked rows.

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

## Data a dev fleet never produces

`data difference` accept entries in `tools/admindiff/accept/*.txt` that are real data
the fleet lacks, not fakes, for the second pass to decide on (seed, or accept for good).
At the time of writing no entry in `index.txt` or `events.txt` has that kind: they are
`not ported yet`, `template choice`, `contract gap` or a declared deviation, and no mail
accept file exists yet. A port's `data difference` entries are listed here in its part
`b` or `a` commit (plan Step 24b is where seeding is decided).

_None._
