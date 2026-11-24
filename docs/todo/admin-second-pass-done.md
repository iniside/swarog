# Admin second pass — done

Last updated: 2026-11-24

- [x] **Deployment name and version** (2026-11-24, admin-second-pass step 11)
  ADMIN2-01. Mockup: top-bar deployment and the dashboard subtitle. Removed: `DEPLOYMENT`,
  `DASHBOARD_SUBTITLE` and the process count and topology from the subtitle (mockup
  subtitle now "emberline-prod · Swarog 0.1.0"). Replaced by `ADMIN_DEPLOYMENT_NAME` read
  once at init (unset or blank shows "—") and the workspace version.
- [x] **Config revision writers** (2026-11-24, admin-second-pass step 10)
  ADMIN2-65. Mockup: the "by <user>" actor of each Revisions entry and the Keys row's writer.
  Removed: `fake_by` and `FAKE_LOGINS`. Replaced by `config.setting_history.by`: the
  `config.writer` setting the module's writers set for their statement (`InvokeCall.operator`
  for the admin create and save, "system" for `Service::set` and `set_tx`), else the
  session's database role.
- [x] **Config prior revisions** (2026-11-24, admin-second-pass step 10)
  ADMIN2-66. Mockup: the Revisions entries before the current one. Removed: `fake_prior` and
  `Selected::at_days_back`. Replaced by `config.setting_history`, one row per insert, update
  and delete of a setting written by the `config.settings` AFTER trigger with the revision it
  carries; the Revisions panel draws the key's newest 10 rows.
- [x] **API key created by** (2026-11-24, admin-second-pass step 9)
  ADMIN2-58. Mockup: the Keys panel's "Created by" operator login. Removed: `fake_creator`
  and `FAKE_CREATORS`. Replaced by `apikeys.keys.created_by` (text NOT NULL, 128-byte cap),
  written from `InvokeCall.operator` by the create invoke and as "dev seed" by the dev seed.
- [x] **Cloud saves used sparkline** (2026-11-24, admin-second-pass step 8b)
  ADMIN2-52. Mockup: the tile's sparkline. Removed: `fake_spark`, the tile's series and the
  mockup spark sample (registry.js, pages-core.js catalog tile, KpiTile preview).
- [x] **Download all cloud saves** (2026-11-24, admin-second-pass step 8b)
  ADMIN2-53. Removed: `DOWNLOAD_ALL`, the mockup's Download all tool. No download carrier
  exists in the control plane (D1).
- [x] **Download one cloud save** (2026-11-24, admin-second-pass step 8b)
  ADMIN2-54. Removed: `DOWNLOAD`, the row action, `Row.item` and the mockup's row download
  button and empty action column.
- [x] **Download cloud saves from the player row menu** (2026-11-24, admin-second-pass step 8b)
  ADMIN2-55. Removed: `DOWNLOAD_PLAYER`, `order::player_row_action::STORAGE`, its drawn_orders.txt
  line and the mockup's registry.js row-menu item.
- [x] **Delete cloud saves** (2026-11-24, admin-second-pass step 8b)
  ADMIN2-56. Removed: `DELETE` and the mockup's Delete tool. The tab's read-only banner now
  reads "inspection" without "and download"; storage declares no action.
- [x] **Character level** (2026-11-24, admin-second-pass step 8a)
  ADMIN2-48. Mockup: the Characters tab's Level column. Removed: `fake_level`, the Level cell and
  column, the pinned-hash test, the splitproof header; the mockup already drew four columns.
- [x] **Character region** (2026-11-24, admin-second-pass step 8a)
  ADMIN2-49. Mockup: the Characters tab's Region column. Removed: `fake_region`, `REGIONS`, the
  Region cell and column.
- [x] **Character last played** (2026-11-24, admin-second-pass step 8a)
  ADMIN2-50. Mockup: the Characters tab's Last played column. Removed: `NO_LAST_PLAYED`, the "—"
  cell and column. The tab now draws Name, Class, Created (UTC day-time from SQL) and Character id.
- [x] **Export CSV** (2026-11-24, admin-second-pass step 7b)
  ADMIN2-25. Mockup: the Users & players "Export CSV" head action. Removed: `EXPORT_CSV`, its
  forbidden arm and the mockup button. No download carrier exists in the control plane (D1).

- [x] **Create account** (2026-11-24, admin-second-pass step 7b)
  ADMIN2-26. Mockup: the "Create account" head action. Removed: `CREATE_ACCOUNT`, its
  forbidden arm and the mockup button. An account is created only by the player's registration.

- [x] **Edit profile** (2026-11-24, admin-second-pass step 7b)
  ADMIN2-29. Mockup: the Profile panel's "Edit". Removed: `EDIT_PROFILE`, its forbidden arm
  and the mockup tool. The display name is immutable.

- [x] **Link and unlink identity** (2026-11-24, admin-second-pass step 7b)
  ADMIN2-30. Mockup: the Linked identities "Link" tool and each row's "Unlink". Removed:
  `LINK_IDENTITY`, `UNLINK_IDENTITY`, their forbidden arms, the identity row's action and item,
  and the mockup tool, button and empty action column.

