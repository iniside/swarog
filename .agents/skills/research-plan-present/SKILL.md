---
name: research-plan-present
description: >-
  Enforce swarog's research, plan, present, then edit order for non-trivial
  changes, API mapping, new or replacement modules/events/services/admin
  sections, cross-seam refactors, and anything that touches core/, modules/,
  api/ or cmd/ beyond the dispatch-threshold list. Skip known-symbol lookups
  and work inside the dispatch-threshold list in planning-dispatch.md.
---

# Research, plan, present, then edit

Use the repository's canonical policy rather than restating it in the prompt.

1. Read `AGENTS.md` and the active adapter. For research, load
   `.agents/shared/research-navigation.md` and `docs/reference/research-mode.md`.
   Load `docs/reference/subagent-dispatch.md` only when dispatching.
2. Preserve the owner's outcome and exclusions. State each unresolved question,
   give it one owner, and reuse sufficient completed research. A research
   handoff includes verified source anchors, conclusions, and open questions.
   Ask the owner which research method to use only when the method changes the
   answer. Stop when evidence supports the plan; expand only for a concrete
   dependency, contradiction, or failing case, and disclose unresolved
   uncertainty. Map overlapping seams (module registry, service registry, event
   bus, contribution slots) before proposing a new or replacement system, with
   an explicit "why not extend / depend on X".
3. Before writing the plan, load `.agents/shared/planning-dispatch.md` and
   `docs/reference/plan-writing-workflow.md`. Close before approval only unknowns
   that can affect scope, architecture, compatibility, topology (monolith vs
   split), lifecycle, or feasibility. Write ordered steps covering files, public
   contracts, behavior, ownership, dependencies, and validation; leave private
   implementation details to the implementer. Tests are their own later
   `[test-author]` step, never bundled into an implementation step.
4. Load `docs/reference/implementation-mode.md` only when choosing and adding
   implementation lanes. Present the plan and wait for explicit approval before
   editing. After approval, use the chosen lane for each whole step, pass subagent
   effort via the Agent `effort:` parameter, and follow the canonical
   commit/review contract. Do not reopen an approved lane unless scope or
   constraints changed; ask the owner first if it must.

This skill applies to the whole requested change. Never split a feature into
inline-sized patches. For a one-shot known-symbol lookup, trivial mechanical
edit, or few-line existing-function fix within the dispatch-threshold list in
`.agents/shared/planning-dispatch.md`, work inline without this workflow, a
subagent, or a review.
