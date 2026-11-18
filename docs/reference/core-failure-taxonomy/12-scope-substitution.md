# 12. `scope-substitution` / `fix-forward-loop` — *coherent code, wrong product*

Ported from the ArcGame workflow (2026-10-21), not mined from a swarog commit.

**What.** The owner-selected shape is replaced by a broader architecture: an
unrequested module, topic, capability, cache, bridge, or contract; an
extract/move that became a parallel rewrite. A fix-forward loop then hardens
problems that exist only because of that addition instead of removing it.

**Attack.** Compare the original request and its exclusions, the approved plan,
the task commit, and the cumulative diff from the task-start commit. Look for a
second implementation beside the moved one, a new `api/<name>` surface or
durable topic nobody asked for, a temporary dual authority, or review fixes
whose only reason to exist is one of those additions. Feasibility, reuse,
robustness, and a detailed plan do not authorize the scope.

**Authority.** `.agents/skills/research-plan-present/SKILL.md` step 0 and
`.agents/shared/planning-dispatch.md` → **Adversarial Diff Review**; the owner's
original request.

**Verdict.** If the fix exists only because the task introduced an unrequested
mechanism, return `REJECT — scope loop` (authority:
`.agents/skills/diff-review/SKILL.md` → **Origin**): remove it or return to the
last approved boundary, without prescribing further hardening. If removal would
change the owner-selected shape, ask one blocking question.
