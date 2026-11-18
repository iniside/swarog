---
name: research-plan-present
description: >-
  Enforce swarog's research, plan, present, then edit order for non-trivial
  changes, API mapping, new or replacement modules/events/services/admin
  sections, cross-seam refactors, and anything that touches core/, modules/,
  api/ or cmd/ beyond the dispatch-threshold list. Also the checklist a
  core-reviewer applies to a written plan. Skip known-symbol lookups and work
  inside the dispatch-threshold list in planning-dispatch.md.
---

# Research, plan, present, then edit

Run these in order for any plan (plan mode / "write me a plan" / a
`docs/plans/…-plan.md`) — no skipping for "it's small". Research follows
`.agents/skills/code-navigation/SKILL.md`; owner decisions stay closed
(`.agents/shared/research-navigation.md`).

0. Preserve the owner's shape. The Context opens with the requested outcome
   and explicit exclusions in the owner's words. Every unrequested module,
   topic, capability, cache, bridge, or contract names the requirement that
   cannot be met without it, or becomes one blocking question. Extract/move
   work relocates the existing code; it never builds a parallel twin.
1. Map the overlap, then pick the open questions. The three seams (module
   registry, service registry `registry::provide` / `registry::require`,
   event bus) plus the contribution slots mean a capability often already
   exists or has a near-twin. Any plan proposing a new module, service, event,
   or admin section (or a replacement) documents in its Context, per
   overlapping candidate: what it does, how it differs, and an explicit "why
   not extend / depend on X". Without that the plan is incomplete. Before
   approval, close only questions that change scope, architecture,
   compatibility, topology (monolith vs split), lifecycle, or feasibility;
   record the rest for the implementer.
2. Research three non-overlapping angles: API surface (public signatures,
   `lifecycle::Module` methods, contract traits, `bus::define` descriptors),
   API usages (who constructs and consumes, `registry::require` callers,
   `on_tx` subscribers, slot contributors), and patterns (how existing
   modules register, migrate, emit/subscribe, contribute admin). Synthesize
   in the main agent — never write off one subagent; a search-lane answer is
   a lead.
3. Write concrete specifics: exact files, exact signatures, exact API calls,
   sequencing. Banned phrases ("figure out as we go", "TBD", "investigate
   during implementation", "may need to", "something like") mean a research
   gap — go back to step 2. Leave private implementation details to the
   implementer.
4. Structure as an ordered `Step 1 → Step 2 → …` sequence, not a catalog. Each
   step states **(a)** what is touched (exact files/symbols), **(b)** why now /
   order — the dependency forcing it before the next, **(c)** how —
   non-mechanical moves spelled out (`requires()`, `registry::provide` /
   `registry::require` wiring, edge glue, registration in `cmd/server` and
   the service composition root), **(d)** dispatch tag
   (`.agents/skills/implementation-dispatch/SKILL.md`). Steps need not each
   compile, but every step must be written out.

   **Step size.** One step is one mechanism in one module or one consumer
   group, sized so one implementer finishes it in about 60 tool calls:
   roughly eight files, or one subsystem port. About 100 calls is where the
   implementer stops and hands off. A larger unit is split into ordered
   steps when the plan is written, each still a whole slice of the feature
   (never split to get under the inline threshold or to keep an old path
   alive).

   **Tests.** The implementer writes the tests in the same step as the code,
   to the bar "revert the fix and the test must fail". A separate, later
   `[test-author]` step is only for verify gates, splitproof assertions and
   new test harnesses; it names the landed commit/behaviour, the
   previously-wrong branch each test must exercise, and the at-risk topology
   (split, not just monolith). A plan that touches testable production code
   with no tests is incomplete unless the user explicitly waived tests.
5. Review: dispatch `subagent_type: "core-reviewer"` — that field is written
   first, before `description` and before the prompt. Never a generic
   implementer or explore/plan type, and no hand-authored "grumpy senior
   engineer" persona. A plan gets one review, before approval. It checks scope
   substitution first, then hunts logical holes, missing pieces (a module's
   `migrate`, a `requires()` that does not match the real synchronous
   dependency, an event mutated instead of evolved additively, an import of
   another module's implementation crate, a cross-module foreign key),
   ambiguity and unstated assumptions. It rejects only with evidence, lists
   unverified concerns separately, and returns a punch list, never a
   rewrite. Address it before showing the user, or note deferrals with
   rationale.
6. Present the plan and wait for explicit approval before editing. After
   approval, use the chosen lane for each whole step. Do not reopen an
   approved lane unless scope or constraints changed; ask the owner first if
   it must.

This applies to the whole requested change. Never split a feature into
inline-sized patches.
