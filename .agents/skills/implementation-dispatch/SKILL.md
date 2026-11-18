---
name: implementation-dispatch
description: Choose the lane, model and effort for a plan step or fix and write the subagent prompt. Use when tagging plan steps, before any Agent call that implements, tests or documents, when a subagent returns a hand-off or part hand-off, and when a review punch list comes back while other steps are in flight.
---

# Implementation dispatch

Binding short rules (inline only below the threshold, explicit model, review
always a subagent) stay in `.agents/shared/planning-dispatch.md`; this is the
how-to.

## Lanes

Dispatch is decided per plan step, not per session. Tags describe capability
and cost; `.agents/adapters/claude.md` **Dispatch Mapping** maps them to agents
and models. Claude plans may write `[opus]` / `[fable]` for `[independent]` and
`[sonnet]` for `[mechanical]`; do not ban `[fable]`.

- `[inline]` — the main agent; the exception, not the default. Two cases only:
  the closed below-threshold list, and a compile/typo fix inside a file this
  turn's approved step already has the main agent editing. Never `[inline]`,
  at any line count: a new function / type / signature change, a control-flow
  or condition change, anything spanning more than one file, anything a plan
  step lists under (a). "It's only a few lines" is not an argument. When
  unsure which lane, it is not `[inline]`.
- `[independent]` — top-tier subagent in a separate context. New API design,
  bus/registry seams, lifecycle ordering, cross-module behaviour, security
  boundaries. For `core/*` internals or cross-seam work, the vehicle is
  `core-implementer`.
- `[mechanical]` — cheaper implementation lane: renames, scaffolding,
  N-similar edits, applying a fully specified step, compile fixes,
  JSON/config. Never spend a higher tier on a rename. Visual/UI design is
  never `[mechanical]`, with one exception: a provider port's view part
  (`b`) whose `mockup-implementation` checklist is complete and has no open
  contract gap runs `[mechanical]` on the `mockup-implementer` persona.
  Checklist extraction, contract decisions and review of that part stay at
  the top tier. A gap found during the view part goes back to the backend
  part. A port that goes badly is fixed in the skill, its tooling and its
  tests, never by moving the view part to a higher tier. Documentation prose
  is never `[mechanical]`; it goes through `[docs]`.
- `[test-author]` — verify gates (golden render coverage, `admindiff`,
  admincheck and conformance findings), splitproof assertions and new test
  harnesses (a new event-plane fixture), always a separate step after the
  implementation it covers. Other tests belong to the implementation step.
  Top tier by default; cheap tier when it copies a reference written at the
  top tier. A cheap-tier test that goes badly is fixed in the reference
  file, skill and tooling, never by moving it to a higher tier.
- `[docs]` — `docs-writer`, for documentation prose written as a planned docs
  step: root guidance, `docs/reference`, `docs/roadmap`, `docs/todo`, plan
  errata, and Rust doc-comments. A standalone comment edit stays `[inline]`.
  It runs last, against landed code. Cheap default; escalate only when the
  doc must judge what a seam *means*. A plan's docs step names the exact
  files — an unnamed file is never found. Comment-only review findings are
  not a `[docs]` dispatch: the orchestrator fixes them inline.
- `[review]` — reviewer lane; critique only, no rewrites.

The lane is chosen for the whole plan step or named fix; never divide a
feature to fit the inline threshold. Tags are approved with the plan. Ask only
for untagged/ad-hoc work; an authorized lane is not reopened.

## Model and effort

Every Agent call passes an explicit `model:` from the adapter; nothing
inherits. Effort is the Agent `effort:` parameter; prompt wording does not set
it. Subagents run at `medium`, `low` for the search lane, pinned in each
`.claude/agents/*.md` definition; a built-in agent type passes `effort`
explicitly (`.claude/hooks/explicit_model_guard.py` blocks the call
otherwise). The owner may set another effort for a specific step or request.

## Prompts and hand-offs

A subagent starts from a fresh context: give it the task facts and source
anchors, not the conversation. A prompt states the goal, authority, owned
files, verification and commit boundary, plus, for code-touching work, the
path `.agents/skills/code-navigation/SKILL.md` (subagents do not inherit it).
An implementation prompt carries the step text and the plan sections it
depends on (invariants, step preamble) pasted, or named by heading; never
"read the whole plan". A precedent commit is named with what to copy from it
(file and function), so the subagent reads `git show --stat` and those hunks,
not the whole commit.
A hand-off states the outcome, verified anchors, what was and was not
validated, open questions, and **Leftovers for owner**. A subagent's commit
trailer names the executing model (adapter **Commit Trailers**); after a
multi-subagent rollout, audit the trailers against each step's lane.

**Step parts.** An implementer that returns a part hand-off
(`.claude/agents/core-implementer.md` **Long steps**) is followed by a fresh
one given the step text and that hand-off. A part gets no review of its own;
the review runs on the step's cumulative diff once the step is done.

## Review in parallel

Review does not gate the next task unless the next step depends on it.
Serialize only if the two steps touch any of the same files, or the later
step builds on an API/behaviour the earlier one introduces (plan step (b)
names that dependency). Otherwise dispatch the reviewer and the next
implementation concurrently, in one message — the reviewer is read-only, so
the sole hazard is two writers in one file.

When a parallel review returns a punch list, triage it by origin
(`.agents/skills/diff-review/SKILL.md`): local findings get one fix commit
carrying `Kind: review-fix`; structural findings go to the owner. If a fix
touches a file the in-flight task is editing, wait for that task to land,
then fix — never edit a file a live subagent holds.
