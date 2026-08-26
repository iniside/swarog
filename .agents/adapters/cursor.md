# Cursor Adapter

Use this adapter for Cursor Agent / Cursor CLI sessions after reading
`.agents/shared/*.md`.

`.cursorignore` keeps `CLAUDE.md` out of this runtime's context. Load
`AGENTS.md` + `.agents/` + `.claude/agents/`. Shared rules plus this adapter
win on model slugs, trailers, and plan path.

Do not claim or imply that Cursor Grok is Claude, Fable, Opus, or Sonnet. Do
not pass `model:"opus"`, `model:"sonnet"`, `model:"haiku"`, or
`model:"fable"`. Do not invent Task `model:` slugs. Use only identifiers the
current session's Task schema actually lists. Preferred families:

- **Composer 2.5, Fast off** — mechanical, explore, listing research, default
  test-author. Pass the schema's Composer id that is not `*-fast` (e.g.
  `composer-2.5`).
- **Grok 4.6, Fast off** — independent, review, proof-audit, mockup, novel
  test harness. Pass the schema's Grok 4.6 id that is not `*-fast` (e.g.
  `cursor-grok-4.6-medium`).

Never pass a `*-fast` slug for those lanes (`composer-2.5-fast`,
`cursor-grok-*-fast`). If the schema exposes only a Fast slug for the needed
family, do not substitute it — stop and say the schema gap. `inherit` is
banned except on the closed `[inline]` threshold list.

Project custom agents are loaded from `.claude/agents/` (Cursor's Claude
compat). Do not duplicate them into `.cursor/agents/`. Do not use Cursor
built-in `code-reviewer`, `security-review`, `bugbot`, or `codex-rescue`
unless the user explicitly asked for that product.

Synonym table (existing Claude-tagged plans map through this table):

| Written tag | Shared lane |
|---|---|
| `[opus]` / `[fable]` | `[independent]` |
| `[sonnet]` | `[mechanical]` |

## Runtime Artifacts

During plan mode, edit only the Cursor plan-mode harness artifact this
session already opened. Edits to `docs/plans/` or other repo files while
plan mode is active are rejected. After approval, before code, copy the
plan into `docs/plans/YYYY-MM-DD-HHMM-<topic>-plan.md`.

Never use `best-of-n-runner` or any Task isolation that creates a git
worktree. This repo bans worktrees.

Never `resume` a `core-reviewer` or `proof-auditor`. That is the Cursor form
of the banned reviewer reuse. Round 2 is a fresh Task with its own diff
range.

## Dispatch Mapping

Write `subagent_type` first, before `description` and before `prompt`, when
dispatching `core-reviewer`.

Every Task call passes explicit `model:` from the current schema (see
preferred families above). Effort does not inherit; embed it in the prompt.
The rust navigation chain does not inherit; paste it into every
code-touching subagent prompt: rust-analyzer → targeted read → research
subagent → grep as a labelled lower bound. Comments: default NONE; name at
most the one or two things that earn a line in that file.

- `[inline]` — current Cursor main agent edits in this context. Only the
  shared dispatch-threshold list, or a compile/typo fix inside a file this
  turn's approved step already has this agent editing.
- `[independent]` — `Task` `subagent_type: "core-implementer"` (visual/UI:
  `"mockup-implementer"`), `model:` Grok 4.6 non-fast from the schema (e.g.
  `"cursor-grok-4.6-medium"`).
- `[mechanical]` — `Task` `subagent_type: "generalPurpose"`, `model:`
  Composer 2.5 non-fast from the schema (e.g. `"composer-2.5"`). Not
  `core-implementer`. Visual/UI design is never this lane. Tests are never
  this lane.
- `[test-author]` — `Task` `subagent_type: "test-author"`, `model:` Composer
  2.5 non-fast by default; Grok 4.6 non-fast only for a novel
  harness/topology.
- `[review]` — `Task` `subagent_type: "core-reviewer"`, `model:` Grok 4.6
  non-fast (same family as `[independent]`, not Composer even when the
  author was mechanical). Never `code-reviewer`. Never `security-review`
  unless the user asked for that Cursor product. Do not invent a Task
  `readonly` field when the schema lacks one — `core-reviewer` must not
  edit; say so in the prompt.
- Research / read-only — `Task` `subagent_type: "explore"`, `model:`
  Composer 2.5 non-fast. Listing-only uses the same type and model. Prompt:
  read-only, no edits.
- Proof audit — `Task` `subagent_type: "proof-auditor"`, `model:` Grok 4.6
  non-fast. Prompt: read-only, no edits.

Do not dispatch `bugbot` unless the user explicitly asked for a Bugbot-like
review. Do not dispatch `codex-rescue` unless the user asked for Codex.

## Commits

Use Conventional Commits from shared rules. For `Co-Authored-By`, the
executing model writes its own identity:

```text
Co-Authored-By: Cursor Grok 4.6 <noreply@x.ai>
Co-Authored-By: Composer 2.5 <noreply@cursor.com>
```

Do not use Claude model trailers for Cursor-authored commits. After a
multi-subagent rollout, audit recent trailers against the intended lanes.

## Skills

Use the project skills under `.claude/skills/` (Cursor's Claude-compat skill
roots). Do not reimplement:

- `safe-verification` — before any `cargo test` / `devctl up` / `verifyctl`
- `architecture-review` — fortress / topology-blind / seam law
- `split-topology-debugger` — split-only wiring
- `add-game-module` — new fortress recipe
- `mockup-implementation` — admin/UI against `UILayout/*.dc.html`
