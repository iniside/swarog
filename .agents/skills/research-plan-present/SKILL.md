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
   `.agents/shared/research-navigation.md`.
2. Preserve the owner's outcome and exclusions. State each unresolved question,
   give it one owner, and reuse sufficient completed research. A research
   handoff includes verified source anchors, conclusions, and open questions.
   Ask the owner which research method to use only when the method changes the
   answer. Stop when evidence supports the plan; expand only for a concrete
   dependency, contradiction, or failing case, and disclose unresolved
   uncertainty. Map overlapping seams (module registry, service registry, event
   bus, contribution slots) before proposing a new or replacement system, with
   an explicit "why not extend / depend on X".
3. Before writing the plan, load `.agents/shared/planning-dispatch.md`. Close
   before approval only unknowns that can affect scope, architecture, compatibility, topology (monolith vs
   split), lifecycle, or feasibility. Write ordered steps covering files, public
   contracts, behavior, ownership, dependencies, and validation; leave private
   implementation details to the implementer. Tests go in the implementation
   step, except the `[test-author]` cases in `planning-dispatch.md`.
4. Present the plan and wait for explicit approval before editing. After approval, use the chosen lane for each whole step, pass subagent
   effort via the Agent `effort:` parameter, and follow the canonical
   commit/review contract. Do not reopen an approved lane unless scope or
   constraints changed; ask the owner first if it must.

This skill applies to the whole requested change. Never split a feature into
inline-sized patches. For a one-shot known-symbol lookup or a change on the
closed dispatch-threshold list in `.agents/shared/planning-dispatch.md`, work
inline without this workflow.
