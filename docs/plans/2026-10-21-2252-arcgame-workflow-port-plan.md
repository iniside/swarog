# Port the ArcGame agent workflow into swarog — plan

## Context

swarog's agent workflow (`.agents/*`, `docs/reference/` workflow docs, `.claude/agents`)
was last touched 2026-08-21; the sibling repo `G:\ArcGame` evolved the same house style
through 2026-10-21. Research (3 parallel Opus subagents, 2026-10-21: hooks/settings,
shared docs/adapters, agents/skills) compared both trees file by file. Two findings were
spot-checked against code:

- `CLAUDE.md:760-1091` still carries a full inline copy of the working agreements that
  contradicts swarog's own `.agents/shared` (e.g. `CLAUDE.md:799` "ask how should I
  research this" for any non-trivial research vs `.agents/shared/research-navigation.md:18`
  "only when the method changes the answer"; `CLAUDE.md:855` "grumpy senior-engineer
  reviewer" vs `planning-dispatch.md` `core-reviewer`).
- `.agents/adapters/grok.md:66-67` runs `proof-auditor` with `capability_mode: "read-only"`
  — no shell, so it cannot `git show` the commit it audits (the same file warns about this
  for the reviewer two bullets earlier).

swarog has no hooks at all (`.claude/settings.json` holds one prompt-type agent-type
guardrail); ArcGame enforces its git-safety, model-lane and review rules mechanically.

**Why not just copy ArcGame wholesale:** ArcGame is Unreal C++ and deliberately diverged on
several policies. Owner decisions (2026-10-21) on each divergence:

| # | ArcGame divergence | Decision |
|---|---|---|
| 1 | Tests only on owner request | **Keep swarog** — separate `[test-author]` step stays MANDATORY |
| 2 | Effort set via the Agent `effort:` parameter, default `medium`, not prompt prose | **Adopt** |
| 3 | Inline allowed for a few-line fix incl. a condition change | **Keep swarog** threshold |
| 4 | Owning Mistakes cut to two sentences, no resignation letter, no memory copies of rules (propose a hook instead) | **Adopt** |
| 5 | No fixed subagent count; don't ask the count band every time | **Adopt** |
| 6 | Comment-only review findings end the round; fixed inline, flagged comments deleted not reworded | **Adopt** |
| 7 | Fix the Authority dropped from shared rules | **Keep swarog** — section stays MANDATORY |
| 8 | Architecture lives only in a shared file; `CLAUDE.md` is anchors (found in plan review: `gamebackend.md` drifted from `CLAUDE.md:14-751`) | **Adopt** — `gamebackend.md` is the single source (Step 4b), `CLAUDE.md` anchors (Step 6) |
| 9 | Below-threshold `[inline]` diffs get no review round (found in Step 3 review: `planning-dispatch.md` vs `CLAUDE.md:1031`) | **Adopt** the `.agents` rule — Step 6 drops CLAUDE.md's "every `[inline]` fix before commit" |
| 10 | Tier conflicts between `CLAUDE.md` (MANDATORY) and `.agents` (RULE) for Plans & Status Docs, Research / Search Mode, Agent Memory Backup (found in Step 5 review) | **`.agents` RULE wins** — recorded in Step 6 |
| 11 | Commit Message Format: MANDATORY in old `CLAUDE.md`, RULE in `.agents` (found in Step 6 review) | **RULE** |
| 12 | "Ask how should I research this": always (old `CLAUDE.md`) vs only when the method changes the answer (`.agents`) | **Only when the method changes the answer** |
| 13 | Mid-rollout lane-tag flip: "don't silently flip — ask" lost its "ask" in Step 6 | **Restore: ask the owner first** |

Out of scope (Unreal-specific or not worth it now): all `arc-*`/`ue-*` skills, Unreal MCP
(`.mcp.json`, `.claude/scripts/`), `arcsem` semantic index, `clang-nav`→`rust-nav` port
(only if the built-in LSP tool proves unreliable), ArcGame's 160 one-off
`permissions.allow` entries (run `fewer-permission-prompts` instead), the
`docs/todo` pair layout (swarog keeps `docs/roadmap/feature-tracker.md`), a new
concurrent-rollout guard hook (not requested — candidate follow-up), Codex
`.codex/agents/*.toml`.

