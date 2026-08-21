# Grok runtime

This session is Grok, not Claude.

Read `AGENTS.md`, then every file in `.agents/shared/`, then
`.agents/adapters/grok.md`. `CLAUDE.md` is loaded because this runtime
cannot skip a committed top-level copy. Ignore its Claude-only details:
`model:"opus"` / `"sonnet"` / `"haiku"` / `"fable"`, Claude hook paths,
Claude commit trailers, and `C:\Users\lukas\.claude\plans\`. Do not dispatch
those slugs even though `CLAUDE.md` is in context.

Shared lanes map through `.agents/adapters/grok.md`. Never invent a model
slug this runtime does not expose. Never use git worktrees
(`isolation: "worktree"` is banned; always `isolation: "none"`). Never
`resume_from` a `core-reviewer` or `proof-auditor`.
