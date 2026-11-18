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
- Review stays a subagent and stays adversarial: it hunts implementation gaps
  and structural problems, and it must challenge the plan itself — a defect
  that follows from the plan is reported as a plan defect, never passed as
  "matches the plan" (owner 2026-11-18). Every finding names its origin:
  local (this diff) or structural (wrong layer, flawed plan, half-ported
  subsystem, second source of truth).
- What goes: default-REJECT / "must find something" pressure; style, comment
  and wording findings as REJECT grounds; patching structural findings one by
  one. A structural finding stops the loop and goes to the owner with a
  root-cause proposal (fix the plan, finish the subsystem) instead of a
  review-fix round.
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

### Step 4 — path-scoped rules instead of per-action loading [docs]
Replace AGENTS.md's "read before the action" loading of area knowledge with
`.claude/rules/*.md` files carrying `paths:` frontmatter (loaded by the
harness only when matching files are touched; code.claude.com/docs memory
"Organize rules"), or skills for multi-step procedures. First candidates:
`.agents/shared/gamebackend.md` **Domain modules** (paths `modules/**`,
`api/**`), event-plane, push-hub, admin/mockup, dev tooling scope (`tools/**`,
`weles/**`). Keep `gamebackend.md` for the always-true architecture core.
Cut MANDATORY/IMPORTANT emphasis: rules with zero exceptions are enforced by
hooks already (git safety, commit format, explicit model, worktrees); the rest
is stated once in plain words.

### Step 5 — procedures to skills, path knowledge to rules, delete code-readable prose [docs]
Owner 2026-11-18. Binding rules that must be known before an action stay
always/action-loaded as short statements (git safety, commit after task,
wipe/no dual-write, comments, hard constraints, one rollout at a time, review
always a subagent, no inline, no worktrees); only how-to detail moves.
- 5a: `planning-dispatch.md` Implementation Mode → skill
  `implementation-dispatch`; Adversarial Diff Review method → skill
  `diff-review` (core-reviewer reads it); Plan Writing → the
  `research-plan-present` skill; `research-navigation.md` method and search
  budget → skill `code-navigation` (agents get a pointer); AGENTS.md table and
  adapter repointed; Agent Effort folded into implementation-dispatch.
- 5b: `core-rules.md` Plans And Status Docs format →
  `.claude/rules/status-docs.md` (paths docs/todo, docs/plans, docs/roadmap),
  keeping file locations and the leftover rule; Commit Message Format trimmed
  (hook enforces); Agent Memory Backup → skill `memory-backup`;
  `gamebackend.md` one-rollout detail → `safe-verification` skill (short rule
  stays), Adding-a-module recipe deduped into `add-game-module`, Layout and
  the Commands tail deleted (read from the tree and --help).
- 5c: prune `.claude/rules/domain-modules.md` of text restating each
  module's own lib.rs header / api traits; split
  `docs/reference/core-failure-taxonomy.md` into per-class files with the
  same path kept as the index.

### Review
One core-reviewer pass over the cumulative diff after step 4, checking for
contradictions and dangling references (prose steps get no per-commit review
under the new policy). Step 1 gets its own single review.

### Probation
`scout_nav_guard.py` stays; re-measure with
`.claude/scripts/agent_token_report.py` after two weeks.
