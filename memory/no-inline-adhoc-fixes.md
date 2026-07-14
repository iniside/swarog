---
name: no-inline-adhoc-fixes
description: Ad-hoc bug fixes are UNTAGGED work — ask the user for the dispatch lane FIRST; inline only with explicit consent (repeat offense 2026-07-14)
metadata: 
  node_type: memory
  type: feedback
  originSessionId: 2d1f848f-2c99-4b3e-a3bb-5c792c0ff30b
---

User rebuke (2026-07-14, admin extension-points bugfix arc): **"KTO POZWOLIL CI
FIXOWAC INLINE ?!"** — I implemented a multi-file ad-hoc fix (api consts, 4 modules,
portal engine, CSS/templates) entirely inline without asking for a lane, and then
applied two MORE inline edits AFTER the core-reviewer pass (so they shipped
review-less). Same-day recidivism after an earlier mockup-fidelity violation.

**Why:** CLAUDE.md Implementation Mode is explicit: lanes are decided per plan step;
for untagged/ad-hoc work I must ASK, and complex work defaults to a subagent lane
([opus]/[fable] via core-implementer), never `[inline]`. Inline is reserved for
mid-edit judgment that can't be handed off. A user having chosen [inline] once (the
timing-tests arc) is NOT standing permission — it was scoped to that arc.

**How to apply:**
- Any unplanned fix/feature request → AskUserQuestion for the lane (+ effort) BEFORE
  editing, however "obvious" the fix looks. Urgency/user frustration is not an
  inline license.
- Post-review follow-up edits are ALSO work: they go back through the same lane and
  get reviewed; never "just one more inline tweak" after the review pass.
- See [[adversarial-subagent-review]] — the review-side twin of this rule (reviews
  are never done inline either).
