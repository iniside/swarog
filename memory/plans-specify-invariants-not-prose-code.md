---
name: plans-specify-invariants-not-prose-code
description: "A plan step that re-states code line by line (SQL, signatures, line anchors) never converges in plan review; specify decisions + invariants + tests, let diff review catch wiring"
metadata:
  node_type: memory
  type: feedback
  originSessionId: 6694aaef-849f-495c-bec1-e211bdc7a370
  modified: 2026-11-10T19:04:28.799Z
---

Step 8a of the admin-control-plane-redesign plan (2026-11-10) grew to ~400 lines of prose code (SQL, struct fields, line anchors, check order). Six core-reviewer rounds each returned REJECT with 1–2 majors; each fix added prose that the next round found new gaps in (round 2's blocker came from round 1's fix). Owner asked "czy w ogóle istnieje możliwość, że to dostanie kiedykolwiek pass".

**Why:** prose cannot be compiled or tested; a fresh default-REJECT reviewer always finds another unwired carrier. Most findings (missing `_form` in modals/panels) a compiler or the first round-trip test would catch in seconds.

**How to apply:** a plan step states owner decisions, invariants, and the tests that pin them; leave field names/anchors/wiring to the implementer and the diff review. Respect the 2-round plan-review cap; past it, go to implementation and review the diff. Related: [[adversarial-subagent-review]], [[prose-about-code-is-not-evidence]].
