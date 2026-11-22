# Groups

Last updated: 2026-11-22

- [x] **admin_recent_groups counts per row before the LIMIT** (2026-11-22, admin-control-plane-redesign step 13f)
  GROUPS-09. Closed by deleting `admin_recent_groups` (and `admin_totals`, `ALL_ROWS`):
  the old admin page was its only caller, and checklist G8
  (`docs/plans/2026-11-22-step18-groups-checklist.md:287`) drops it.
