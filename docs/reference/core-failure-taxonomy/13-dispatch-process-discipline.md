# 13. `dispatch-process-discipline` — *the audit trail is part of correctness*

Ported from the ArcGame workflow (2026-10-21), not mined from a swarog commit.

**What.** How work is dispatched and recorded: a wrong-model trailer or a lost
commit corrupts the audit trail, not just etiquette.

**Attack.** An Agent call without an explicit `model:`; a plan step whose lane
tag does not match the work (gate tests or splitproof assertions outside `[test-author]`, documentation
prose outside `[docs]` in a planned docs step, an `[inline]` change off the threshold list); a lane
silently flipped mid-rollout; a `Co-Authored-By` trailer naming the harness
default instead of the executing model; many tasks batched into one commit; a
plan-step commit without its `(<plan-slug> step <id>)` suffix; a review-fix
commit without `Kind: review-fix`; research done as
one grep pass and reported as the API.

**Authority.** `.agents/shared/planning-dispatch.md` → **Implementation Mode**
(never flip a lane, explicit model);
`.agents/skills/implementation-dispatch/SKILL.md` → **Lanes** (what each lane
covers);
`.agents/shared/core-rules.md` → **Commit After Every Task**, **Commit Message
Format**; `.agents/skills/code-navigation/SKILL.md` → **Research method**;
`docs/reference/commit-format.md`.
