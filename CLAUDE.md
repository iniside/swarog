# GameBackend

`AGENTS.md` is the universal mandatory core and action-trigger table. Read it
and `.agents/adapters/claude.md` first, then load every authority required by
the matching action row before acting. Shared files define behavior; the Claude
adapter translates runtime tools, models, effort, trailers, and plan artifacts.

The headings below are stable anchors only. Each points at the one section that
owns the rule. The full set of files to read before an action is always the
matching `AGENTS.md` row, never this file.

@AGENTS.md

## The point of this codebase

`.agents/shared/gamebackend.md` **The point of this codebase**.

## Hard constraints

`.agents/shared/gamebackend.md` **Hard constraints (do not violate without discussing)**.

## Adding a module

`.agents/shared/gamebackend.md` **Adding a module (the recipe)**.

## Domain modules

`.agents/shared/gamebackend.md` **Domain modules (18 fortresses + gateway)**.

## Commands

`.agents/shared/gamebackend.md` **Commands**.

## Dev tooling scope — MANDATORY

`.agents/shared/gamebackend.md` **Dev tooling scope**.

## One test rollout at a time — MANDATORY

`.agents/shared/gamebackend.md` **One test rollout at a time**.

## Database

`.agents/shared/gamebackend.md` **Database**.

## Layout

`.agents/shared/gamebackend.md` **Layout**.

## Owning Mistakes — RULE

`.agents/shared/core-rules.md` **Owning Mistakes**.

## Research Before Planning — MANDATORY

`.agents/shared/research-navigation.md` **Research Before Planning**.

## Research / Search Mode — RULE

`.agents/shared/research-navigation.md` **Research / Search Mode**.

## Navigation — RULE

`.agents/shared/research-navigation.md` **Navigation**.

## Research Never Overrules a Decision — MANDATORY

`.agents/shared/research-navigation.md` **Research Never Overrules a Decision**.

## Plans & Status Docs — RULE

`.agents/shared/core-rules.md` **Plans And Status Docs**; plan artifact in
`.agents/adapters/claude.md`.

## Plan Writing Workflow — MANDATORY

`.agents/shared/planning-dispatch.md` **Plan Writing Workflow**.

## Implementation Mode — MANDATORY

`.agents/shared/planning-dispatch.md` **Implementation Mode**; lanes in
`.agents/adapters/claude.md`.

## Fix the Authority, Not the Symptom — MANDATORY

`.agents/shared/planning-dispatch.md` **Fix the Authority, Not the Symptom**.

## Adversarial Diff Review — MANDATORY

`.agents/shared/planning-dispatch.md` **Adversarial Diff Review**.

## Refactor Safety

`.agents/shared/planning-dispatch.md` **Refactor Safety**.

## Comments — MANDATORY

`.agents/shared/core-rules.md` **Comments**.

## No Dual-Write, No Topology Branch, Wipe — MANDATORY

`.agents/shared/core-rules.md` **No Dual-Write, No Topology Branch, Wipe**.

## Agent Memory Backup — RULE

`.agents/shared/core-rules.md` **Agent Memory Backup**.

## Commit After Every Task — MANDATORY

`.agents/shared/core-rules.md` **Commit After Every Task**.

## Git Safety — MANDATORY

`.agents/shared/core-rules.md` **Git Safety**.

## Commit Message Format — RULE

`.agents/shared/core-rules.md` **Commit Message Format**; trailer in
`.agents/adapters/claude.md`.
