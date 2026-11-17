---
name: admin-refactor-red-gates-expected
description: "While the admin control-plane redesign is in progress, splitproof admin assertions (404 on /admin/*) and clippy dead-code in admin-adjacent modules are expected red; no verifyctl until the admin panel lands"
metadata:
  node_type: memory
  type: project
  originSessionId: fb0529fe-4b60-43b5-a11a-9d60ec9ddba8
  modified: 2026-11-13T17:08:28.981Z
---

Owner 2026-11-13: the admin control-plane redesign (docs/plans/2026-10-22-1759-admin-control-plane-redesign-plan.md; b8435c9a razed the old /admin routes) is mid-refactor. In `verifyctl --fast` run verify-1794585899-57512-fdc055d3: 44 split-proof FAILs all = 404 on `/admin/...` (incl. grants routed through admin → NT3/NT4/PH*/PY7/WL*), clippy FAIL = dead code left by the raze (ADMIN_BUDGET, mail outbox_stats/OutboxListRow, …). Expected.

**Why:** owner said failing admin is expected during the refactor; next verifyctl waits until the admin panel is finished.

**How to apply:** don't treat those specific failures as regressions or file them; don't run verifyctl again (incl. `--slow` for the csharp-client stage) until the admin panel work is done — ask first. Any NON-admin red stage is still a real signal ([[ask-for-observed-gate-state]]). Note: csharp-client stage is only in `--slow`/`--all`, so GATEWAY-07's C# t1–t4 cases are still unconfirmed live.
