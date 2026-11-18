# Swarog - Agent Reference

This file is the entry point for agents working in this repo. It does not
duplicate the full project policy; the shared rules live under `.agents/`.

Swarog is a game backend in **Rust** (Cargo workspace): a **modular monolith with
a proven split** — one `cmd/server` binary for the monolith, and every domain
module also compiles and boots as its own `cmd/<name>-svc` process. The system
grows by addition (Open/Closed at the architecture level): new modules, contract
traits, and subscriptions, with no module importing another's implementation.
Architecture facts (seams, fortresses, commands, layout)
live in `.agents/shared/gamebackend.md` — do not treat this index as the
architecture doc.

## Rules

Every section in `.agents/shared/` and `.claude/rules/` is binding. Hooks
enforce git safety (no stash, restore or checkout over uncommitted work), the
commit subject format and trailer, an explicit `model` on every Agent call (plus
`effort` for built-in agent types), and no worktrees.

## Read Before The Action

Read this file and `.agents/adapters/claude.md` first. Before each concrete
action, load every authority named by its matching row. Classify only that
next action; if it is unclear, keep it read-only and use the research row
until it is classified. Reuse authorities already read this session when
their files have not changed. `.agents/README.md` maps the files.

| Action | Required authority |
| --- | --- |
| Edit source/config/docs/memory; add or commit | `.agents/shared/core-rules.md` |
| Research APIs/usages/data flow/overlap; navigate Rust | `.agents/shared/research-navigation.md` |
| Write/review a plan | `.agents/shared/research-navigation.md`; `.agents/shared/planning-dispatch.md` |
| Implement/delegate a step or review a diff/commit | `.agents/shared/core-rules.md`; `.agents/shared/planning-dispatch.md` |
| Touch `core/`, `modules/`, `api/`, `cmd/`, `demos/`, `tools/`, or `weles/` | `.agents/shared/gamebackend.md`; the `.claude/rules/` file matching the path; for a new module, the `add-game-module` skill |
| Run, or delegate a step that runs, `cargo test`, `verifyctl`, or `devctl` | `safe-verification` skill; `.agents/shared/gamebackend.md` **One test rollout at a time**; `docs/reference/testing.md` |
| Build or change admin UI | `mockup-implementation` skill |

Durable technical references live in `docs/reference/`; workflow policy lives
in `.agents/`. Do not invent APIs, tools, models, or paths.

## Non-Negotiables

- At most one rollout-bearing command at a time (`devctl up`, `verifyctl`, or
  an ad-hoc `cargo test`); check for live cargo/rustc and an active fleet
  before launching another.
- No module→module imports, every domain module boots as `cmd/<name>-svc`,
  and schema/event contract breaks are DROP + boot fresh — no dual-writes or
  data-migration bridges.