## Sequencing rationale

Content moves into `.agents/shared` + `docs/reference` FIRST (Steps 1–4), the router next
(Step 5), and only then is `CLAUDE.md` reduced to anchors (Step 6) — reducing it earlier
would drop rules that have no other home yet. Agents (Step 7) cite the shared sections, so
they follow. Skills relocation (Step 8) needs the adapters' skill line from Step 4. Hooks
(Step 9) quote the final rule wording in their deny messages, so they come after the prose;
their tests (Step 10) follow the landed hooks. Every step is one commit and gets one
`core-reviewer` pass on that commit before the next dependent step dispatches; independent
steps may proceed while a fix lands.

Effort for every subagent below: passed as the Agent `effort:` parameter, `"medium"` unless
the owner states otherwise (decision 2, which Step 3 writes down).

## Step 1 — core-rules: Owning Mistakes, Comments, Commit format `[docs]` model:"sonnet"

(a) `.agents/shared/core-rules.md` sections **Owning Mistakes**, **Comments**, **Commit
Message Format**; `docs/reference/commit-format.md`.
(b) First: these are leaf rules nothing else in this plan depends on, and Step 6 needs
them to exist in `.agents` before `CLAUDE.md` loses its copies.
(c)
- Owning Mistakes → ArcGame `G:\ArcGame\.agents\shared\core-rules.md:3-8` wording:
  name the mistake, state the correction, fix it, two sentences; a MANDATORY violation names
  the section and the mismatch. Delete the resignation letter and the "update memory for
  the violated rule" clause; add "do not create memory copies of rules; propose a hook
  when enforcement is needed" (decision 4).
