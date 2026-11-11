---
name: always-fix-at-the-source
description: "Owner rule (2026-11-11) — when a defect has a test-side workaround and a source fix, propose ONLY the source fix; never offer the workaround as an option"
metadata:
  node_type: memory
  type: feedback
  originSessionId: fb0529fe-4b60-43b5-a11a-9d60ec9ddba8
  modified: 2026-11-11T18:52:11.978Z
---

"Zawsze naprawiamy u źródła" — Lukasz, 2026-11-11, after I listed a test-only OnceCell
migrate workaround next to the production fix for the asyncevents plane-DDL lock cycle
(steady-state `CREATE INDEX IF NOT EXISTS` / `CREATE OR REPLACE TRIGGER` taking table
locks on `asyncevents.events`).

**Why:** a workaround hides a defect the product also has (here: every boot against a busy
plane queued all writers up to 60s). Same principle as `.agents/shared/planning-dispatch.md`
**Fix the Authority, Not the Symptom**, stated by the owner as a standing preference.

**How to apply:** when diagnosis yields both a symptom-side and a source-side fix, recommend
the source fix and do not present the workaround as an equal option; mention it only if the
source fix is impossible. Also: leftovers get fixed, not filed to `docs/todo`, unless the
owner defers them ("nic, trzeba to poprawić", same day). Related: [[core-failure-taxonomy]],
[[no-inline-adhoc-fixes]].