- [x] **Players list column chooser** (2026-11-24, admin-second-pass step 7b)
  ADMIN2-33. Mockup: the "Columns: 9 of 12" toolbar control. Removed from the mockup; no
  column chooser exists and `DataTable` has no toolbar-right carrier.

- [x] **Personal data panel** (2026-11-24, admin-second-pass step 7b)
  ADMIN2-34. Mockup: the denied "Personal data" panel (`accounts.pii.read`). Removed from the
  mockup; no PII store exists and accounts contributes no such panel.

- [x] **Player country** (2026-11-24, admin-second-pass step 7a)
  ADMIN2-21. Mockup: the Profile panel's Country. Removed: `fake_country`, the profile row,
  its test and the mockup row and sample `country` values. No country is stored for an account.

- [x] **Active players delta and sparkline** (2026-11-24, admin-second-pass step 7a)
  ADMIN2-24. Mockup: the "Active players (24 h)" tile's "▲ 6.1% vs prev. day" and its
  sparkline. Removed: the tile has no delta and no series, and the mockup, catalog and
  design-system tiles no delta and no spark. No daily history of active players is kept.

- [x] **Define mail "retrying" so a healthy first send is not one** (2026-11-24, admin-second-pass step 6)
  ADMIN2-19. Plan D6: retrying is `state = 'pending' AND last_error IS NOT NULL` and pending is
  `state = 'pending' AND last_error IS NULL`, in the tab predicates, the stats count and
  `shown_state` together; a row claimed for its first send stays pending.

- [x] **Queue-length history** (2026-11-24, admin-second-pass step 6)
  ADMIN2-12. Mockup: the Mail queue tile's sparkline. Removed: the tile has no series and the
  mockup tile no spark. The outbox still keeps no queue-length history.

- [x] **Relay throttle signal** (2026-11-24, admin-second-pass step 6)
  ADMIN2-11. Mockup: the "▲ relay throttled" tile delta, the "Mail relay throttling"
  attention item and the SMTP 421 banner. Removed: the tile has no delta, the attention
  contribution, `order::dashboard_attention::MAIL` and its `drawn_orders.txt` line went, the
  outbox has no banner, and the mockup and design-system copies with them, including the
  mail-svc health note. The drain still records no throttle state.

- [x] **API key requests in the last 24 hours** (2026-11-24, admin-second-pass step 5b)
  ADMIN2-62. Mockup: the revoke confirm's "Requests (24 h)" impact row. Removed by owner
  ruling: `fake_requests` and `FAKE_REQUESTS_CAP` went, the confirm lists only the Key row,
  the mockup row with them. The gateway still counts no requests per key.

- [x] **Row-bound "Requests by this key" link** (2026-11-24, admin-second-pass step 5b)
  ADMIN2-60. Mockup: the apikey row menu's "Requests by this key". Removed by plan D10: the
  `key.requests` action, its `drawn_orders.txt` line and the mockup's audit menu group went;
  no store records a request per key.

- [x] **API key last used** (2026-11-24, admin-second-pass step 5b)
  ADMIN2-59. Mockup: the Keys panel's "Last used" column. Removed by owner ruling:
  `fake_last_used`, `FAKE_LAST_USED`, the table column and the mockup column and
  `lastUsed` samples went; the gateway records no key use.

- [x] **Audit log export** (2026-11-24, admin-second-pass step 5a)
  ADMIN2-69. Mockup: the Audit log head's "Export" button. Removed by plan D1 (no
  download carrier exists): the `log.export` action, its invoke arm and the mockup button.
  The download carrier is a separate plan.

- [x] **Audit log actor options with no source** (2026-11-24, admin-second-pass step 5a)
  ADMIN2-70. Mockup: the Actor select's `scheduler` and `apikey:ak_c4a8` options. Removed
  by owner ruling: `FAKE_ACTORS` and `actor_options` went, the select lists only the stored
  operator logins and `system`, and the mockup options went with them.

- [x] **Rewind checkpoint** (2026-11-24, admin-second-pass step 4)
  ADMIN2-17. Mockup: the subscription row's "Rewind checkpoint…". No checkpoint rewind
  exists in the event plane. Removed by owner ruling: the `sub.rewind` action, its invoke
  arm and its drawn_orders.txt line, and the mockup row menu entry. A rewind operation (a
  fenced, danger-level write) would be a new feature.

- [x] **Subscription consumer module and handler name** (2026-11-24, admin-second-pass step 4)
  ADMIN2-10. Mockup: the Consumer column and "module · handler" line. Closed by plan D4:
  the subscription id convention is `<consumer>.<topic-kebab>.v<N>` (core/bus/src/lib.rs,
  `SubscriptionSpec`), so the Consumer column is the id prefix, a derived fact, and lost
  its FAKE marker. No handler name exists anywhere, so the handler half left the portal and
  the mockup.

