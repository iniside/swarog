# Config

Last updated: 2026-11-27

- [x] **archcheck fails on config's DDL** (2026-11-27, verify-reds-fix step 1)
  Rule 22 flagged six `CREATE` phrases in `SCHEMA_DDL_TEMPLATE`; renamed to `SCHEMA_TEMPLATE_DDL`
  (used only inside the `SCHEMA_DDL` initialiser, SQL unchanged).
- [x] **clippy fails on a config test** (2026-11-27, verify-reds-fix step 1)
  `modules/config/src/tests.rs:423` now passes `&mut tx` (`clippy::explicit_auto_deref`).
- [x] **AdminSubmit for the config admin page** (2026-11-24, admin-control-plane-redesign step 23)
  CONFIG-04. Stale: the adminapi page it described was razed in plan step 1, and
  `modules/config/src/lib.rs:522-524` became the edge contribution. Step 23 serves the
  config page as a control-plane provider (b0e8e377); split writes go through
  `cp.invoke` (part 23a2); no splitproof covers them (23t dropped, see config.md).
