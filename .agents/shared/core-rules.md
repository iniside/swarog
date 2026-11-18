# Core Rules

## Owning Mistakes

If you ignore a rule or fabricate an API, path, behaviour, or claimed work,
name the mistake, state the correction, and fix it without excuses or praise.
Use two sentences; name the violated section and the mismatch.
Do not create memory copies of rules; propose a hook when enforcement is needed.

## Plans And Status Docs

Store all planning/design/status/progress/summary docs inside the repo, never
on a scratch drive or temp path.

- Plans: `docs/plans/YYYY-MM-DD-HHMM-<kebab-topic>-plan.md`
- Status/progress/fix/summary:
  `docs/<subdir>/YYYY-MM-DD-HHMM-<kebab-topic>-<status|progress|fix|summary>.md`
- Reference (durable knowledge): `docs/reference/<topic>.md`
- TODO lists: only in `docs/todo/`, no date prefix.

The `-HHMM` suffix is mandatory. Never put plan/status files at repo root or in
a temp dir. The TODO file format, done-file refs, `Status:` lines and
close-on-commit are in `.claude/rules/status-docs.md` (loaded by path in Claude
Code; read it before editing `docs/todo/`, `docs/plans/` or `docs/roadmap/`).

A leftover found during work (unfinished part, bug, review leftover, pending
owner decision) is fixed in the same step when it lies in the step's files and
scope. What remains is never written to a TODO file by the implementer: list it
in the report under **Leftovers for owner**, each with what it is and its
user-visible effect, why it was not fixed, the cost of fixing it now, and a
recommendation (fix now or defer). The owner decides per item.

Work lands directly on `master`; do not create a branch unless the user
explicitly requests one. Do not use git worktrees.

## Agent Effort

Subagents run at effort `medium`, `low` for the search lane, pinned in each
`.claude/agents/*.md` definition; a built-in agent type passes `effort`
explicitly (`.claude/hooks/explicit_model_guard.py` blocks the call
otherwise). The owner may set another effort for a specific step or request.

## Git Safety

Never `git stash`, `git checkout -- <file>`, `git restore`, or anything that
discards/overwrites uncommitted working-tree changes without the user's
say-so. To inspect old contents use `git show <sha>:<path>`.

Only ever `git reset --soft HEAD~1` to undo a commit *you* just created *this
turn*, and only when nothing else has committed since. Never `git reset` past
commits you did not make this turn. Never `git push --force` or rewrite
published history without explicit instruction. Push only when the user asks.

## Commit After Every Task

After completing every task — or each independently reviewable, verified part
of a larger task — create a git commit containing only the changes made for
that unit. Do not wait for a long multi-part rollout to finish, and do not
include unrelated pre-existing working-tree changes. If a task produces no
repository changes, no commit is required. A request to commit is implicit in
every task; pushing still requires an explicit user request.

## Commit Message Format

Conventional Commits: `<type>(<scope>): <imperative description>`; a plan-step
commit ends `(<plan-slug> step <id>)`, a review-fix commit adds `Kind: review-fix`
to the trailer paragraph. The `commit-msg` hook enforces the shape: never skip it
(`--no-verify`), fix the message. The `Co-Authored-By` trailer names the model
that did the work, in the adapter's format. Detail: `docs/reference/commit-format.md`.

## Comments

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

## No Dual-Write, No Topology Branch, Wipe

Wipe is the migration strategy (current phase). When a schema or event-contract
change would need a data migration, DROP the affected schemas (or the whole
DB) and boot fresh. Do not build bridges, dual-writes, backfills, versioned
data-migration machinery, or "kept for compatibility" fields. Module `migrate`
is create-only DDL through `lifecycle::Ddl` (`core/lifecycle/src/ddl.rs`), never
an alter or replace of an existing object, nothing more. If losing dev
data hurts, the answer is a seed script, not a migration.

Modules are topology-blind. No `Option<transport>`, no `if split`, no env
topology branches in domain code. Edge exposure goes through `EDGE_SLOT`;
remote resolution through the registry swap; durable delivery through the bus.
`cmd/*` mains differ only in module list + which QUIC planes the process
serves.

Never keep two rails for the same behaviour. When a step introduces the new
thing, that same step deletes the old one. Two sources of truth are worse than
a broken intermediate build.

## Agent Memory Backup

After ANY change to agent memory (a memory file or `MEMORY.md`), run
`scripts/memory-sync.sh push`; after a `git pull`, run `pull`. Procedure: the
`memory-backup` skill.
