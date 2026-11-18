# Research / Search Mode

Detail for the **Research / Search Mode** rule in
[`.agents/shared/research-navigation.md`](../../.agents/shared/research-navigation.md).
This file holds the method menu and research-specific dispatch shape. Ask
"how should I research this?" only when the method changes the answer. There
is no fixed fan-out count. Cross-cutting
effort/navigation/prompt rules live in [subagent-dispatch.md](subagent-dispatch.md).

## Why not just grep

One grep pass is lossy here — it misses trait implementations, generated RPC glue,
macro-expanded operation metadata, durable event subscriptions assembled through
typed descriptors, and registry/contribution consumers reached through shared keys
and slots. Treat any single grep sweep as a **lower bound, not the answer**, and
always say which method you used.

## Method menu

Offer the fitting subset:

- **LSP / rust-analyzer** — Rust symbol navigation from a concrete
  file+line+column anchor: definition, references, trait implementations, call
  hierarchy, and inferred type information (preferred for "where is X defined /
  who calls Y / what implements this trait"). Start with one targeted `rg` result
  to establish the anchor, then query rust-analyzer; implementation and call
  hierarchy results are the part a text sweep cannot prove.
- **Parallel research subagents** — fan out subagents, each a distinct **non-overlapping** angle (e.g. API surface / callers+consumers / event publishers+subscribers / config+env wiring). Dispatch mechanics per [subagent-dispatch.md](subagent-dispatch.md)—every one gets the nav guidance pasted in and reports which method it used.
- **Targeted main-model read** — small surface, one file end-to-end.
- **Grep/Glob** — only when nothing else fits; acknowledge it's a lower bound.

## Search budget

Every search result and file view stays in the agent's context and is re-read
on every later turn, so its cost grows with the length of the task.

- Rust symbol (definition, callers, implementations, type): LSP, with the name
  position from `workspaceSymbol`. Grep only when LSP returns nothing, and say so.
- Non-Rust text and strings: grep always scoped to a folder, never the repo
  root. List first (`rg -l` / `rg -c`), then read matches with `head_limit` and
  at most `-C 2`.
- File views: about 100 lines around the anchor; a whole file only when the step
  changes most of it. Never view a range already in context unless the file was
  edited since.
- A search that takes more than one or two lookups goes to the search lane.

On Claude, `.claude/hooks/scout_nav_guard.py` enforces this for
`core-implementer`, `test-author` and `docs-writer`; the main session and the
review and proof lanes are not gated. A Rust-capable search (Grep tool, Bash
`rg`, `grep` or `git grep`) over the repo root or a bare `core`, `modules`,
`api`, `cmd`, `tools` or `demos` is denied until the agent has made a
position-based LSP call (`hover`, `goToDefinition`, `findReferences`,
`goToImplementation`, `incomingCalls`, `outgoingCalls`, `prepareCallHierarchy`)
or dispatched `code-scout`; two denials at most, then it passes. A Bash `rg` or
`grep` for plain text or a deleted symbol carries the comment `# nav-ok` to pass.
A third `sed -n`, `head` or `tail` slice of one file is denied (two denials at
most); read the file once with `Read` instead.

## How research dispatch differs from implementation

Research subagents may fan out **in parallel** (distinct angles), are mostly **read-only**, do not commit, and are **synthesized in the main model**—never write a conclusion from one subagent. Give each question one owner; the main agent does not repeat their searches. Each handoff provides verified source anchors, conclusions, and open questions. Escalating a wide fan-out to the top-tier model multiplies cost by its width — name the reason or don't escalate. Implementation is sequential per plan step, uses an execution lane, reviews each diff, and commits completed units—see [implementation-mode.md](implementation-mode.md).

Listing-only research (enumerate files/symbols, no reasoning) runs on the cheapest model — on Claude, `model:"haiku"` (see [`.agents/adapters/claude.md`](../../.agents/adapters/claude.md)).

## Choosing research questions

State what remains unanswered. Dispatch one agent per independent question that merits separate read-only work; use none when a targeted read answers it. Do not pad a fan-out to a fixed count, split one question between owners, or duplicate a completed search. Reuse finished research unless it is missing, stale, contradicted, or insufficient. Typical angles: API surface, callers+consumers, event publishers+subscribers, config+env wiring.

## Stopping rule

Stop once the evidence supports the decision or proposed change. Expand only for a concrete dependency, contradiction, or failing case. If a remaining point cannot be resolved with the available methods, state the uncertainty and its impact instead of repeating the same search.
