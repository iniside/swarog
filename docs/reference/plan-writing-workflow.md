# Plan Writing Workflow

Detail for the **Plan Writing Workflow — MANDATORY** rule in
[`.agents/shared/planning-dispatch.md`](../../.agents/shared/planning-dispatch.md).
The skeleton stays there; this file holds the full elaboration. Shared plan
tags are `[inline]` / `[independent]` / `[mechanical]` / `[test-author]` /
`[review]`. Claude plans may still write `[opus]` / `[fable]` / `[sonnet]`;
adapters translate.

Front-load the thinking. For any plan (plan mode / "write me a plan" / a `docs/plans/…-plan.md`), run the steps in order — no skipping for "it's small".

## Step 1 — Ask how many research subagents

Bands (same as [research-mode.md](research-mode.md)): **2–4** narrow / **4–8** multi-module / **8–12** whole-repo survey. Ask **every time**, even mid-session—the count is task-specific. Use the best available research profile and keep provider/model names out of durable guidance.

## Step 2 — Research subagents on 3 non-overlapping angles

- *API surface* — every public function, type, and trait with full signatures; the
  module's `lifecycle::Module` methods (`name`/`requires`/`register`/`init`/
  `migrate`/`start`/`stop`); contract traits and `bus::define` event descriptors.
- *API usages* — concrete call sites: who constructs, who consumes, how arguments
  are filled; who uses `registry::require`, who subscribes with `on_tx`, and who
  contributes to or reads a shared slot.
- *Patterns* — idioms to reuse: how existing modules register, migrate their schema, emit/subscribe, contribute an admin section.

Synthesize in the main model — never write a plan off a single subagent.

## Step 3 — Write concrete specifics

Exact files (repo-relative paths), exact function/type signatures, exact API calls drawn from step 2, sequencing + what each step compiles/tests against. **Banned phrases** (any one = research gap; go back to step 2): "figure out as we go", "TBD", "investigate during implementation", "may need to", "something like", "we'll see what shape this takes".

## Step 4 — Structure as an ordered step sequence, NOT a catalog

A catalog (files-to-create table + a list of call-sites + one big "build at the end") leaves the *implementation order, dependency topology, and per-step actions* as "figure as you go" — that is the failure mode and it is **banned**.

The plan body must be `Step 1 → Step 2 → …` where each step spells out, explicitly:

- **(a) what** is touched — exact files/symbols.
- **(b) why now / in what order** — the dependency that forces this step before the next (e.g. "declare the event payload in `<module>events` before the consumer subscribes"; "the schema migration before the store methods that query it").
- **(c) how** — the concrete actions, not just "add a module" but any
  non-mechanical move (declaring `requires()`, wiring `registry::provide`/
  `registry::require` to a contract trait, contributing generated edge glue, and
  registering the module in `cmd/server/src/lib.rs` plus its service composition
  root).
- **(d) dispatch tag** — `[inline]`, `[independent]`, `[mechanical]`,
  `[test-author]`, or `[review]` (see [implementation-mode.md](implementation-mode.md)).
  Claude may write `[opus]` / `[fable]` / `[sonnet]`; adapters translate.

Steps do **NOT** each have to compile or pass tests in isolation — a step may leave the tree broken mid-rollout — but every step MUST be **written out**: a reader follows them top-to-bottom without inventing the order. Reference material (Context, Verified facts, file tables) is fine as supporting sections, but it does not replace the ordered steps — it feeds them.

## Step 5 — Dispatch `core-reviewer`

Write `subagent_type: "core-reviewer"` first. Never a generic implementer or
explore/plan type. Do not hand-author a persona. **Ask the think-effort
level first** — it does not inherit; embed it in the prompt. Punch list,
never a rewrite. Address it before showing the user, or note deferrals.

It hunts logical holes, missing pieces (module `migrate`? separate-file unit test?
a declared `requires()` capability that does not match the real synchronous
dependency? an event mutated instead of evolved additively? a module importing
another module's implementation crate, or a cross-module foreign key?), ambiguity,
unstated assumptions, rule conflicts, and "figure-it-out-later" smell. It produces
a punch list, does **not** rewrite. Address the list before showing the user (or
note deferred items with rationale).
