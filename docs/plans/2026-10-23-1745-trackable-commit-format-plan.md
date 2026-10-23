# Trackable commit format (port from ArcGame)

## Context

Owner (2026-10-23): "format commitów, który da się trackować będzie przydatny" — after
reviewing ArcGame's workflow changes. Requested outcome: swarog commits follow a
machine-checked format so plan/step progress can be read from `git log` —
`<type>(<scope>): <imperative> (<plan-slug> step <id>)` plus a `Kind: review-fix`
trailer marking commits that fix review findings.

Exclusions: ArcDesk integration is undecided ("nie wiem czy arcdesk będzie czytał
swaroga") — nothing ArcDesk-specific. The ArcGame `docs/todo/` tracking rules are not
part of this plan.

### Overlap / why not reuse as-is

- **ArcGame `.claude/hooks/commit_format.py`** (working copy): single source of the rule —
  `check_subject` (shape, step suffix last, slug exists under `docs/plans`) and
  `check_message` (subject + `Kind:` trailer placement/uniqueness/value; the working copy
  allows `Kind: review-fix` on off-plan named fixes, no suffix required). Self-contained;
  `PLANS_DIR = parents[2]/docs/plans` already fits swarog (flat `docs/plans/`, `rglob`
  works). Ported almost verbatim.
- **ArcGame `git_safety_hook.py`** re-parses the subject out of the shell command for an
  early PreToolUse check. **Not ported**: swarog's `git_safety_hook.py` is a different
  design on `.claude/hooks/shell_data.py`; the git `commit-msg` hook sees the final
  message (heredoc, `-F`, here-string, editor) and covers every committer (Codex, Cursor,
  Grok, SourceGit), and its rejection reaches the agent on stderr. It is the single
  enforcement point.
- **swarog `git_safety_hook.py`** keeps its `Co-Authored-By` check and gains only the
  `--no-verify` / `-n` denial (ArcGame has the same denial,
  `G:/ArcGame/.claude/hooks/git_safety_hook.py:545`), so agents cannot skip the hook.
- Known gap, accepted and documented: a clone without the installed hook is unchecked
  (install is one `cp` per clone, as in ArcGame). No install-detection guard.
- Verified: `.git/hooks/` holds only samples; `core.hooksPath` unset. History has two
  non-conforming subjects (`docs: …`, `revert(storage): …`); only new commits are checked.

## Steps

### Step 0 — plan copy and hook install `[inline]`

(a) `docs/plans/2026-10-23-1745-trackable-commit-format-plan.md` (this plan, links
relative); `.git/hooks/commit-msg` (untracked).
(b) First: every step commit below ends with `(trackable-commit-format step N)`, and the
checker rejects a slug without a matching `docs/plans` file.
(c) Copy and commit the plan: `docs(plans): add the trackable commit format plan`
(no step mention, passes the checker). The hook itself is installed at the end of step 1
(the shim does not exist before it).

### Step 1 — commit_format.py, commit-msg shim, --no-verify guard `[opus]`

(a) New `.claude/hooks/commit_format.py`, new `.claude/hooks/git-commit-msg.sh`
(tracked as mode `100755` via `git update-index --chmod=+x`); edit
`.claude/hooks/git_safety_hook.py` (commit loop in `deny_reason`);
`.claude/hooks/tests/test_hooks.py:444` exclusion set.
(b) Tests, docs and install depend on these files.
(c)
- `commit_format.py`: copy of the ArcGame working copy. Changes: `HELP` example →
  `docs(agents): port the commit format (trackable-commit-format step 3)`; add
  `sys.dont_write_bytecode = True` like the other swarog hooks. CLI
  `python commit_format.py <msg-file>` → exit 1 + stderr on violation; wrong argc or
  unreadable file → exit 0.
- `git-commit-msg.sh`: the ArcGame 6-line shim verbatim (warns and exits 0 if `python`
  is missing).
- `git_safety_hook.py`, new helper `skips_verify(args)` called in the existing
  `for args in git_args(code, "commit")` loop before the `--amend/--no-edit` `continue`.
  Note `shell_data.unquote_words` already blanks multi-word quoted values to `''`, so a
  message never splits into flag-like tokens; single-word values still need skipping:
  - stop at `--`;
  - long option: deny when `len(a) >= 6 and "--no-verify".startswith(a)`
    (`--no-v` … `--no-verify`; never `--no-verbose`/`--no-edit`); skip the next token
    after a value-taking long option given without `=` (`--message --file --template
    --author --date --cleanup --reuse-message --reedit-message --fixup --squash
    --trailer --pathspec-from-file`);
  - short cluster `-[a-zA-Z]+`: scan letters left to right, deny on `n`, stop at the
    first value-taking letter (`m F C c t u S`); if that letter is last in the cluster,
    skip the next token. So `-nm` denies, `-am` / `-uno` / `-mnote` do not.
    Erratum (landed c67a05d1): `u` and `S` take only a glued value, so they never skip
    the next token; only `m F C c t` do.
  - Reason: `core-rules.md Commit Message Format: never skip the commit-msg hook
    (--no-verify / -n); fix the message instead.`
- After committing step 1 (the commit itself runs before the hook exists), install:
  `cp .claude/hooks/git-commit-msg.sh .git/hooks/commit-msg`, smoke
  `python .claude/hooks/commit_format.py <tmp>` with a bad and a good subject (exit 1 / 0).
- comments: default NONE.
Commit: `feat(hooks): commit-msg format check and no-verify guard (trackable-commit-format step 1)`.

### Step 2 — tests `[test-author]` (sonnet)

(a) `.claude/hooks/tests/test_hooks.py`: new `CommitFormat` class, new `NEW_GIT` cases.
(b) After step 1 landed.
(c)
- `check_message` (import the module; point `commit_format.PLANS_DIR` at a temp dir
  holding `2026-01-01-0000-demo-plan.md`): shape ok / no scope / `[Scope]` / unknown type
  / `revert(x)`; exempt prefixes; step id without suffix; suffix not last; `steps 3-4`
  ok; unknown slug with close-match hint; prose "step" without id ok; `Kind: review-fix`
  in the final trailer paragraph ok with and without suffix; `Kind:` in a body paragraph,
  two `Kind:`, `Kind: other` → reject; `#` lines and the scissors line ignored.
- Shim end-to-end: temp git repo with the shim as `commit-msg` and a `docs/plans` fixture
  plus a copied `commit_format.py`; assert a bad subject fails AND stderr contains the
  checker's message (not only the exit code), a good one succeeds. Skip with a reason
  when `sh` or `python` is not on PATH (fail-open would make the assertion vacuous).
- `git_safety_hook` under Bash and PowerShell: deny `--no-verify`, `--no-veri`, `-n`,
  `-nm 'x'`, `-an`; silent for `-am 'x'`, `-uno`, `--no-verbose`, `--no-edit --amend`, and
  `-m 'docs(agents): deny --no-verify and -n'` (all with the trailer).
Commit: `test(hooks): cover the commit format check and no-verify guard (trackable-commit-format step 2)`.

### Step 3 — docs `[docs]` (sonnet)

(a) `.agents/shared/core-rules.md` **Commit Message Format**;
`docs/reference/commit-format.md`; `.agents/adapters/claude.md` **Commit Trailers**;
`.agents/shared/planning-dispatch.md` (Implementation Mode parallel-review fix commit
~:141, **Adversarial Diff Review** round fixes); `docs/reference/core-failure-taxonomy.md`
class 13 (~:307, :352-353); commit paragraphs of `.claude/agents/{core-implementer,
test-author,docs-writer,mockup-implementer}.md` and `.grok/agents/core-implementer.md`;
`.agents/adapters/{codex,cursor,grok}.md` where they restate the template.
(b) Last, against the landed hooks.
(c)
- `Kind: review-fix`: on commits fixing review findings of a plan step or a named fix,
  in the final trailer paragraph with `Co-Authored-By` (one `-m` for all trailers).
- Enforcement: `commit_format.py` via git `commit-msg` (install
  `cp .claude/hooks/git-commit-msg.sh .git/hooks/commit-msg` once per clone; unchecked
  without it; fails open without python); `git_safety_hook.py` denies `--no-verify`/`-n`.
  Known gaps: uninstalled clone, `git -c core.hooksPath=…`.
- commit-format.md: **Enforcement** and **Kind trailer** sections with accepted/rejected
  examples in swarog scopes; every commit closing a step ends with the suffix.
- Taxonomy class 13 also audits the `Kind` trailer on review-fix commits.
Commit: `docs(agents): document the checked commit format and Kind trailer (trackable-commit-format step 3)`.

## Review

After each task commit a fresh `core-reviewer` (model ≥ author tier, effort medium) on
the commit + cumulative diff. Steps 1 and 2 also get `proof-auditor` (the commit-msg hook
is a gate and its tests are the risk surface). Review-fix commits use the new trailer.

## Verification

- `python -B -m unittest discover .claude/hooks/tests` green (not a cargo rollout).
- The installed hook checks this plan's own step 2 and 3 commits.
