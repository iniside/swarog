# Implementation Mode

Detail for the **Implementation Mode — MANDATORY** rule in
[`.agents/shared/planning-dispatch.md`](../../.agents/shared/planning-dispatch.md).
Cross-cutting Agent-call rules live in [subagent-dispatch.md](subagent-dispatch.md).
This file holds the lane heuristic, the implementation-specific dispatch shape, and refactor safety.

## Lanes — execution shape, not provider or model

Dispatch is decided **per plan step at plan-writing time**, not per session. Tags describe the execution and review shape. Shared tags:

- `[inline]` — main agent. Closed dispatch-threshold list only, or a typo/compile fix in a file this turn already has open.
- `[independent]` — top-tier separate context (`core-implementer`; visual/UI uses `mockup-implementer`).
- `[mechanical]` — cheap implementation. Visual/UI never this. Tests never this.
- `[test-author]` — only lane that writes tests; always a later step.
- `[review]` — `core-reviewer` (read-only).

Claude plans may write `[opus]` / `[fable]` → `[independent]`, `[sonnet]` → `[mechanical]`. Adapters own the concrete model slugs.

The user approves the tags together with the plan (call them out at ExitPlanMode) — that approval replaces the old blanket "inline or subagents?" question. Ask it only for untagged/ad-hoc work (no plan), and if any step is a subagent lane, also ask **"what effort level?"** (effort does NOT inherit — embed it in the prompt; see [subagent-dispatch.md](subagent-dispatch.md)). Mid-rollout, do not re-litigate a tag: a tagged step that turns out to need different handling gets a follow-up question, not a silent lane switch.

## How implementation dispatch differs from research

Implementation subagents run **sequentially per plan step** (no parallel fan-out for sequential steps), use the best available execution profile for the lane, are **read-write**, may **commit their own work**, and get a **diff review between tasks** instead of a synthesis pass. Research is the opposite: parallel fan-out, read-only, synthesized in the main model — [research-mode.md](research-mode.md).

## Dispatch rules (implementation-specific)

The shared effort/navigation/prompt rules are in [subagent-dispatch.md](subagent-dispatch.md). On top of those, implementation adds:

1. **Review between tasks.** Main model reviews each diff against the plan step (did what the plan said? touched out-of-scope files? introduced conflicting patterns — a module importing another module's impl crate, a cross-module foreign key, untyped event JSON outside a deliberate raw sink, or an event publish used where a sync capability was needed?) before dispatching the next. No parallel fan-out for sequential plan steps.
2. **Trust but verify.** Read the actual edits — self-reports describe intent, not result.
3. **Commit after each task or independently reviewable part.** Granular history beats one final rollout commit: `git add` + `git commit` immediately after each unit verifies. **Subagents MAY commit their own work** before main-model review. The main-model review still runs afterward; a bad commit is fixed with a follow-up commit, never by discarding history.

## Refactor safety

- **Verify dependency and wiring changes through the canonical runner:**
  `cargo run -p verifyctl -- --fast` performs the workspace build, clippy, tests,
  fortress/architecture gates, generated-contract checks, and live topology proof.
  Do not trust a grep saying "no consumers found"; make the Rust compiler and
  architecture checkers prove it.
- **Cargo rejects crate cycles, but not every forbidden acyclic edge.** A direct
  module-to-module implementation dependency may compile, so the `archcheck` and
  fortress stages remain mandatory. Resolve cross-domain work through the durable
  bus (async) or a provider contract trait resolved from the registry (sync), never
  by importing the other module's implementation crate.
- **Delete through dying chains.** When a file depends on a dying type, delete the file too — don't shim the survivor around the dying API. Ask "is the consumer still meaningful?", not "can it survive?".
