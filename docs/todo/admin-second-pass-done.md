# Admin second pass — done

Last updated: 2026-11-24

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
