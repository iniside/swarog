# Codex Adapter

Use this adapter for Codex sessions after reading `.agents/shared/*.md`.

This runtime is Codex. Do not claim or imply that Codex is Claude, Fable,
Opus, or Sonnet. Preserve the shared dispatch intent (explicit model/tool
choice; cheap lane for mechanical work; independent review when available)
without inventing unavailable models or Claude slugs.

Use Codex tools by their actual names. Discover tools with this session's
tool-search rather than copying another runtime's schema. If a multi-agent
tool is needed, use only tools that are actually available.

Synonym table (existing Claude-tagged plans map through this table):

| Written tag | Shared lane |
|---|---|
| `[opus]` / `[fable]` | `[independent]` |
| `[sonnet]` | `[mechanical]` |

## Runtime Artifacts

Codex does not use Claude plan-mode temp files. If a plan must be written
before approval, keep it in the Codex/harness planning artifact when
available. Once approved, copy it to
`docs/plans/YYYY-MM-DD-HHMM-<topic>-plan.md` as required by shared rules.

Never use a git worktree. Never reuse a reviewer conversation for round 2.

## Dispatch Mapping

- `[inline]` — the current Codex main agent edits in this context. Only the
  shared dispatch-threshold list, or a compile/typo fix in a file this
  turn's approved step already has this agent editing.
- `[independent]` — a separate Codex/multi-agent context only if an actual
  multi-agent tool exists in this session; otherwise do the work inline and
  disclose that it was not an independent context. Visual/UI uses
  `mockup-implementer` when that persona is available; never the mechanical
  lane.
- `[mechanical]` — use the cheapest available implementation tool/model only
  if the environment exposes one; otherwise do it inline and say why.
  Visual/UI is never this lane. Tests are never this lane.
- `[test-author]` — tests only, after the implementation they cover. Use the
  cheapest available writer; escalate only for a novel harness/topology
  (new splitproof assertion, event-plane fixture).
- `[review]` — use an available `core-reviewer` (or equivalent read-only
  reviewer) if one exists; otherwise perform an explicit main-agent review
  and disclose that it was not an independent review. Do not reuse a
  reviewer conversation for round 2.
- Research / listing-only — cheapest available read-only tool; disclose if
  it is the main agent.

Do not hardcode `model:"fable"`, `model:"opus"`, or `model:"sonnet"` in
Codex instructions unless those exact models are exposed by a tool in the
current session. If a tool has its own model field, set it deliberately and
name the actual model/tool used. Effort does not inherit; embed it in the
prompt when a subagent exists. Comments: default NONE.

The rust navigation chain does not inherit; paste it into every
code-touching prompt: rust-analyzer → targeted read → research subagent →
grep as a labelled lower bound.

## Commits

Use Conventional Commits from shared rules. For `Co-Authored-By`, use a
trailer that truthfully identifies the executing agent/model. Do not use
Claude or Grok trailers for Codex-authored commits unless that model
actually executed.

## Skills

Use project skills under `.claude/skills/` when this runtime can load them
(`safe-verification`, `architecture-review`, `split-topology-debugger`,
`add-game-module`, `mockup-implementation`). Do not reimplement. Follow
`safe-verification` before any `cargo test` / `devctl up` / `verifyctl`.
