# Research / Search Mode

Detail for the **Research / Search Mode — MANDATORY** rule in [CLAUDE.md](../../CLAUDE.md). The rule itself (ask "how should I research this?"; grep is a lower bound; the "non-trivial" definition) stays in CLAUDE.md — this file holds the method menu and the research-specific dispatch shape. Cross-cutting Agent-call rules (explicit `model:`, effort/chain don't inherit, trailer, concise prompts) live in [subagent-dispatch.md](subagent-dispatch.md).

## Why not just grep

One grep pass is lossy here — it misses interface satisfaction (a type implements an interface with no textual reference to it), embedded/promoted methods, generated code, event subscribers wired by string topic (`core.Define`/`core.On`), and the registry/reflection-driven surface (`Provide`/`Require`, `Contribute`/`Contributions`). Treat any single grep sweep as a **lower bound, not the answer**, and always say which method you used.

## Method menu

Offer the fitting subset:

- **LSP / gopls** — Go symbol nav with a file+line anchor: definition, references, implementations (preferred for "where is X defined / who calls Y / what satisfies this interface"). The interface-implementations query is the one grep can't do.
- **Parallel research subagents** — fan out cheap subagents, each a distinct **non-overlapping** angle (e.g. API surface / callers+consumers / event publishers+subscribers / config+env wiring). If picked, ask **"how many?"** (bands below). Dispatch mechanics per [subagent-dispatch.md](subagent-dispatch.md) — every one gets the nav guidance pasted in (it does not inherit) and reports which method it used.
- **Targeted main-model read** — small surface, one file end-to-end.
- **Grep/Glob** — only when nothing else fits; acknowledge it's a lower bound.

## How research dispatch differs from implementation

Research subagents fan out **in parallel** (multiple at once, distinct angles), run on **cheap models** (Haiku read-and-list, Sonnet light reasoning), are mostly **read-only**, don't commit, and are **synthesized in the main model** — never write a conclusion off a single subagent. (Implementation is the opposite: sequential per plan step, lane→model, review each diff, may commit — [implementation-mode.md](implementation-mode.md).)

## Subagent count bands

Asked every time a fan-out is picked (count is task-specific, even mid-session):

- **2–4** — narrow / single-module.
- **4–8** — multi-module.
- **8–12** — whole-repo survey.
