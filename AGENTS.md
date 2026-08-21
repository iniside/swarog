# GameBackend - Agent Reference

This file is the entry point for agents working in this repo. It does not
duplicate the full project policy; the shared rules live under `.agents/` so
runtime-specific instructions do not drift.

A for-fun game backend in **Rust** (Cargo workspace): a **modular monolith with
a proven split** — one `cmd/server` binary for the monolith, and every domain
module also compiles and boots as its own `cmd/<name>-svc` process. Features
are added by writing new code, not modifying existing code (Open/Closed at the
architecture level). Architecture facts (seams, fortresses, commands, layout)
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

## Required Read Order

1. Read `.agents/README.md`.
2. Read every file in `.agents/shared/`.
3. Read exactly one adapter for the active runtime under `.agents/adapters/`
   (adapters may land after this index; still read the one that matches the
   runtime you are running under when present).
4. Read any referenced `docs/reference/*.md` file for the area you are touching.

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

Durable technical references remain in `docs/reference/`; `.agents/` is for
agent behavior, workflow, and runtime adapters.
