---
name: splitproof-is-proof-surface
description: "tools/splitproof work is proof-auditing, not test authoring — route it to proof-auditor, never test-author"
metadata:
  node_type: memory
  type: feedback
---

Twice in one session I dispatched `test-author` for `tools/splitproof` work and was
corrected both times ("zly agent"): once to fix a splitproof defect (should have been
`core-implementer`), once to write the `[DL*]` account-deletion assertions (should have
been `proof-auditor`).

**Why:** splitproof is not a test suite that accompanies code — it is the repo's only
instrument for a whole class of properties no gate checks. `topiccheck`'s unsubscribed
check is keyed on `(topic, version)`, so six of seven consumers can be missing with
both profiles green; splitproof is what would catch it, and its content is entirely
hand-written with nothing forcing an assertion to exist. Work there is therefore
answering "does this gate catch the failure it claims to", which is proof-auditing.

**How to apply:** before dispatching anything that touches `tools/splitproof`, ask what
the task actually is. Writing or strengthening an assertion, or judging whether the
existing ones are non-vacuous → `proof-auditor`. Fixing a defect in the harness itself
→ `core-implementer`. `test-author` owns module `src/*_tests.rs`, not the harness.
Related: [[specialized-core-agents]], [[a-green-signal-that-never-ran-the-thing]],
[[not-pinned-lists-are-unexecuted-prose]].
