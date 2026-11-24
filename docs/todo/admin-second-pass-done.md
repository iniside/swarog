# Admin second pass — done

Last updated: 2026-11-24

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
  (`attention_lore`): there is no Lore module. Replaced by: a Lore module, or dropping the
  elements by owner decision.

- [x] **Matches in progress tile** (2026-11-24, admin-second-pass step 1)
  ADMIN2-06. Mockup: "Matches in progress" KPI. Fake: core/admin/src/fakes.rs:141
  (`matches_in_progress`): `match.matches` holds finished matches only. Replaced by:
  in-flight match tracking in the match module.

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
