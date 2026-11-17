---
name: cheap-tier-ports-fix-workflow-not-model
description: Owner rule 2026-11-17 for admin redesign ports — never fall back to a stronger model when a cheap-tier (sonnet) port goes badly; post-mortem and fix the skill/tools/tests instead
metadata:
  node_type: memory
  type: feedback
  originSessionId: 6694aaef-849f-495c-bec1-e211bdc7a370
  modified: 2026-11-17T14:04:01.199Z
---

For the admin control-plane redesign ports (Steps 11–24), the view part (b) runs at the cheap tier (mockup-implementer, sonnet). When a port goes badly, do NOT propose falling back to opus. Run a post-mortem and fix the skill (`mockup-implementation`), the checklist, `tools/admindiff`, or the golden render tests, so the next port can be done cheaply.

**Why:** owner: "nie chcemy robić fallbacku na mocniejszy model, tylko chcemy tym planem doprowadzić skille i ogólnie workflow, żeby dało się to robić tanio i w miarę sprawnie". There are many panels left, so the goal of the plan is a workflow that works on the cheap tier.

**How to apply:**
- Contract gaps are closed in part `a` (opus core-implementer) before b starts.
- Post-mortems are scheduled after Step 11 (pilot), after Step 13 (accounts, the first port with new engine work), and after any port whose review needs more than one fix round.
- Recorded in the plan, Step 10b item 4.
- Test tier (owner 2026-11-17):
  - Gate tests stay on opus: golden, admindiff, admincheck/conformance findings, splitproof, fail-closed proofs.
  - Per-module provider `admin_tests.rs` and small fixture tests run on sonnet. They copy the mail pilot's file, which opus writes.
  - Plan: Step 10b item 4 **Test tier** and Step 25 (d).

Related: [[follow-uilayout-mockup-faithfully]].
