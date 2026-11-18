# Agent workflow simplification

Status: approved by owner 2026-11-18 (from the workflow audit in that session)

## Context

Owner's decision (2026-11-18): adopt the audit's simplifications. Reviews stay
on subagents, but they get shorter and some changes get none. Measured basis
(14 days to 2026-11-18): 340 commits, 106 review-fix (31%), 122 prose-only
(36%); ~9 dispatches per code step (impl, test, docs, each with 2 review
rounds); the same rule copied in 4-8 files; core-reviewer ~13% of context
tokens; main session loading ~64 KB of rules for an implementation action.

Keep unchanged: git-safety, commit-format, explicit model, core-reviewer on
code in core/, modules/, split wiring and verify gates; authority-first
implementation; safe-verification and one-rollout; wipe / no dual-write /
topology-blind; effort pinned per agent (commit 1783a99d).

## Steps

### Step 1 — hooks [mechanical]
Remove the LLM prompt hook on Agent calls from `.claude/settings.json`;
delete `.claude/hooks/haiku_listing_reminder.py` and
`.claude/hooks/exit_plan_checklist.py` and their wiring; drop the core/
taxonomy trigger from `.claude/hooks/ref_reminder.py` (keep event-plane,
push-hub, mockup, safe-verification triggers); update the wiring pins in
`.claude/hooks/tests/test_hooks.py`. Independent of steps 2-4.

### Step 2 — review and test policy [docs]
Single owner: `.agents/shared/planning-dispatch.md`.
- No review for prose-only commits (`docs/`, `.agents/`, `.claude/agents/`,
  `docs/plans/`, memory). A plan gets one review before approval.
  Exception: `.rs` doc comments on `api/` and `core/` contracts are code.
- One review round by default. A second round only for `core/`, split/edge
  wiring, or verify gates (verifyctl, archcheck, conformance, splitproof,
  goldens).
- Tests written in the same step are reviewed together with the step.
- `[test-author]` only for verify gates, splitproof assertions and new test
  harnesses; other tests are written by the implementer in the same step,
  with the bar "revert the fix and the test must fail".
- Shorten `.claude/agents/core-reviewer.md` to its lane-specific method and
  point at planning-dispatch for the shared rules; align
  `core-implementer.md`, `test-author.md`, `proof-auditor.md`,
  `.claude/hooks/post_commit_review_reminder.py` wording if it states a rule
  that changed (hook code change goes to a core-implementer follow-up).
- `core-rules.md` **Agent Effort**: one sentence — medium by default, low for
  the search lane, pinned in each agent definition; built-in types pass it.

### Step 3 — one owner per rule [docs]
After step 2. Merge `docs/reference/implementation-mode.md` and
`docs/reference/subagent-dispatch.md` into `.agents/shared/planning-dispatch.md`;
merge `docs/reference/research-mode.md` into
`.agents/shared/research-navigation.md` (keep the Search budget); cut
`docs/reference/plan-writing-workflow.md` to what planning-dispatch does not
already say (or merge it); delete the duplicate Refactor Safety. Trim the
agent files to their lane-specific bar with one-line pointers. Effort stated
once. CLAUDE.md keeps only the `@AGENTS.md` import and a short preamble;
AGENTS.md's action table points at the merged files and no longer loads
`core-failure-taxonomy.md` for the main session. Remove the adapter's
"Haiku Lanes" prose and synonym tables; move the codex/cursor/grok adapters
out (delete; git history keeps them). Sweep every reference to moved or
deleted files and sections (docs, skills, hooks' reason strings, agent files).

### Step 4 — split the module catalogue [docs]
Move `.agents/shared/gamebackend.md` **Domain modules** into
`docs/reference/domain-modules.md`; leave a pointer; AGENTS.md loads it only
when a module is touched.

### Review
One core-reviewer pass over the cumulative diff after step 4, checking for
contradictions and dangling references (prose steps get no per-commit review
under the new policy). Step 1 gets its own single review.

### Probation
`scout_nav_guard.py` stays; re-measure with
`.claude/scripts/agent_token_report.py` after two weeks.
