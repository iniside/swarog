# Claude Adapter

Use this adapter for Claude Code / Claude harness sessions after reading
`.agents/shared/*.md`.

This runtime is Claude. Keep `[fable]`. Do not invent a model slug this
harness does not expose. Every `Task` / `Agent` call passes explicit
`model:` — there is no inheritance path except the closed `[inline]`
threshold list.

Synonym table (Claude plans may write the left-hand tags; shared plans write
the right-hand tags; this adapter accepts both):

| Written tag | Shared lane |
|---|---|
| `[opus]` / `[fable]` | `[independent]` |
| `[sonnet]` | `[mechanical]` |

## Runtime Artifacts

During plan mode, edit only `C:\Users\lukas\.claude\plans\<slug>.md`. After
user approval, copy the plan into
`docs/plans/YYYY-MM-DD-HHMM-<topic>-plan.md`.

Project skills live under `.claude/skills/`. Use them; do not reimplement.
Never use a git worktree.

## Dispatch Mapping

- `[inline]` — current Claude main model writes in the active context. Only
  the shared dispatch-threshold list, or a compile/typo fix in a file this
  turn's approved step already has this agent editing.
- `[independent]` / `[opus]` / `[fable]` — `subagent_type: "core-implementer"`
  (visual/UI: `"mockup-implementer"`) with `model:"opus"` or `"fable"`. New
  API, bus/registry/edge/lifecycle, cross-module behaviour, security
  boundaries.
- `[mechanical]` / `[sonnet]` — `model:"sonnet"`. Renames, scaffolding,
  N-similar edits, applying a fully specified step, compile fixes,
  JSON/config. Visual/UI is never this lane. Tests are never this lane.
- `[test-author]` — `subagent_type: "test-author"`, `model:"sonnet"` default;
  `"opus"` / `"fable"` only for a novel harness/topology. The only lane that
  writes tests.
- `[review]` — `subagent_type: "core-reviewer"` written first, before
  `description` and before the prompt; `model:` ≥ the author's tier.
  Never resume a `core-reviewer` or `proof-auditor`. Round 2 is a fresh
  spawn with its own diff range.
- listing-only research — `model:"haiku"` unless intentionally overridden.
- read-only research — Explore / general-purpose, `model:"sonnet"` unless
  listing-only.
- Proof audit — `subagent_type: "proof-auditor"`, `model:` ≥ the author's
  tier. Only when the diff touches a verify stage or the test/gate is the
  risk surface.

Effort does not inherit; embed it in the subagent prompt. The rust
navigation chain does not inherit; paste it into every code-touching
subagent prompt: rust-analyzer → targeted read → research subagent → grep
as a labelled lower bound. Comments: default NONE; name at most the one or
two things that earn a line in that file.

## Commit Trailers

Use the executing model. A tag selects a tier; the subagent writes whatever
that tier currently is.

```text
Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>
Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
Co-Authored-By: Claude Sonnet 4.6 <noreply@anthropic.com>
```

After multi-subagent rollouts, audit recent commit trailers against the
intended lanes before reporting done.

## Skills

Use the project skills under `.claude/skills/`:

- `safe-verification` — before any `cargo test` / `devctl up` / `verifyctl`
- `architecture-review` — fortress / topology-blind / seam law
- `split-topology-debugger` — split-only wiring
- `add-game-module` — new fortress recipe
- `mockup-implementation` — admin/UI against `UILayout/*.dc.html`
