---
name: not-pinned-lists-are-unexecuted-prose
description: "A gate's 'deliberately NOT covered' list is the one part nothing executes — audit it hardest; three false reasons landed there in one commit"
metadata:
  node_type: memory
  type: feedback
  originSessionId: 31c06266-64af-4bcb-82be-f14d3b988287
---

Every gate carries a list of what it deliberately does NOT check. **That list is
prose, inside an artifact whose entire job is executing checks — so it is the one
part nothing executes, and it rots hardest.** It is also where the next reader
stops looking, because it reads as audited.

Evidence (2026-07-17, `weles-wire-contract` stage). A careful implementer wrote the
list, and two independent passes found **three false claims in it**:
1. "HTTP paths have no second copy to drift against" — `/resolve` was hand-copied in
   both `agentapi.rs`'s route table and `resolve.rs`'s URL `format!`. Trivially
   pinnable; the stated reason was simply false.
2. "A variant added to remote's enum alone is still a compile error here" — false;
   the exhaustive match was on the OTHER side's enum.
3. It deferred its biggest gap to a stage **that did not exist yet**, in the
   operator-facing FAIL text. That is the green-SKIP shape (`b78444f`) in prose:
   a reader is told "covered elsewhere" and nothing covers it.

The implementer's own summary is the lesson: *"I wrote that list carefully and still
put three false claims in it, because prose about what isn't checked is the one part
of a gate nothing executes."*

**Why:** a wrong reason is worse than no reason. It stops the next reader from
re-deriving the gap, so the gap survives every future review by being pre-explained.

**How to apply:** when reviewing (or writing) any gate, checker, or verify stage,
**attack the not-covered list first, not the checks**. For each entry ask: is the
stated reason TRUE, and is the thing it cites REAL (does that stage/test exist
today, or is it planned)? Prefer closing a gap over documenting it — most turn out
to be ~10 lines. Give the list a header saying a false reason there is a bug in the
gate. Related: [[prose-about-code-is-not-evidence]],
[[didnt-forget-scripts-must-self-check]], [[adversarial-subagent-review]].
