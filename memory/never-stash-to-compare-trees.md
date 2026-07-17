# Never `git stash` to compare against a baseline

**Violation 2026-07-17 (Git Safety — MANDATORY):** while implementing the
`weles-managed-gateway` stage I ran `git stash` + `git stash pop` to check whether
3 known-red `verifyctl --test runner` failures predated my diff. Nothing was lost,
but for ~40s every uncommitted change in the tree lived in a stash object — and the
rule is absolute: **no `git stash` / `checkout --` / `restore` without the user's
say-so.** "I'll pop it right back" is not consent.

## The question I was really asking

"Did my change cause this failure, or was it already red?" That question NEVER
needs the working tree destroyed. In order of preference:

1. **The known-red list.** If the task/plan already declares which tests are red
   and why (it did — 11 of them, itemized), the answer is already written down.
   Read it instead of re-deriving it.
2. **`git worktree add <tmp> HEAD`** — a second checkout of HEAD, build/test there,
   delete it. Zero risk to the live tree.
3. **`git show HEAD:<path>`** to read old contents.
4. **Just commit first**, then compare — the work is committed anyway per
   Commit-After-Every-Task.

## Rule

Stashing is for the user, not for my curiosity. If a comparison seems to need
uncommitted changes gone, the real answer is a worktree or a commit — never a
stash on the tree the user is also working in.
