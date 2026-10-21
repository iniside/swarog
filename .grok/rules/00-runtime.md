# Grok runtime

This session is Grok, not Claude.

Read `AGENTS.md` and `.agents/adapters/grok.md`, then the authorities of the
matching `AGENTS.md` row. `CLAUDE.md` is loaded because this runtime cannot
skip a committed top-level copy; it is an anchor index with no rules of its
own. Do not dispatch Claude model slugs.

Shared lanes map through `.agents/adapters/grok.md`. Never invent a model
slug this runtime does not expose. Never use git worktrees
(`isolation: "worktree"` is banned; always `isolation: "none"`). Never
`resume_from` a `core-reviewer` or `proof-auditor`.
