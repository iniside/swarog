---
name: proportional-verification-and-scope
description: "Verification and review must match the step's purpose; no full suites, extra review rounds or side work the step doesn't need"
metadata:
  node_type: memory
  type: feedback
  originSessionId: 8214c2ca-dbe5-496f-9b45-71068b5bf051
  modified: 2026-11-26T21:33:51.126Z
---

Size verification, reviews and side work to what the step is for. A step that only adapts callers so the workspace compiles is verified by a build, not the workspace suite plus verifyctl. A feature that is half-implemented across a plan gets crate-level tests per step; full suites and gates wait until the functionality exists. Reviewers report behaviour-breaking defects; test-quality and edge-hardening notes go to TODO, not into another fix round.

**Why:** owner, 2026-11-26, one-token-mechanism: five review-fix rounds on Step 2 drifting into test hardening and the admin panel; Step 3 ran a 40+ minute full suite and regenerated the C# client before JWT signing even existed. "te bledy nadgorliwosci to ci sie zdecydowanie za czesto zdarzaja."

**How to apply:** before every dispatch, state in one line what the step must prove and pick the smallest check that proves it; put "no workspace suite, no verifyctl" in implementer briefs unless the step is the plan's final verification. Flag anything outside the plan's core scope to the owner before doing it. Related: [[csharp-client-after-contract-is-final]], [[no-inline-adhoc-fixes]].
