# Plan Writing Workflow

Detail for the **Plan Writing Workflow — MANDATORY** rule in [CLAUDE.md](../../CLAUDE.md). The 5-step skeleton stays in CLAUDE.md; this file holds the full elaboration of the steps that need it.

Front-load the thinking. For any plan (plan mode / "write me a plan" / a `docs/plans/…-plan.md`), run the steps in order — no skipping for "it's small".

## Step 1 — Ask how many research subagents

Bands (same as [research-mode.md](research-mode.md)): **2–4** narrow / **4–8** multi-module / **8–12** whole-repo survey. Ask **every time**, even mid-session — count is task-specific. Pass `model:` explicitly (Haiku read-and-list, Sonnet light reasoning).

## Step 2 — Research subagents on 3 non-overlapping angles

- *API surface* — every exported function / type / interface with full signatures; the module's `core.Module` methods (`Name`/`DependsOn`/`Init`/`Migrate`/`Start`/`Stop`); declared events (`core.Define`).
- *API usages* — concrete call sites: who constructs, who consumes, how args are filled; who `Require`s a service, who subscribes (`core.On`) to an event.
- *Patterns* — idioms to reuse: how existing modules register, migrate their schema, emit/subscribe, contribute an admin section.

Synthesize in the main model — never write a plan off a single subagent.

## Step 3 — Write concrete specifics

Exact files (repo-relative paths), exact function/type signatures, exact API calls drawn from step 2, sequencing + what each step compiles/tests against. **Banned phrases** (any one = research gap; go back to step 2): "figure out as we go", "TBD", "investigate during implementation", "may need to", "something like", "we'll see what shape this takes".

## Step 4 — Structure as an ordered step sequence, NOT a catalog

A catalog (files-to-create table + a list of call-sites + one big "build at the end") leaves the *implementation order, dependency topology, and per-step actions* as "figure as you go" — that is the failure mode and it is **banned**.

The plan body must be `Step 1 → Step 2 → …` where each step spells out, explicitly:

- **(a) what** is touched — exact files/symbols.
- **(b) why now / in what order** — the dependency that forces this step before the next (e.g. "declare the event payload in `<module>events` before the consumer subscribes"; "the schema migration before the store methods that query it").
- **(c) how** — the concrete actions, not just "add a module" but any non-mechanical move (declaring `DependsOn`, wiring `Provide`/`Require` to a consumer-defined interface, registering in `cmd/server/main.go`).
- **(d) dispatch tag** — `[inline]`, `[fable]`, `[opus]`, or `[sonnet]` (see [implementation-mode.md](implementation-mode.md) for the heuristic).

Steps do **NOT** each have to compile or pass tests in isolation — a step may leave the tree broken mid-rollout — but every step MUST be **written out**: a reader follows them top-to-bottom without inventing the order. Reference material (Context, Verified facts, file tables) is fine as supporting sections, but it does not replace the ordered steps — it feeds them.

## Step 5 — Dispatch a grumpy senior-engineer reviewer

One reviewer subagent at **session tier** (separate context — the independent-reviewer boundary is the point). **Ask the user the think-effort level first** (default / think / think hard / ultrathink) — effort does NOT inherit through the Agent tool, so embed the chosen level in the reviewer's prompt.

It hunts logical holes, missing pieces (schema `Migrate`? unit test? a declared `DependsOn` that doesn't match the real sync dependency? an event mutated instead of evolved additively? a module importing another module's package, or a cross-module foreign key?), ambiguity, unstated assumptions, rule conflicts, "figure-it-out-later" smell. It produces a punch list, does **not** rewrite. Address the list before showing the user (or note deferred items with rationale).
