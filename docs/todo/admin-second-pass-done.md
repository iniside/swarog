# Admin second pass — done

Last updated: 2026-11-19

- [x] **Blank filter value reads as a filter on an empty outbox** (2026-11-19, admin-control-plane-redesign step 13c.2b2)
  `filter_views` maps an empty param to `"any"`, as the providers already read it, so a
  hand-typed `?template=` no longer reads as a filter. Covered by the route test
  `only_declared_page_params_reach_the_panel_query` rather than a golden case.

- [x] **Muted mono value (Event plane Head cell)** (2026-11-18, admin-control-plane-redesign step 13c.1a)
  ADMIN2-18. `CellKind::MonoMuted` draws `span.mono.muted`; the Event plane Head cell uses it
  and both `contract gap` lines left tools/admindiff/accept/events.txt.
