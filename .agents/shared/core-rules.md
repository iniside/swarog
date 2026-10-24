# Core Rules

## Owning Mistakes - RULE

If you ignore a rule or fabricate an API, path, behaviour, or claimed work,
name the mistake, state the correction, and fix it without excuses or praise.
Use two sentences; for a MANDATORY violation name the section and the mismatch.
Do not create memory copies of rules; propose a hook when enforcement is needed.

## Plans And Status Docs - RULE

Store all planning/design/status/progress/summary docs inside the repo — never
on a scratch drive or temp path. The repo is the single source of truth.

- Plans: `docs/plans/YYYY-MM-DD-HHMM-<kebab-topic>-plan.md`
- Status/progress/fix/summary:
  `docs/<subdir>/YYYY-MM-DD-HHMM-<kebab-topic>-<status|progress|fix|summary>.md`
- Reference (durable knowledge): `docs/reference/<topic>.md`

The `-HHMM` suffix is mandatory so files sort chronologically by listing. Never
put plan/status files at repo root or in a temp dir.

- TODO lists: only in `docs/todo/`, never in other `docs/` subfolders. A TODO
  list is a set of `- [ ]` task items; a plan's migration inventory table is a
  record, not a TODO list. One exception: feature-level status lives in
  `docs/roadmap/feature-tracker.md`; a feature first found during work becomes
  a tracker row with `(added YYYY-MM-DD)`. Every topic is a pair:
  `<kebab-topic>.md` (open work) and `<kebab-topic>-done.md` (closed work,
  empty until the first item closes), no date in the name. Topics are
  module/crate kebab names (`accounts`, `wallet`, `core-edge`, `splitproof`);
  cross-cutting topics use a name (`account-deletion`, `event-plane`,
  `agent-tooling`). Both files have a `Last updated: YYYY-MM-DD` line under
  the title, changed on every edit.
  - The open file has these sections, in this order, and nothing else:
    `## In progress` (someone started it and it is not finished; parked or
    merely partial/unverified items are not in progress), `## Owner checks`
    (optional; things only the owner can verify by hand — in a live fleet, a
    browser or a client — each naming what to check and where; no `###`
    subsections), `## Open` (not started; may hold `###` area subsections,
    with work deliberately postponed in a last `### Deferred` subsection),
    `## Notes` (optional background, design context, links, decisions,
    measured facts, rejected alternatives and non-goals under `### Not doing`;
    may hold `###` subsections; never a task). Every task is a
    `- [ ] **Short title**` line with any detail indented under it. The title
    is a short noun phrase naming the task. Empty `In progress`/`Open`
    sections stay with a `_None._` line.
  - Leftovers of a plan step that the owner chose to defer go under
    `### Known limits (<plan-slug> step N)` in `## Open`, where `<plan-slug>`
    is the plan file name without the date and `-plan.md`
    (`2026-10-23-2021-todo-tracking-plan.md` → `todo-tracking`). Leftovers
    with no plan step use `### Known limits (<area>)`.
  - The done file lists `- [x] **Short title** (YYYY-MM-DD, <ref>)` items,
    newest first, with detail indented under them. `<ref>` is exactly one of:
    `<plan-slug> step N`, a commit sha (7–12 hex), or `no ref`; an unknown
    date is `date unknown`. A done file with nothing closed holds a single
    `_None._` line.
  - A TODO item is a real gap in a feature or a real bug, in the product or
    tool, that someone must fix later. Session and process notes are never
    items: a fleet or server not restarted, a build, test, review or
    measurement not run in this session, a commit that took someone else's
    file. Say those in the report instead.
  - A leftover found during work (an unfinished part, a bug, something that
    "could not be done", a review leftover, an owner decision pending) is
    fixed in the same step when it lies in the step's files and scope. What
    remains is not written to a TODO file by the implementer: it is listed in
    the report under **Leftovers for owner**, each with what it is and its
    user-visible effect, the concrete reason it was not fixed (out of scope,
    missing design decision, other module's fortress), the cost of fixing it
    now, and a recommendation (fix now or defer). The owner decides per item;
    the lead fixes the "fix now" items in the same task and writes only the
    deferred ones to the TODO file.
  - Before creating a TODO topic, list `docs/todo/` and add to a matching
    file; create a new pair only when none fits, so the TODO set stays small
    enough to track.
  - Starting an item moves it to `In progress` (the lead, before dispatching
    the step that works on it); finishing it moves it to the done file with
    the date and the plan step or commit that closed it (the step's
    implementer, in the step commit).
  - Before committing a plan step or named fix, re-check the open items of the
    TODO topic for the area you changed (Known limits included) and move every
    item your change fixed or made obsolete to the done file in the same
    commit; leave items you cannot prove closed untouched.
- When a plan is finished but some of its steps have no step commit, add a
  `Status: done` line under the plan title. When a plan or its finished work
  waits for the owner's approval or acceptance, the line is
  `Status: waiting for approval` instead, replaced once the owner decides.

Work lands directly on `master`; do not create a branch unless the user
explicitly requests one. Do not use git worktrees.

## Agent Effort - RULE

Every subagent runs at effort `medium`. A plan may propose another effort for
a specific step, written in that step with a one-line reason; the owner
approves it with the plan. Outside a plan, a different effort needs the
owner's explicit request. Never raise effort on your own judgment.

## Git Safety - MANDATORY

Never `git stash`, `git checkout -- <file>`, `git restore`, or anything that
discards/overwrites uncommitted working-tree changes without the user's
say-so. To inspect old contents use `git show <sha>:<path>`.

Only ever `git reset --soft HEAD~1` to undo a commit *you* just created *this
turn*, and only when nothing else has committed since. Never `git reset` past
commits you did not make this turn. Never `git push --force` or rewrite
published history without explicit instruction. Push only when the user asks.

## Commit After Every Task - MANDATORY

After completing every task — or each independently reviewable, verified part
of a larger task — create a git commit containing only the changes made for
that unit. Do not wait for a long multi-part rollout to finish, and do not
include unrelated pre-existing working-tree changes. If a task produces no
repository changes, no commit is required. A request to commit is implicit in
every task; pushing still requires an explicit user request.

## Commit Message Format - RULE

Use Conventional Commits:

```text
<type>(<scope>): <imperative description>
```

`type` is one of `feat`, `fix`, `perf`, `refactor`, `test`, `docs`, `chore`.
`scope` is the lowercased module/package; multiple scopes are comma-separated
(`fix(match,rating): …`). Do not use bracketed `[Module]` scopes. A commit
implementing a plan step, including its review fixes, ends the description with
`(<plan-slug> step <id>)` (`steps 3-4` for a range); the slug is the plan file
name without the date prefix and `-plan.md`. A commit fixing review findings
adds `Kind: review-fix` to the final trailer paragraph. The git `commit-msg`
hook checks the subject shape, that a subject naming `step <id>` ends with the
suffix and an existing `docs/plans` slug, and the `Kind` value, count and
placement. It cannot tell whether a commit belongs to a step or fixes review
findings: that is on the committer and on review. Never skip the hook
(`--no-verify` / `-n`), fix the message.

The `Co-Authored-By` trailer identifies the model/agent that executed the work.
The active adapter defines the format; the executing model fills in its own
identity from its runtime identity, never copied from an example. More detail:
`docs/reference/commit-format.md`.

## Comments - MANDATORY

Default is no comment. Write one only when it carries what the code cannot
show: a non-obvious invariant, a Postgres/tokio/sqlx gotcha, the reason for a
workaround, or intent invisible from the signature. One line, present tense,
describing what the code does. Doc comments (`///`) on a public contract
surface (`api/*` traits, `core/*` public items) document the contract and are
held to the same truth standard.

Banned: changelog prose ("removed X", "moved to Y", "this used to…", "(Step
3)"); paraphrase of the next line; multi-line blocks restating the body;
comments asserting behaviour the code does not have; exclusivity claims ("the
only place…"). A false comment is a correctness defect — fix it in the same
rollout, never "later". A claim about library or runtime behaviour (tokio,
sqlx, Postgres, quinn) requires reading that code this session and a
`file:line` citation.

Verbose comments hijack review. Findings that are only about comments mean
there are too many comments, not that the diff is clean. A comment flagged in
review is deleted, not reworded. Comment-only findings end the review round and
the orchestrator fixes them inline; no prose-fixing agent is dispatched.

Every implementation dispatch says `comments: default NONE` and names the one
or two things that earn a line in that file.

## No Dual-Write, No Topology Branch, Wipe - MANDATORY

Wipe is the migration strategy (current phase). When a schema or event-contract
change would need a data migration, DROP the affected schemas (or the whole
DB) and boot fresh. Do not build bridges, dual-writes, backfills, versioned
data-migration machinery, or "kept for compatibility" fields. Module `migrate`
stays idempotent DDL (`CREATE … IF NOT EXISTS`), nothing more. If losing dev
data hurts, the answer is a seed script, not a migration.

Modules are topology-blind. No `Option<transport>`, no `if split`, no env
topology branches in domain code. Edge exposure goes through `EDGE_SLOT`;
remote resolution through the registry swap; durable delivery through the bus.
`cmd/*` mains differ only in module list + which QUIC planes the process
serves.

Never keep two rails for the same behaviour. When a step introduces the new
thing, that same step deletes the old one. Two sources of truth are worse than
a broken intermediate build.

## Agent Memory Backup - RULE

Project memory lives outside the repo under the Claude Code scheme
(`$HOME/.claude/projects/<mangled-repo-path>/memory/`) and is mirrored into
the repo at `memory/` so it survives across machines via git. The live path is
the one `scripts/memory-sync` derives, overridable with `CLAUDE_MEMORY_DIR`.
`… path` prints it. The `.ps1` twin and the no-exec-bit invocation form are in
`docs/reference/platform-notes.md`.

- After ANY change to memory (write/update/delete a memory file or
  `MEMORY.md`), run `scripts/memory-sync.sh push` (or `.ps1`) — it mirrors
  live → `memory/` and commits `chore(memory): …`. Do not hand-copy.
- After a `git pull`/sync, run `scripts/memory-sync.sh pull` before relying on
  recall.
