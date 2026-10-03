---
name: splitproof-is-proof-surface
description: "Authoring tools/splitproof assertions is core-implementer; auditing landed ones is proof-auditor (read-only, cannot write). Not test-author."
metadata: 
  node_type: memory
  type: feedback
  originSessionId: 3dcd09ee-8844-4095-a44d-a83c5ff1a9f8
  modified: 2026-10-03T19:37:49.958Z
---

`tools/splitproof` is the only instrument for properties no other gate checks — a live
two-topology fleet driven as a harness. Route by **activity**:

- **Authoring or editing an assertion → `core-implementer`.** These are assertions inside
  a *binary harness crate*, not cargo test cases. CLAUDE.md itself lists "extend
  `tools/splitproof` with a named assertion" as part of implementing a module.
- **Auditing landed assertions → `proof-auditor`.** Its tool set is `Read, Grep, Glob,
  Bash` — it **cannot edit files**, so it can never be the author. This is the fact that
  settles every argument about this path; check an agent's tools before routing to it.
- **Not `test-author`**, whose lane is cargo tests for landed code.

**Why this entry was rewritten (2026-09-12):** its earlier form said only "assertions →
proof-auditor, harness defects → core-implementer, NEVER test-author". I read "assertions"
literally, ignored that proof-auditor cannot write, and overrode a plan step that had
tagged the work correctly. Three dispatches were rejected in a row — core-implementer →
proof-auditor → test-author → core-implementer, a full cycle — because the repo guardrail
hook has no rule for this path and resolves it differently each time.

**How to apply:**
- Before routing, check the agent definition's **tool list**. A lane that cannot write is
  not an authoring lane, whatever a memory or a role blurb calls it.
- When a guardrail rejects two different lanes for one task, stop re-dispatching: that is
  a rule defect, not a routing question. Report the cycle and ask.
- A plan's own lane tag was written with the step in front of it — do not override it from
  a general memory without checking that the memory's lane can do the work.

**Observed 2026-10-03 (push invoke rollout):** new assertions dispatched to core-implementer
passed the hook (Step 5, [PH7–PH11]); a later *tightening of existing assertions* after a
proof audit was rejected for core-implementer and accepted for `test-author` (model opus),
which did it correctly. The hook keys on wording: "add/author assertions for a step" →
core-implementer passes; "tighten/fix assertions per an audit" reads as test work. Phrase
the prompt by the actual activity; still only one re-route before asking.

Related: [[specialized-core-agents]], [[no-inline-adhoc-fixes]],
[[cite-a-precedent-only-after-reading-it]].
