---
name: diff-review
description: The adversarial review method for a produced diff — scope check, hunt, finding format with origin, verdict and scope loop. Use when you are the core-reviewer or proof-auditor on a commit or task diff, and when the main session triages a returned punch list.
---

# Adversarial diff review

Who reviews what, and when, is `.agents/shared/planning-dispatch.md`
**Adversarial Diff Review**; this is the method. Hunt implementation gaps and
structural problems, and challenge the plan itself: a defect that follows from
the plan is a plan defect, never "matches the plan". The vibes skim ("roughly
matches the plan → pass") is banned. Route by files-touched through
`docs/reference/core-failure-taxonomy.md`, with a different method than the
implementer used.

## Method

1. Read the original request, the approved plan step, and the real diffs
   (`git diff` / `git show`), never the author's self-report.
2. Scope first: the owner's shape kept, every new mechanism necessary, no
   second source of truth or temporary bridge. Then line-by-line against the
   plan step: every (a)-listed symbol touched? anything touched the step did
   not authorize? any part silently skipped, stubbed, or "simplified"?
3. Hunt, do not confirm. Attack the fix's own new seams first (a loop that
   can partially fail, a constant that shadows a config knob, an error class
   folded into success, a resource owned by the wrong scope). Then: what
   input, state, ordering, or partial failure makes this wrong; does the
   negative-path test execute the previously-wrong branch; is the at-risk
   topology split, not just monolith.
4. Findings as `class` · origin (`local` or `structural`, below) · `file:line`
   · failing scenario · what it should be.
   "Looks fine" / "mostly matches" / "pass with reservations" / "minor nits"
   without a list are banned phrases. REJECT only for a concrete defect, a
   scope violation, or missing required validation, each with a source
   anchor and a failing scenario or affected requirement; style, comment and
   wording findings are not REJECT grounds. Unverified concerns go in a
   separate list and do not change the verdict. No demand for unrelated
   hardening (`.claude/rules/dev-tooling.md`). A defect in the approved design
   is reported as a structural plan finding for the owner; it is neither
   suppressed nor patched.
5. Binary verdict: PASS with the taxonomy class list attacked for the files
   touched, or a punch list. No "pass with reservations". Bounce findings;
   never silently absorb them. A punch list of only comment findings ends
   the round; the orchestrator deletes the flagged comments inline.

## Origin

Origin means what produced the finding, not only its symptom: a local slip,
or a structural cause. Structural causes include a second copy of a rule (a
test, gate or tool that restates production logic, a hand list beside the real
source), a mechanism where each fix adds another special case, a seam where
new code meets differently-shaped old code, the wrong layer, a flawed plan,
and a plan scope that leaves part of the subsystem behind. A finding in a
mechanism that an earlier round of the same task already fixed is structural
by default. A structural finding names the cause and the fix for the cause
(fix the plan, finish the subsystem, replace the mechanism, delete the copy,
derive from the real source), never one more patch beside it.

If a fix exists only because the task introduced an unrequested mechanism,
the verdict is `REJECT — scope loop`: remove it or return to the approved
boundary. A fix that needs a new abstraction, module, storage layer, or
cross-module contract beyond the request/plan is a blocking owner question,
not an automatic fix.

## After the verdict (main session)

Group the findings by origin before dispatching any fix. Local findings get
one fix commit carrying `Kind: review-fix`. Fix by severity, not list order:
correctness first, comment/doc-truth only if it fits; leftovers go under
**Leftovers for owner** in the report, and the owner-deferred ones are written
to `docs/todo` by the lead. When the allowed rounds are spent, stop, report
what stands open plus the recommended fix, and wait.
