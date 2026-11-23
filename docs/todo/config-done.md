# Config

Last updated: 2026-11-24

- [x] **AdminSubmit for the config admin page** (2026-11-24, admin-control-plane-redesign step 23)
  CONFIG-04. Stale: the adminapi page it described was razed in plan step 1, and
  `modules/config/src/lib.rs:522-524` became the edge contribution. Step 23 serves the
  config page as a control-plane provider (b0e8e377); split writes go through
  `cp.invoke` (part 23a2) and are proven by 23t CF2-CF4.
