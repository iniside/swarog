# GameBackend Agent Instructions

This folder is the shared instruction source for GameBackend agents.

`CLAUDE.md` is the Claude-specific copy of the same workflow substance plus
the Claude overlay (models, trailers, plan path, hook names). Keep it in
lockstep on workflow substance. Runtime adapters only translate tools, model
slugs, trailers, and plan-file paths.

## Read Order

1. Read every file in `.agents/shared/`.
2. Read exactly one runtime adapter in `.agents/adapters/` for the active agent.
3. Follow referenced durable technical docs in `docs/reference/` when the task
   touches that area.

## Files

- `.agents/shared/core-rules.md` — mistakes, docs, git safety, commit-after-task,
  commit format, comments, wipe / no dual-write / topology-blind, memory-sync.
- `.agents/shared/research-navigation.md` — research before planning, search
  mode, decisions-are-final, rust navigation.
- `.agents/shared/planning-dispatch.md` — plan writing, implementation lanes,
  `core-reviewer`, Fix the Authority, refactor safety.
- `.agents/shared/gamebackend.md` — architecture: three seams, hard constraints,
  13 fortresses + gateway including wallet, commands, one-rollout, wipe, layout.

Adapters (one per runtime) live under `.agents/adapters/` and map shared lanes
to that runtime's tools and trailers.

If an adapter conflicts with shared rules, the shared project rule wins unless
the adapter is only translating tool names for that runtime.
