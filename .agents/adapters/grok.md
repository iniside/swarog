# Grok Adapter

Use this adapter for Grok Build / Grok harness sessions after reading
`.agents/shared/*.md`.

Grok auto-loads both `AGENTS.md` and `CLAUDE.md`. `CLAUDE.md` is the Claude
Code copy of the workflow. Do not treat its model names, hook paths,
trailers, or `C:\Users\lukas\.claude\plans\` path as yours. Shared rules plus
this adapter win on those conflicts.

Do not claim or imply that Grok is Claude, Fable, Opus, or Sonnet. Do not
pass `model:"opus"`, `model:"sonnet"`, `model:"haiku"`, or `model:"fable"`.
The slugs this runtime exposes for `spawn_subagent` are `grok-4.5` and
`grok-4.6`. Do not invent others.

Synonym table (existing Claude-tagged plans map through this table):

| Written tag | Shared lane |
|---|---|
| `[opus]` / `[fable]` | `[independent]` |
| `[sonnet]` | `[mechanical]` |

## Runtime Artifacts

During plan mode, edit only the Grok plan file:
`~/.grok/sessions/<cwd>/<session-id>/plan.md`. Edits to any other file are
rejected while plan mode is active. After approval, before code, copy the
plan into `docs/plans/YYYY-MM-DD-HHMM-<topic>-plan.md`.

Never set `isolation: "worktree"` on `spawn_subagent`. This repo bans git
worktrees. Always `isolation: "none"` (the default).

Never `resume_from` a `core-reviewer` or `proof-auditor`. That is the Grok
form of the banned reviewer reuse. Round 2 is a fresh spawn with its own
diff range.

Project Grok agents live in `.grok/agents/`. Project Grok rules live in
`.grok/rules/`.

## Dispatch Mapping

- `[inline]` — current Grok main agent edits in this context. Only the
  shared dispatch-threshold list, or a compile/typo fix in a file this
  turn's approved step already has this agent editing.
- `[independent]` — `spawn_subagent` with
  `subagent_type: "core-implementer"` (visual/UI: `"mockup-implementer"`),
  `model: "grok-4.6"`, `isolation: "none"`. Visual/UI is never
  `general-purpose`.
- `[mechanical]` — `spawn_subagent` with
  `subagent_type: "general-purpose"`, `model: "grok-4.5"`,
  `isolation: "none"`. Visual/UI is never this lane. Tests are never this
  lane.
- `[test-author]` — `spawn_subagent` with `subagent_type: "test-author"`,
  `model: "grok-4.5"` by default; `"grok-4.6"` only for a novel
  harness/topology.
- `[review]` — `spawn_subagent` with `subagent_type: "core-reviewer"`
  written first, before `description` and before `prompt`;
  `capability_mode: "read-only"`; `model` ≥ the author's tier
  (`grok-4.6` if the author was `grok-4.6`, otherwise `grok-4.5`);
  `isolation: "none"`.
- Research / read-only — `spawn_subagent` with `subagent_type: "explore"`,
  `capability_mode: "read-only"`, `model: "grok-4.5"`. Listing-only uses
  the same type and model.
- Proof audit — `spawn_subagent` with `subagent_type: "proof-auditor"`,
  `capability_mode: "read-only"`, `model` ≥ the author's tier.

Every `spawn_subagent` call must pass explicit `model:`. There is no
intentional inheritance path. Grok has no spawn-time effort field; embed
the requested effort in the prompt. The rust navigation chain does not
inherit; paste it into every code-touching subagent prompt: rust-analyzer
→ targeted read → research subagent → grep as a labelled lower bound.
Comments: default NONE; name at most the one or two things that earn a
line in that file.

## Commits

Use Conventional Commits from shared rules. For `Co-Authored-By`, the
executing model writes its own identity:

```text
Co-Authored-By: Grok 4.6 <noreply@x.ai>
Co-Authored-By: Grok 4.5 <noreply@x.ai>
```

Do not use Claude model trailers for Grok-authored commits. After a
multi-subagent rollout, audit recent trailers against the intended lanes.

## Skills

Use the project skills under `.claude/skills/` — Grok discovers Claude-compat
skill roots. Do not reimplement:

- `safe-verification` — before any `cargo test` / `devctl up` / `verifyctl`
- `architecture-review` — fortress / topology-blind / seam law
- `split-topology-debugger` — split-only wiring
- `add-game-module` — new fortress recipe
- `mockup-implementation` — admin/UI against `UILayout/*.dc.html`