- Comments: add from ArcGame `core-rules.md:115-127` — exclusivity claims ("the only
  place…") banned; a claim about library/runtime behaviour (tokio, sqlx, Postgres, quinn)
  cites a `file:line` read this session; a comment flagged in review is deleted, not
  reworded; comment-only findings end the review round and are fixed inline by the
  orchestrator, no `[docs]` dispatch (decision 6). Keep swarog's "inverse check".
- Commit format: add type `perf`; plan-step commits end the subject with
  `(<plan-slug> step <id>)` (range `steps 3-4`; review fixes reuse the step's suffix) per
  ArcGame `commit-format.md:13-18`, replacing swarog's `(Step N — …)`. Trailer = the
  identity of the model that actually executed, read from its own runtime identity, never
  copied from an example. Rewrite `docs/reference/commit-format.md` examples with swarog
  scopes.
(d) `[docs]` docs-writer, `model:"sonnet"`.

## Step 2 — research + dispatch rules `[docs]` model:"sonnet"

(a) `.agents/shared/research-navigation.md`, `docs/reference/research-mode.md`,
`docs/reference/subagent-dispatch.md`.
(b) Independent of Step 1; before Step 3 because planning-dispatch cites these.
(c)
- From ArcGame `research-navigation.md:3-16` and `docs/reference/agents/research-mode.md:15-72`:
  one owner per unresolved question; finished research is reused unless stale or
  contradicted; handoff = verified anchors + conclusions + open questions; **stopping
  rule** — stop when evidence supports the decision and state residual uncertainty instead
  of re-searching; do not pad a fan-out to a fixed count; escalating a fan-out to the top
  tier needs a stated reason. Delete swarog's "ask the subagent-count band every time"
  and the 2–4/4–8/8–12 bands as a mandatory ask (decision 5).
- Navigation additions: empty LSP results while rust-analyzer is still indexing are not
  absence; never hand symbol navigation to a grep-sweeping agent; never pre-authorize grep.
- `subagent-dispatch.md`: port ArcGame `docs/reference/agents/subagent-dispatch.md:15-44`
  invariants — pre-flight each Agent call for `model:`; trailer = executing model, audited
  after rollout; fresh bounded context (reviewers always fresh); result handoff shape;
  effort via the `effort:` parameter, default `"medium"`, prompt prose does not set
  effort (decision 2).
(d) `[docs]` docs-writer, `model:"sonnet"`.

## Step 3 — planning, implementation, review, taxonomy `[docs]` model:"opus"

(a) `.agents/shared/planning-dispatch.md`, `docs/reference/plan-writing-workflow.md`,
`docs/reference/implementation-mode.md`, `docs/reference/core-failure-taxonomy.md`.
(b) After Step 2 (cites its research/dispatch rules). Opus: it must judge where ArcGame
text conflicts with decisions 1/3/7 and keep swarog's side.
(c)
- Plan Writing: new **Step 0 — preserve the owner's shape** (ArcGame
  `plan-writing-workflow.md:9-24`): restate outcome + exclusions in the owner's words; any
  unrequested subsystem/cache/bridge/contract names the requirement forcing it or becomes
  one blocking question; extract/move work relocates code, never builds a parallel twin.
  Placed before swarog's overlap-mapping ("Research before planning"). Before approval close
  only questions that change scope/architecture/feasibility. Plan reviewer: scope
  substitution checked first, reject only with evidence, unverified concerns listed
  separately. Step 1 "ask how many research subagents" removed (decision 5); step 5 "ask
  the think-effort level" replaced by the `effort:` parameter (decision 2). Keep the
  banned-phrase list, the mandatory separate `[test-author]` step (decision 1), and the
  ordered-step structure.
- Implementation Mode: move the `[docs]`/`docs-writer` lane in (it exists only in
  `CLAUDE.md` today), with the decision-6 carve-out (comment-only review findings fixed
  inline). Lane is chosen for the whole step; an authorized lane is not reopened. Keep
  swarog's inline threshold (decision 3) and Fix the Authority (decision 7) verbatim.
- Adversarial Diff Review (ArcGame `planning-dispatch.md:23-74`): review runs on the task
  commit plus the cumulative diff since the task-start commit; scope check first; REJECT
  only for a concrete defect, scope violation, or missing required validation, each with a
  source anchor and failing scenario; unverified concerns in a separate list that does not
  change the verdict; no demand for unrelated hardening and no reopening the approved
  design (backs "Dev tooling scope"); new verdict `REJECT — scope loop`; a new abstraction
  needed by a fix becomes an owner question; round-one fixes are committed before round
  two; independent steps proceed during fixes. Keep "must earn PASS with a class list",
  fresh spawn for round 2, cap 2 rounds, proof-auditor trigger unchanged. Review applies
  to prose-only commits too (this plan's Steps 1–8 are prose).
- **Excluded ArcGame line** (conflicts with decision 1): `planning-dispatch.md:10`
  "Tests appear only on explicit owner request" — not ported. ArcGame
  `planning-dispatch.md:26` "Below-threshold diffs get no review" already matches swarog's
  own `planning-dispatch.md` text and is kept per decision 9; Step 6 drops
  `CLAUDE.md:1031`'s "every `[inline]` fix before commit".
- Taxonomy: add class **Scope substitution** (ArcGame `ue-failure-taxonomy.md:184-208`,
  rewritten for swarog: unrequested module/topic/capability, parallel rewrite instead of a
  move) and **Dispatch & process discipline** (missing `model:`, wrong trailer, lane
  drift), with their file-routing rows.
(d) `[docs]` docs-writer, `model:"opus"`.

## Step 4 — adapters `[docs]` model:"sonnet"

(a) `.agents/adapters/claude.md`, `grok.md`, `cursor.md`, `codex.md`.
(b) After Step 3 (adapters translate the shared lanes/effort rule into runtime mechanics).
(c)
- `grok.md:66-67`: proof-auditor `capability_mode: "execute"`; port ArcGame `grok.md:64-80`
  "Reviewers Need Shell And Git" with `hostile-reviewer`→`core-reviewer`.
- `claude.md`: effort via the Agent `effort:` parameter (default `"medium"`); replace the
  hardcoded trailers at `claude.md:65-69` with ArcGame `adapters/claude.md:71-81`
  self-identification wording; add Haiku checklist lanes adapted from ArcGame — verifyctl/
  cargo log triage, pre-review diff lint (comment rules, no dual-write, no topology branch),
  trailer audit.
- `cursor.md`: note Cursor auto-loads `CLAUDE.md`.
- All four: one line "shared skills live in `.agents/skills/`" (consumed by Step 8).
(d) `[docs]` docs-writer, `model:"sonnet"`.

## Step 4b — single architecture source: gamebackend.md `[docs]` model:"sonnet"

(a) `.agents/shared/gamebackend.md` ← `CLAUDE.md:14-751` (The point of this codebase …
Layout).
(b) Before Step 5: the router sends every `core/`/`modules/`/`api/`/`cmd/` touch to
`gamebackend.md`, which has drifted (last change 2026-09-17: "17 fortresses", zero
mentions of `party`; CLAUDE.md 2026-10-05: 18 fortresses, party). Owner decision
2026-10-21 (decision 8): `gamebackend.md` is the single architecture source, `CLAUDE.md`
becomes anchors only (Step 6).
(c) Replace the body of `gamebackend.md` (from line 11 on, below its own heading) with
`CLAUDE.md:14-751` verbatim. Keep-list is **empty**: a word diff (plan review round 2,
2026-10-21) found every `gamebackend.md`-only token to be an older or re-wrapped form
("17 fortresses", `[DL0-DL13]`, missing `:8098/:9017`). Intentional hunks only: (1) the
root-relative links `[platform notes](docs/reference/platform-notes.md)` at
`CLAUDE.md:546,599,603,647,704` become `../../docs/reference/platform-notes.md` (and any
other root-relative link in the block likewise); (2) the tier markers
(`Dev tooling scope`, `One test rollout at a time` stay MANDATORY). Verify with
`git diff --no-index --word-diff <(sed -n 14,751p CLAUDE.md) <(sed -n '11,$p' .agents/shared/gamebackend.md)`
(via temp files on Windows) showing only those hunks.
(d) `[docs]` docs-writer, `model:"sonnet"`.

## Step 5 — action router `[docs]` model:"sonnet"

(a) `AGENTS.md`, `.agents/README.md`.
(b) After Steps 1–4: the router names the sections they produce.
(c) Replace "Required Read Order — read every file in `.agents/shared/`" with ArcGame's
router (`G:\ArcGame\AGENTS.md:3-9` + action table): read `AGENTS.md` + adapter, then the
authorities for the next action; unclear action stays read-only under the research row;
reuse authorities already read; rules bind even if not loaded. swarog rows: edit/commit →
`core-rules.md`; research/navigate → `research-navigation.md`, `research-mode.md`;
write/review a plan → + `planning-dispatch.md`, `plan-writing-workflow.md`;
implement/review → `planning-dispatch.md`, `implementation-mode.md`, `subagent-dispatch.md`,
`core-failure-taxonomy.md`; touch `core/`/`modules/`/`api/`/`cmd/` → `gamebackend.md`
(+ `add-game-module` skill for a new module); run cargo test/verifyctl/devctl →
`safe-verification` skill, `gamebackend.md` **One test rollout at a time**,
`docs/reference/testing.md`; admin UI → `mockup-implementation` skill. Keep the MANDATORY
index (renumber: Owning Mistakes stays RULE-tier) and Non-Negotiables. Rewrite
`.agents/README.md` as an instruction map (ArcGame `.agents/README.md:3-16`).
(d) `[docs]` docs-writer, `model:"sonnet"`.

## Step 6 — CLAUDE.md → anchor index `[docs]` model:"opus"

(a) All of `CLAUDE.md` after the title: the project block `CLAUDE.md:14-751` and the
working-agreements block `CLAUDE.md:752-1091`.
(b) Only after Steps 1–5 (incl. 4b) gave every rule and every architecture section a home
in `.agents`.
(c0) Project block: one anchor per `##` section of `gamebackend.md`
(`## The point of this codebase`, `## Hard constraints`, … `## Layout`), each one line
naming `.agents/shared/gamebackend.md` and the section. The two MANDATORY project sections
keep their tier in the anchor heading. ONE preamble for the whole file — the ArcGame one
(`G:\ArcGame\CLAUDE.md:1-12`: read `AGENTS.md` + `.agents/adapters/claude.md`, then every
authority of the matching action row) — plus an `@AGENTS.md` import line so the router
(with its One-test-rollout / Commands row) is auto-loaded into every Claude session.
(c) Verified rule→home map (2026-10-21): Owning Mistakes → `core-rules.md:3`; Plans &
Status Docs → `core-rules.md:24`; Git Safety → `core-rules.md:40`; Commit After Every
Task → `core-rules.md:51`; Agent Memory Backup → `core-rules.md:116`; Research Before
Planning → `research-navigation.md:3`; Research / Search Mode → `research-navigation.md:16`;
Research Never Overrules → `research-navigation.md:53`;
Plan Writing / Implementation Mode / Adversarial Review / Fix the Authority →
`planning-dispatch.md`; Comments / Commit format → `core-rules.md`. Nothing is moved a
second time; the implementer re-greps each heading after Steps 1–5 and only adds text for
a rule whose CLAUDE.md copy carries content its home lacks (a diff, not a guess). Then
replace the block with ArcGame-style anchors (the single c0 preamble + one
`## <Section> — <TIER>` heading per rule, one line naming the owning file and section).
**The `.agents` tier wins** where they differ (decision 10): Owning Mistakes, Plans &
Status Docs, Research / Search Mode and Agent Memory Backup are RULE (MANDATORY in the old
CLAUDE.md copy); the commit message names these four demotions. The commit message lists the rule→home map.
(d) `[docs]` docs-writer, `model:"opus"`.

## Step 7 — agent definitions `[docs]` model:"opus"

(a) `.claude/agents/core-reviewer.md`, `proof-auditor.md`, `core-implementer.md`, plus
citation repointing in `docs-writer.md`, `test-author.md`.
(b) After Steps 3 and 6 (they cite the review rules Step 3 writes and must stop citing the
`CLAUDE.md` copies Step 6 turns into anchors).
(c)
- Every `CLAUDE.md → <section>` citation in `.claude/agents/*.md` points at the owning file
  instead (known: `docs-writer.md:35,43`, `test-author.md:25` "Hard constraint #10" →
  `gamebackend.md` **Hard constraints**, `test-author.md:27`, `core-implementer.md:14`;
  re-grep `CLAUDE.md` in `.claude/agents` for the rest). `docs-writer.md:40` "Historical
  docs are archives" cites a section that exists nowhere — delete the citation.
- core-reviewer ← ArcGame `hostile-reviewer.md:34-45`: REJECT evidence bar, separate
  unverified-concerns list, no unrelated hardening / no reopening approved design, review
  the committed task commit + cumulative diff, independent work proceeds; add the
  `REJECT — scope loop` verdict and the scope-first check. Keep banned phrases, no
  subagents/worktree/resume, fresh round 2, architecture-review composition, old-rail
  class.
- proof-auditor ← ArcGame `proof-auditor.md:33-34,41-46,51-53`: verdicts
  `green-on-real-failure` and `covers-expected-behavior`; check that a claimed known gap
  was verified against code. Skip the "tests only on owner request" lines (decision 1).
  Keep unenumerated-drift and the NotApplicable stance check.
- core-implementer ← ArcGame `core-implementer.md:20-29,39-41`: inline 3-line Rust nav
  chain (LSP/rust-analyzer → targeted read / search subagent → Grep tool, a lower bound);
  name the taxonomy classes it must survive; executing-identity trailer. Keep hand-off (c)
  as the failing-branch test.
(d) `[docs]` docs-writer, `model:"opus"`.

## Step 8 — skills layout + research-plan-present `[sonnet]` then `[docs]` model:"sonnet"

(a) Move `.claude/skills/{add-game-module,architecture-review,mockup-implementation,safe-verification,split-topology-debugger}/SKILL.md`
→ `.agents/skills/<name>/SKILL.md` (`git mv`); recreate each `.claude/skills/<name>/SKILL.md`
as a pointer keeping the original frontmatter (`name`, `description`) and a body
"Read and follow the canonical skill at `../../../.agents/skills/<name>/SKILL.md`. Do not
maintain a second copy." (ArcGame pattern). Then new
`.agents/skills/research-plan-present/SKILL.md` + pointer, adapted from
`G:\ArcGame\.agents\skills\research-plan-present\SKILL.md` with swarog paths and dropping
its tests-on-request line (`:31`). In the moved skills, repoint every `CLAUDE.md →`
citation to its owning file (known: `add-game-module` :18, `safe-verification` :11;
re-grep the rest) and fix relative links for the new depth.
(b) After Steps 4 and 6 (adapters announce `.agents/skills/`; anchors exist).
(c) Two commits: the mechanical move, then the new skill.
(d) Move: `[docs]` docs-writer `model:"sonnet"` (write-capable type; the agent-type
guardrail in `.claude/settings.json` denies writes from `general-purpose`). New skill:
`[docs]` docs-writer `model:"sonnet"`.

## Step 9 — hooks `[opus]` model:"opus"

(a) New `.claude/hooks/{git_safety_hook,post_commit_review_reminder,haiku_listing_reminder,grep_sweep_guard,ref_reminder,no_worktree_hook}.py`;
`.claude/settings.json`.
(b) After the prose (deny messages cite final section names).
(c) Port from `G:\ArcGame\.claude\hooks\`:
- **All Bash-only scripts** (`git_safety_hook`, `ref_reminder`, `grep_sweep_guard`,
  `post_commit_review_reminder`) get their internal `tool_name == "Bash"` checks widened to
  `{"Bash", "PowerShell"}` — widening only the matcher would leave PowerShell a silent
  no-op (ArcGame `ref_reminder.py:26-41`).
- `git_safety_hook` (Pre `Bash|PowerShell`): as ArcGame, plus deny `git push --force`/`-f`
  and branch creation (`git checkout -b`, `git switch -c`, `git branch <name>` with no
  listing/delete flag) — listing (`git branch`, `-a`, `--show-current`), `-d`, and
  switching to an existing branch stay allowed. These two return
  `permissionDecision: "ask"` (not deny, no override token): CLAUDE.md allows both on
  explicit owner instruction, and the owner's permission prompt is that consent — a token
  the agent writes itself would be self-granted. Trailer check
  accepts any `Co-Authored-By: Claude <Family> <Version> <noreply@anthropic.com>`
  (including PowerShell `@'…'@` here-strings); the deny message shows that `<Family>
  <Version>` form, never a concrete model (ArcGame hardcodes Opus 4.8). Worktree reason:
  second `target/` + second build against the one shared Postgres.
- `no_worktree_hook` (Pre `Agent|EnterWorktree`): same reason text.
- explicit-model guard (inline in settings, Pre `Agent`): deny Agent calls without
  `model`, message naming swarog lanes.
- `haiku_listing_reminder` (Pre `Agent`): as-is, reason cites `research-mode.md`.
- ExitPlanMode checklist (Pre `ExitPlanMode`, `command` hook emitting
  `hookSpecificOutput.additionalContext` only — a reminder, never a deny, as in ArcGame):
  plan reviewer `core-reviewer` ran; lane tags + `model:` + `effort:` per step listed;
  separate `[test-author]` step present.
- `grep_sweep_guard` (Pre `Bash|PowerShell`): artifact dirs `target`, `run`, `deploy`,
  `__pycache__`; also `Select-String -Recurse`/`Get-ChildItem -Recurse`; message: LSP →
  Grep tool, grep is a lower bound; `force-grep` override kept.
- `post_commit_review_reminder` (Post `Bash|PowerShell`): `core-reviewer` +
  `docs/reference/core-failure-taxonomy.md`; mention proof-auditor only when the commit
  touches `tools/{verifyctl,archcheck,conformance,topiccheck}` or golden files.
- `ref_reminder` (Pre `Edit|Write` + `Bash|PowerShell`), once per session: `core/asyncevents`
  → `docs/reference/event-plane-ops.md`; `core/edge`, `modules/gateway` →
  `docs/reference/push-hub.md` and the edge references present in `docs/reference/`;
  `core/` → `core-failure-taxonomy.md`; `UILayout/`, `modules/admin/templates` →
  `mockup-implementation` skill; `cargo test`/`verifyctl`/`devctl up` →
  `safe-verification` skill + `platform-notes.md`. Every target path verified to exist.
- settings: commands `python "$CLAUDE_PROJECT_DIR/.claude/hooks/<x>.py"` (never
  `python3` — on Windows that is the WindowsApps stub); matchers use `Agent` (current tool
  name; no `Task`); `env.CLAUDE_CODE_MAX_SUBAGENTS_PER_SESSION: "300"`. If
  `$CLAUDE_PROJECT_DIR` does not expand in the Windows hook shell (Step 10 smoke proves
  it), fall back to the repo-relative `.claude/hooks/<x>.py` path.
- `post_commit_review_reminder` stays silent on prose-only commits (ArcGame behaviour). That
  is only a reminder: this plan's prose Steps 1–8 are still reviewed, dispatched by the
  orchestrator per the Sequencing rationale. The implementer confirms the existing
  agent-type guardrail prompt allows `core-reviewer` on a docs-only commit and widens its
  wording if not.
No cargo involved — no rollout lease needed.
(d) `[opus]` core-implementer, `model:"opus"` (write-capable type; `general-purpose` is
read-only under the guardrail).

## Step 10 — hook tests `[test-author]` model:"sonnet"

(a) New `.claude/hooks/tests/test_hooks.py` (stdlib `unittest`), run with
`python -m unittest discover .claude/hooks/tests`.
(b) After Step 9 lands and runs.
(c) Each hook driven as a subprocess with crafted PreToolUse/PostToolUse JSON on stdin,
asserting the decision JSON: git_safety denies `git stash`, `git restore x`,
`git reset --hard`, a commit without trailer, and
the PowerShell tool name; ALLOWS `git stash list`, a quoted `"git stash"` inside a commit
message, a trailered commit. grep_sweep denies `grep -r foo target/`, allows piped grep and
`force-grep`. haiku_listing denies a listing-only prompt on `opus`, allows `force-model`
and a reasoning prompt. post_commit emits the reminder after a real commit, stays silent
on `--amend`/`--dry-run`/prose-only commit. Additional allow cases (false positives that
would block normal work): `git branch`, `git branch -a`, `git branch --show-current`,
`git branch -d x`, `git branch --contains x`, `git branch -v`, `git branch --list pat`,
`git switch master`, `git checkout master`, a trailered PowerShell `@'…'@` commit,
`Get-ChildItem -Recurse -Filter *.rs` outside artifact dirs. `git push --force` and
`git checkout -b x` return `ask`, never `deny` or allow. The smoke run also confirms
`additionalContext` from the ExitPlanMode hook reaches the session.
Also cover: `no_worktree_hook` (denies `isolation:"worktree"` and EnterWorktree, allows a
plain Agent call); the explicit-model guard (denies a missing `model`, ALLOWS a valid call
— a broken guard blocks every Agent call); `ref_reminder` fires once per session on an
`Edit` under `core/asyncevents` and on a PowerShell `verifyctl` command, silent the second
time; ExitPlanMode hook emits context and never a deny. Finally a live smoke: one real
hook invocation through Claude Code on Windows confirms `$CLAUDE_PROJECT_DIR` expansion.
(d) `[test-author]`, `model:"sonnet"`; then `proof-auditor` (tests are the risk surface).

## Deferred

- Concurrent-rollout guard hook (deny a second `cargo test`/`verifyctl`/`devctl up` while
  cargo/rustc runs) — enforces a MANDATORY rule but was not requested.
- `rust-nav` LSP daemon, `arcsem`-style index, Codex `.codex/agents/*.toml`.
- Existing memory files that copy rules are left as-is (decision 4 governs new ones).
