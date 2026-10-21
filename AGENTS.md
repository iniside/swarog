# Swarog - Agent Reference

This file is the entry point for agents working in this repo. It does not
duplicate the full project policy; the shared rules live under `.agents/` so
runtime-specific instructions do not drift.

Swarog is a game backend in **Rust** (Cargo workspace): a **modular monolith with
a proven split** — one `cmd/server` binary for the monolith, and every domain
module also compiles and boots as its own `cmd/<name>-svc` process. The system
grows by addition (Open/Closed at the architecture level): new modules, contract
traits, and subscriptions, with no module importing another's implementation.
Architecture facts (seams, fortresses, commands, layout)
live in `.agents/shared/gamebackend.md` — do not treat this index as the
architecture doc.

## Severities

Every shared section is binding. The tier says what breaking it costs.

- **MANDATORY** (12 sections) — irreversible or days of lost work; nothing
  downstream catches it. Stop and get it right.
- **RULE** — binding, but recoverable: a compiler error, a reviewer, or a hook
  catches it. Follow it; do not stall over it.

### MANDATORY index

1. Research Before Planning — `.agents/shared/research-navigation.md`
2. Research Never Overrules a Decision — `.agents/shared/research-navigation.md`
3. Plan Writing Workflow — `.agents/shared/planning-dispatch.md`
4. Implementation Mode — `.agents/shared/planning-dispatch.md`
5. Adversarial Diff Review — `.agents/shared/planning-dispatch.md`
6. Fix the Authority, Not the Symptom — `.agents/shared/planning-dispatch.md`
7. Git Safety — `.agents/shared/core-rules.md`
8. Commit After Every Task — `.agents/shared/core-rules.md`
9. Comments — `.agents/shared/core-rules.md`
10. No Dual-Write, No Topology Branch, Wipe — `.agents/shared/core-rules.md`
11. Dev tooling scope — `.agents/shared/gamebackend.md`
12. One test rollout at a time — `.agents/shared/gamebackend.md`

## Read Before The Action

Read this file and the active runtime adapter in `.agents/adapters/` first.
Before each concrete action, load every authority named by its matching row.
Classify only that next action; if it is unclear, keep it read-only and use the
research row until it is classified. Reuse authorities already read this
session when their files have not changed. `.agents/README.md` maps the files.
All rules named by the matching rows are binding even if you failed to load
them. Shared behavior rules win; adapters translate runtime mechanics only.

| Action | Required authority |
| --- | --- |
| Edit source/config/docs/memory; add or commit | `.agents/shared/core-rules.md` |
| Research APIs/usages/data flow/overlap; navigate Rust | `.agents/shared/research-navigation.md`; `docs/reference/research-mode.md` |
| Write/review a plan | `.agents/shared/research-navigation.md`; `.agents/shared/planning-dispatch.md`; `docs/reference/research-mode.md`; `docs/reference/plan-writing-workflow.md` |
| Implement/delegate a step or review a diff/commit | `.agents/shared/core-rules.md`; `.agents/shared/planning-dispatch.md`; `docs/reference/implementation-mode.md`; `docs/reference/subagent-dispatch.md`; `docs/reference/core-failure-taxonomy.md` |
| Touch `core/`, `modules/`, `api/`, `cmd/`, `demos/`, `tools/`, or `weles/` | `.agents/shared/gamebackend.md` (for `tools/`/`weles/`, **Dev tooling scope**); for a new module, the `add-game-module` skill |
| Run, or delegate a step that runs, `cargo test`, `verifyctl`, or `devctl` | `safe-verification` skill; `.agents/shared/gamebackend.md` **One test rollout at a time**; `docs/reference/testing.md` |
| Build or change admin UI | `mockup-implementation` skill |

Durable technical references live in `docs/reference/`; workflow policy lives
in `.agents/`. Do not invent APIs, tools, models, or paths.

## Non-Negotiables

- Do not discard working-tree changes (`git stash`, `git checkout --`,
  `git restore`, force-push, or history rewrite without explicit instruction).
- At most one rollout-bearing command at a time (`devctl up`, `verifyctl`, or
  an ad-hoc `cargo test`); check for live cargo/rustc and an active fleet
  before launching another.
- Do not use git worktrees; keep `isolation: "none"`.
- Do not invent runtime model names or slugs. Use the active adapter's real
  tool/model mapping.
- Keep the fortress rule and wipe-as-migration strategy: no module→module
  imports, every domain module boots as `cmd/<name>-svc`, and schema/event
  contract breaks are DROP + boot fresh — no dual-writes or data-migration
  bridges.
