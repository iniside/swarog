---
name: splitproof-is-proof-surface
description: "Authoring tools/splitproof assertions = test-author at model opus (Lukasz's decision 2026-10-21); auditing them = proof-auditor (read-only). Not core-implementer."
metadata:
  node_type: memory
  type: feedback
  originSessionId: 9fff48ab-4fe5-4d42-9605-6da280a4b0af
  modified: 2026-10-21T20:35:20.503Z
---

`tools/splitproof` is the live two-topology harness, the only gate for properties nothing
else checks. Route by **activity**:

- **Authoring or tightening a split-proof assertion → `test-author`, `model:"opus"`.**
  Decided by Lukasz on 2026-10-21. The guardrail hook rejected a core-implementer dispatch
  for new [PH12]–[PH15] assertions, and when asked, Lukasz picked test-author (opus).
  Use opus because the harness and topology are novel. Sonnet is not enough here.
- **Harness client/library code** (`pushws.rs` API, fleet wiring, processctl) **→
  `core-implementer`.** That is production code of the harness, not an assertion.
- **Auditing landed assertions → `proof-auditor`.** It is read-only (`Read, Grep, Glob,
  Bash`), so it can never be the author.

**Why:** the hook resolves this path by wording, and the old memory said
"core-implementer, not test-author". That sent the user through two "zly prompt"
corrections in one session (2026-10-21), each a wasted dispatch. Before that, on
2026-09-12, the same path cycled three lanes.

**How to apply:**
- Tag new plan steps for split-proof assertions `[test-author]` (model: opus).
- An older plan tagged `[opus] core-implementer` for assertions follows this decision
  instead. Record it in that plan's errata.
- If the hook rejects a lane, re-phrase the same lane once. If a second lane is rejected
  too, stop and ask; that is a rule defect.
- Any test-honesty review (flaky timing, does the test fail on the regression) goes to
  `proof-auditor`, not `core-reviewer`. The hook enforces this as well.

Related: [[specialized-core-agents]], [[split-impl-and-tests]].
