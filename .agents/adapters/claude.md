# Claude Adapter

Runtime mapping for Claude Code sessions, read after `AGENTS.md`. Every
`Agent` call passes an explicit `model:`; do not invent a model slug this
harness does not expose.

## Runtime Artifacts

During plan mode, edit only `C:\Users\lukas\.claude\plans\<slug>.md`. After
user approval, copy the plan into
`docs/plans/YYYY-MM-DD-HHMM-<topic>-plan.md`.

Shared skills live in `.agents/skills/`; `.claude/skills/` holds Claude
pointer stubs.

## Dispatch Mapping

- `[inline]` — the current main model, in the active context.
- `[independent]` / `[opus]` / `[fable]` — `subagent_type: "core-implementer"`
  (visual/UI: `"mockup-implementer"`). `[opus]` → `model:"opus"`, `[fable]` →
  `model:"fable"`, `[independent]` → the session's top tier (`fable` when the
  session runs Fable, else `opus`).
- `[mechanical]` / `[sonnet]` — `model:"sonnet"`; a provider port's view part
  is `subagent_type: "mockup-implementer"`, `model:"sonnet"`.
- `[docs]` — `subagent_type: "docs-writer"`, `model:"sonnet"` default;
  `"opus"` / `"fable"` only when the doc must judge what a seam means.
- `[test-author]` — `subagent_type: "test-author"`, top tier by default
  (`"opus"` / `"fable"`); `model:"sonnet"` only when it copies a top-tier
  reference, per the shared `[test-author]` rule.
- `[review]` — `subagent_type: "core-reviewer"`, `model:` ≥ the author's tier.
- Proof audit — `subagent_type: "proof-auditor"`, `model:` ≥ the author's
  tier.
- architecture research (how an area is built, ownership, contracts, flows in
  both topologies, overlap with a proposed change) — `subagent_type:
  "researcher"`, `effort:"medium"`, one question per agent, several in
  parallel only for non-overlapping questions. `model:"sonnet"` by default
  (mapping one area: ownership, contracts, flows, existing overlap); the
  session's top tier (`"opus"` / `"fable"`) when the answer is a design
  judgment: placing a new module or seam, choosing between extending and
  adding across several modules, or settling evidence a Sonnet researcher
  left contradictory (paste its report). Listing-only questions go to
  Explore on `model:"haiku"`.
- other read-only research — Explore / general-purpose, `model:"sonnet"`;
  listing-only research may run on `model:"haiku"`.
- search lane (`code-navigation` skill, fallback step 3) — `subagent_type: "code-scout"`,
  `model:"haiku"` by default, `"sonnet"` for multi-hop questions that need
  interpretation (split-topology wiring, event flows across modules,
  macro-generated code paths); a diagnosis question before a fix outside a
  plan is `model:"sonnet"`, `effort:"medium"` (`implementation-dispatch`
  skill **Fixes outside a plan**).

Effort is the Agent call's `effort:` parameter
(`low`|`medium`|`high`|`xhigh`|`max`), valued per
the `implementation-dispatch` skill **Model and effort**.

## Commit Trailers

Use the executing model. A tag selects a tier; the subagent writes whatever
that tier currently is. Do not hardcode a version per lane.

```text
Co-Authored-By: Claude <Family> <Version> <noreply@anthropic.com>
```

Fill `<Family> <Version>` from the executing session's own model identity,
never from an example or another lane.

Subject template: `<type>(<scope>): <imperative> (<plan-slug> step <id>)`. A
commit fixing review findings adds `Kind: review-fix` above the
`Co-Authored-By` line, in one `-m` (see `docs/reference/commit-format.md`):

```text
fix(match): require a ReportId on match.report (trackable-commit-format step 1)

Kind: review-fix
Co-Authored-By: Claude <Family> <Version> <noreply@anthropic.com>
```

## Measuring agent cost

`python .claude/scripts/agent_token_report.py [--days N | --session ID] [--json]` — read-only token report over this repo's Claude transcripts (totals, per-agentType cost, calls per turn, context share, navigation discipline, baseline; `--session` adds a per-dispatch table for one session).