- [x] **Dead-lettered count and who skipped** (2026-11-24, admin-second-pass step 4)
  ADMIN2-09. Mockup: "Dead-lettered (7 d)" KPI, "all skipped by operator", and the skip
  texts naming a dead-letter list. Skipped events are not kept. Removed by owner ruling: the
  `kpi.dead-lettered` tile and `DEAD_LETTERED` in the portal; the KPI, the "Goes to
  dead-letter list" impact row and the dead-letter hint in the mockup. A stored record of
  skipped events with the operator would be a new feature.

- [x] **Event plane head sequence, append rate and checkpoints** (2026-11-24, admin-second-pass step 4)
  ADMIN2-08. Mockup: Head sequence KPI with its rate, and each subscription's Head and
  Checkpoint numbers. The event log has a (generation, xid, tie) position, not a sequence
  number, and no append rate is measured. Removed by owner ruling: the `kpi.head` tile,
  `HEAD_SEQUENCE`, `HEAD_RATE`, the fake `checkpoint` and the Head column, in the portal
  and the mockup. The Checkpoint column shows the stored cursor
  (`asyncevents::admin::SubInfo.cursor`, "generation/xid/tie"); Lag stays the real count.
  A display ordinal for the log position and a measured append rate would be new features.

- [x] **Scheduler run outcomes** (2026-11-24, admin-second-pass step 3)
  ADMIN2-07. Mockup: the dashboard's "storage.gc failed at 03:30" attention item, and the
  Scheduler page's Target, Last result and Enabled columns and each Recent fires entry's
  result, duration and note. The scheduler records no run (a fire carries only the schedule
  name and bumps `last_fired`). Removed by owner ruling: the `attention.runs` contribution
  and `dashboard_attention::SCHEDULER`, the `fake_*` helpers, and the Target, Last result,
  Enabled, duration and note cells, in the portal and the mockup. The table keeps Id, When,
  Next fire and Last (the last fire time); a fire entry is its schedule name and time.
  Recorded runs would be a new feature.

- [x] **Notification bell** (2026-11-24, admin-second-pass step 2)
  ADMIN2-04. Mockup: the top-bar bell's five entries. Fake: core/admin/src/fakes.rs:98
  (`notifications()`), used at core/admin/src/routes.rs:140: no operator feed exists.
  Removed by owner ruling: bell, pip and popover leave the shell view model, template, mockup
  top bar and catalog (NotificationItem, notification.source). An operator feed fed by
  health, the event plane and audit would be a new feature.

- [x] **Lore tiles and stale-locks attention** (2026-11-24, admin-second-pass step 1)
  ADMIN2-05. Mockup: "Lore seats" and "Lore store used" tiles, "2 stale Lore locks"
  attention. Fakes: core/admin/src/fakes.rs:113 (`lore_seats`), :127 (`lore_store`), :171
  (`attention_lore`): there is no Lore module. Dropped by owner decision on 2026-11-24;
  nothing replaces them.

- [x] **Matches in progress tile** (2026-11-24, admin-second-pass step 1)
  ADMIN2-06. Mockup: "Matches in progress" KPI. Fake: core/admin/src/fakes.rs:141
  (`matches_in_progress`): `match.matches` holds finished matches only. Dropped by owner decision on 2026-11-24;
  nothing replaces it.

- [x] **A filter on an undeclared key draws but does nothing** (2026-11-22, admin-control-plane-redesign step 16)
  ADMIN2-45. `panels::resolve` (core/admin/src/engine/panels.rs) refuses a `DataTable` or
  `WorkQueue` answer whose filter key `declared_params` lacks: the panel shows the error
  state naming the key instead of a filter the route would drop. It runs where the portal
  holds the answer and the manifest, so a co-hosted and a remote provider take the same
  path. Covered by `a_panel_filter_on_an_undeclared_key_is_an_error`.

- [x] **Wallet Grant/Revoke segmented switch** (2026-11-21, admin-control-plane-redesign step 13e4)
  ADMIN2-40. `FormSpec.modes` (`FormMode`: id, label, action, submit label) draws as the
  mockup's `div.seg` (pages-platform.js:172) on a panel form and a modal `Then::Form`;
  `admin.js` swaps the picked mode and the submit label, and the submit runs the action of
  the posted mode the stored row holds. No accept file maps the wallet page yet, so no
  accept line existed to delete; the wallet port (Step 15) declares Grant and Revoke as
  modes instead of a `FieldKind::Enum` field.

- [x] **Blank filter value reads as a filter on an empty outbox** (2026-11-19, admin-control-plane-redesign step 13c.2b2)
  `filter_views` maps an empty param to `"any"`, as the providers already read it, so a
  hand-typed `?template=` no longer reads as a filter. Covered by the route test
  `only_declared_page_params_reach_the_panel_query` rather than a golden case.

- [x] **Muted mono value (Event plane Head cell)** (2026-11-18, admin-control-plane-redesign step 13c.1a)
  ADMIN2-18. `CellKind::MonoMuted` draws `span.mono.muted`; the Event plane Head cell uses it
  and both `contract gap` lines left tools/admindiff/accept/events.txt.
