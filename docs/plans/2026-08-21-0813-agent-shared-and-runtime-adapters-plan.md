# Agent shared rules + runtime adapters

Status: approved. Owner revision after v2: **do not thin `CLAUDE.md`
into a dual-runtime index**; **do not rename `core-reviewer`**. Hide
`CLAUDE.md` from Cursor with `.cursorignore` (ArcGame). Persona
`core-reviewer` is upgraded in place to the prosecutor contract (same
`name:`).

## Goal

1. **Shared agent rules** — one portable workflow body under `.agents/shared/`;
   generic lanes; harden `core-reviewer` in place (same name).
2. **Runtime adapters** — Grok / Cursor / Codex / Claude translate lanes.
   `CLAUDE.md` stays the **Claude-only** document.

No production Rust. No new domain module.

## Owner decisions (this revision — not reopenable by research)

1. **`CLAUDE.md` remains Claude-specific.** Do not replace it with a thin
   “non-Claude runtimes ignore this file” index. Claude Code keeps opus /
   sonnet / fable, trailers, plan path, `core-reviewer`, PreToolUse.
2. **Keep the reviewer *name* `core-reviewer`.** Do not rename the file,
   frontmatter `name:`, PreToolUse type, or dispatch tag to
   `hostile-reviewer`. **Do upgrade the persona body** to ArcGame’s
   prosecutor contract (see Step 3). Same type, meaner agent.
3. **Cursor: `.cursorignore` lists `CLAUDE.md`** — copy ArcGame
   `G:\ArcGame\.cursorignore` lines 7–11 (comment + `CLAUDE.md` +
   `**/CLAUDE.md`). Claude Code does not read that file.

## How each runtime then avoids Claude slugs

| Runtime | Mechanism | Evidence |
|---|---|---|
| **Cursor** | `.cursorignore` skips `CLAUDE.md`. Loads `AGENTS.md` + `.agents/` + `.claude/agents/` (Claude-compat personae). | ArcGame `.cursorignore`; Cursor adapter already says do not duplicate into `.cursor/agents/`. |
| **Grok** | **Cannot** skip a committed top-level `CLAUDE.md`. `compat.claude.agents = false` does **not** gate generic `CLAUDE.md` (`docs` in `~/.grok/docs/user-guide/05-configuration.md`: “generic top-level `CLAUDE.md` stay recognized”). Gitignore would untrack a file `docs-current` requires. | Adapter + `.grok/rules/00-runtime.md`: shared + grok adapter win on slugs / trailers / plan path. Same as ArcGame grok adapter (“Grok auto-loads both”). |
| **Codex** | Typical load is `AGENTS.md`, not `CLAUDE.md`. | Thin `AGENTS.md` + codex adapter. |
| **Claude** | Loads `CLAUDE.md` as today. | Unchanged role; Claude adapter holds the slug table. |

## Context — overlap

| Candidate | Why not |
|---|---|
| Thin both roots into dual indexes (v2) | Owner: `CLAUDE.md` stays Claude-specific; Cursor ignore replaces the “Grok ignore this paragraph” rewrite. |
| Rename `core-reviewer` → `hostile-reviewer` (v2 Step 2) | Owner: keep the name. Hook and personae stay `core-reviewer`. |
| Sync fat `AGENTS.md` up to `CLAUDE.md` without shared | Re-drifts the next Claude-only edit. |
| Gitignore `CLAUDE.md` so Grok skips it | `docs-current` requires the file on disk; Claude Code needs it committed. |
| Dual `core-reviewer.md` + `hostile-reviewer.md` | Banned leftover rail. |

**Extract source:** `CLAUDE.md` (richer). `AGENTS.md` is the stale twin and
becomes the thin non-Claude entry.

## Lane vocabulary

Shared tags (in `.agents/shared/planning-dispatch.md` and in `AGENTS.md`):

| Tag | Meaning |
|---|---|
| `[inline]` | Main agent. Closed dispatch-threshold list only, or a typo fix in a file this turn already has open. |
| `[independent]` | Top-tier separate context (`core-implementer` / `mockup-implementer`). |
| `[mechanical]` | Cheap implementation. Visual/UI never this. Tests never this. |
| `[test-author]` | Only lane that writes tests; always a later step. |
| `[review]` | `core-reviewer` (read-only). |

**New non-Claude plans write the shared tags.** Claude plans **may** write
`[opus]` / `[fable]` / `[sonnet]`. **Every** adapter has the synonym table:
`[opus]` / `[fable]` → `[independent]`; `[sonnet]` → `[mechanical]`.
Do not ban `[fable]`.

## Do not copy from ArcGame

Unreal / Lore / Mass / C++ nav / tests-opt-in / commit-when-asked / ban
`[fable]` / Claude-trailer git hook / `.cursor/agents/` / Unreal MCP.

## Must keep (GB)

`mockup-implementer`, `safe-verification`, one-rollout, `split-topology-debugger`,
`add-game-module`, `architecture-review`, Commit After Every Task, mandatory
separate `[test-author]`, Fix the Authority, PreToolUse guard (**name stays
`core-reviewer`**), root `AGENTS.md` + `CLAUDE.md` present for docs-current,
wipe / fortress / topology-blind.

## Semantic changes (name in the commit)

1. Research-ask: only when the method changes the answer.
2. `core-reviewer` **persona** hardens (same `name:`): default REJECT; binary
   verdict; always-on old-rail class. Dispatch type unchanged.
3. Shared lanes in `AGENTS.md` + reference docs; Claude keeps writing opus
   tags; adapters translate.
4. Trailer = executing runtime (Claude adapter keeps Anthropic strings).
5. Memory live path = `scripts/memory-sync` Claude dir + `CLAUDE_MEMORY_DIR`.
   Drop the AGENTS.md Codex-path claim the script does not implement.
6. Fortress count **12 including wallet** in shared architecture + `README.md:87`.
   `CLAUDE.md` Layout line that still says 11 is patched in the Claude file
   (Claude-specific doc, still the truth for Claude readers).

## Dispatch threshold

Closed list → `[inline]`, no subagent, no review: comments; log/format/UI
strings; include add/reorder; literal/typo; rename inside one file.

## Tests

No test step — instruction files only. Do not run `verifyctl --fast`.
`docs-current` is presence + fragment-stripped links (`docs_current.rs`);
no `--stage` flag. Proof = Step 6 inventory.

---

## Step 1 — Extract `.agents/shared/` from `CLAUDE.md`

- **(a)** Create:
  - `.agents/README.md` — shared vs adapter; `CLAUDE.md` is the Claude copy
    and stays in lockstep on workflow substance; adapters only translate
    tools/models/trailers/plan paths.
  - `.agents/shared/core-rules.md` — Owning Mistakes (**RULE**); Plans &
    Status Docs (**RULE**); Git Safety (**MANDATORY**); Commit After Every
    Task (**MANDATORY**); Commit Message Format (**RULE**, trailers in
    adapters); Comments (**MANDATORY**, `comments: default NONE` on every
    implementation dispatch); no dual-write / no topology branch / wipe
    (**MANDATORY**); memory-sync Claude path only (**RULE**).
  - `.agents/shared/research-navigation.md` — Research before planning
    (**MANDATORY**); Research / Search Mode (**RULE**, ask only when the
    method changes the answer); Research Never Overrules a Decision
    (**MANDATORY**); nav = rust-analyzer / targeted read / research
    subagent; grep labelled lower bound.
  - `.agents/shared/planning-dispatch.md` — Plan Writing (**MANDATORY**,
    shared lanes + mandatory `[test-author]` + synonym table); Implementation
    Mode (**MANDATORY**, threshold, parallel review when files don’t overlap);
    Adversarial Diff Review (**MANDATORY**, prosecutor method, agent name
    **`core-reviewer`**, no resume, 2-round cap); Fix the Authority
    (**MANDATORY**, six rules); Refactor Safety.
  - `.agents/shared/gamebackend.md` — architecture half: three seams, hard
    constraints 1–10, add-module recipe, **12 fortresses + gateway including
    wallet**, Commands including weles, one-rollout, Database wipe, Layout
    (12, not 11).
- **(b)** Adapters and thin `AGENTS.md` have nothing to point at until this
  exists. **Do not edit `CLAUDE.md` in this step** except the Layout 11→12
  patch and the fortress heading already saying 12 (semantic change #6) —
  that is Claude-specific truth, not a dual-runtime rewrite.
- **(c)** Cut-and-place from `CLAUDE.md`. No model slugs in shared. Review
  agent in shared text is `core-reviewer`.
- **(d)** `[independent]`

Commit: `docs(agents): extract shared instruction body from CLAUDE.md`

---

## Step 2 — Thin `AGENTS.md` only; add `.cursorignore`

- **(a)** Replace `AGENTS.md` with an ArcGame-shaped index: purpose;
  MANDATORY index pointing into `.agents/shared/`; read order (shared →
  adapter for the **active** runtime); non-negotiables (git safety,
  one-rollout, `isolation: "none"`, no invented slugs, fortress/wipe).
  Does not name a vendor. Architecture facts live in `gamebackend.md`.

  Create `.cursorignore` (new file) with the ArcGame comment and:

  ```
  CLAUDE.md
  **/CLAUDE.md
  ```

  **Do not thin `CLAUDE.md`.** Optional one-line at the top of `CLAUDE.md`
  (Claude readers only): workflow substance also lives in `.agents/shared/`;
  this file is the Claude overlay (models, trailers, plan path, hook names).
  No “Grok/Cursor ignore this file” paragraph.

- **(b)** Shared must exist (Step 1). Cursor ignore must land before we
  claim Cursor won’t eat opus slugs.
- **(c)** `docs-current` still sees both root files. `.cursorignore` is not
  scanned by docs-current.
- **(d)** `[mechanical]`

Commit: `docs(agents): thin AGENTS.md and ignore CLAUDE.md in Cursor`

---

## Step 3 — Adapters + Grok tree + upgrade `core-reviewer` persona (keep the name)

- **(a)** One commit:

  **`.agents/adapters/claude.md`** — plan
  `C:\Users\lukas\.claude\plans\<slug>.md`; synonym table; `[independent]` →
  `core-implementer` / `mockup-implementer` + `model:"opus"` or `"fable"`;
  `[mechanical]` → `"sonnet"`; `[review]` → **`core-reviewer`** ≥ author;
  `[test-author]` default sonnet; listing-only haiku; Anthropic trailers;
  explicit `model:`.

  **`.agents/adapters/grok.md`** — Grok loads both roots; **shared + this
  adapter win** on slugs/trailers/plan path (cannot unread `CLAUDE.md`);
  only `grok-4.5`/`grok-4.6`; synonym table required; plan
  `~/.grok/sessions/<cwd>/<session-id>/plan.md`; `isolation: "none"`;
  never `resume_from` `core-reviewer` / `proof-auditor`; `[independent]` →
  `core-implementer` (or `mockup-implementer` for UI) + `grok-4.6`;
  `[mechanical]` → `general-purpose` + `grok-4.5`; `[review]` →
  **`core-reviewer`** + `capability_mode: "read-only"` + model ≥ author;
  research → `explore` + `grok-4.5` + read-only; `subagent_type` first on
  review; effort in prompt; trailers `Grok 4.6` / `Grok 4.5 <noreply@x.ai>`;
  use `.claude/skills/` (`safe-verification`, `architecture-review`,
  `split-topology-debugger`, `add-game-module`, `mockup-implementation`).

  **`.agents/adapters/cursor.md`** — synonym table; Composer 2.5 non-fast =
  mechanical / explore / default test-author; Grok 4.6 non-fast =
  independent / review / proof / mockup / novel harness; never `*-fast`
  (schema-only-Fast → stop); never `inherit` except threshold `[inline]`;
  personae from `.claude/agents/`; **no** `.cursor/agents/`; never built-in
  `code-reviewer` / `security-review` / `bugbot` / `codex-rescue` unless
  asked; no Task `readonly` field — “do not edit” in the prompt; never
  `best-of-n-runner` / worktree; never resume reviewer; trailers
  `Cursor Grok 4.6` / `Composer 2.5`. Relies on `.cursorignore` so
  `CLAUDE.md` is not in context.

  **`.agents/adapters/codex.md`** — synonym table; real tool names; no
  invented Claude slugs; `[independent]` only if a multi-agent tool exists,
  else inline and disclose; `[review]` = available reviewer or explicit
  main-agent review + disclosure; no conversation reuse for round 2;
  truthful trailer.

  **`.grok/rules/00-runtime.md`** — this session is Grok; `AGENTS.md` →
  shared → grok adapter; do not dispatch `model:"opus"` etc. even though
  `CLAUDE.md` is in context; never worktree; never `resume_from`
  `core-reviewer` / `proof-auditor`.

  **`.grok/agents/`** — `core-implementer.md`, **`core-reviewer.md`**,
  `proof-auditor.md`, `test-author.md`, `mockup-implementer.md`.

  Frontmatter:

  ```yaml
  ---
  name: <agent-name>
  description: <when; NOT for>
  prompt_mode: full
  permission_mode: default   # implementers, test-author, mockup-implementer
  # permission_mode: plan    # core-reviewer, proof-auditor
  agents_md: true
  ---
  ```

  Point at shared + `docs/reference/core-failure-taxonomy.md`; never spawn
  further subagents; never worktree; `comments: default NONE`; trailer from
  grok adapter; at-risk topology = **split**; `safe-verification` before
  cargo/devctl/verifyctl.

  **`core-reviewer` persona upgrade (same commit, same name):**

  Keep `.claude/agents/core-reviewer.md` path and YAML `name: core-reviewer`.
  Rewrite **description + body** to ArcGame `hostile-reviewer` contract,
  GB-substituted (split topology, wipe/no-compat — not Mass/`Engine/`):

  - description: one adversarial class-keyed pass; prosecutor; after every
    task/commit; read-only; binary verdict; complements proof-auditor.
  - default **REJECT**; guilty until proven; inline self-review banned.
  - read `git diff` / `git show`, never the author’s summary; line-by-line
    against the plan step; hunt, don’t confirm.
  - findings: `class` · `file:line` · failing scenario · what it should be.
  - banned: “looks fine”, “mostly matches”, “pass with reservations”.
  - clean PASS is valid **only** with the taxonomy class list attacked.
  - one pass; never resume / round-2 reuses the same conversation.
  - 2-round cap per task.
  - always-on class **old rail still alive** (run before other classes):
    what did this introduce; what should be dead; is the dead chain still
    in the tree; two runtime ways to do the same thing; delete stopped at
    the entry point. On a **plan**: step (a) names what DIES; razing before
    or in the same step as the replacement.
  - compose: seam law → `architecture-review`; proof honesty →
    `proof-auditor`. Do not re-derive those here.

  `.grok/agents/core-reviewer.md` gets the same body with Grok frontmatter
  (`prompt_mode: full`, `permission_mode: plan`). PreToolUse stays
  `core-reviewer`. No `hostile-reviewer` file, no alias.

- **(b)** `AGENTS.md` already points at adapters. Grok personae must exist
  in the same commit as the grok adapter that names them. Persona upgrade
  in this commit means the first session that uses the new adapters already
  gets the mean reviewer — no window with adapters + old mild body.
- **(c)** Visual/UI → `mockup-implementer` + independent model, never
  `general-purpose`. Keep `[fable]` in the Claude adapter.
- **(d)** `[independent]`

Commit: `docs(agents): add adapters, Grok personae, and hostile core-reviewer body`

---

## Step 4 — Old-rail taxonomy class

Persona text in Step 3 already attacks the class; the catalog must name it
or the next review has no authority to route to.

- **(a)** `docs/reference/core-failure-taxonomy.md` — add class **old-rail /
  dual-path / migration-shim** (what / attack / authority) and a checklist
  bullet in the cross-cutting review section. Provenance sentences keep
  the name `core-reviewer`.
- **(b)** After Step 3 the persona already runs this class; without this
  row the “read the taxonomy, don’t restate it” contract has a hole.
- **(c)** Do not rewrite other classes. Do not mention `hostile-reviewer`.
- **(d)** `[mechanical]`

Commit: `docs(taxonomy): add old-rail dual-path class`

---

## Step 5 — Point current AGENTS.md citations at shared; patch README count

`CLAUDE.md` headings stay (file not thinned), so Claude-heading citations
in `event-plane-ops.md` / `module-reference.md` / taxonomy admin section
**remain valid**. This step only fixes files that pointed at **fat
`AGENTS.md` working-agreements** or the stale 11-fortress README line.

Closed list:

| File | Action |
|---|---|
| `docs/reference/plan-writing-workflow.md` | “rule in AGENTS.md” → `.agents/shared/planning-dispatch.md`; lane table = shared tags + synonym table |
| `docs/reference/implementation-mode.md` | same |
| `docs/reference/subagent-dispatch.md` | same; explicit model → adapters |
| `docs/reference/research-mode.md` | link → `research-navigation.md`; ask-when-method-changes |
| `docs/reference/commit-format.md` | link → `core-rules.md`; trailers = adapters |
| `docs/reference/architecture-enforcement.md` | “source of truth AGENTS.md” → `gamebackend.md` |
| `docs/README.md` | AGENTS vs CLAUDE blurb → AGENTS = non-Claude entry + shared; CLAUDE = Claude overlay; Cursor ignores CLAUDE.md |
| `README.md` (~line 87) | 11 fortresses → 12 + gateway including wallet |
| `docs/reference/decisions-are-final.md` | **create** — cite `memory/gamebackend-north-star-and-jvm-exploration.md` and `memory/shared-postgres-is-the-model.md`; do not invent incidents |

Do not rewrite `docs/plans/*` / `docs/status/*`. Do not rewrite taxonomy
classes beyond Step 4’s new class.

- **(b)** After Step 2, fat AGENTS headings are gone; these five workflow
  docs would lie. CLAUDE.md citations elsewhere stay true.
- **(c)** `[mechanical]`

Commit: `docs(reference): point AGENTS workflow docs at shared body`

---

## Step 6 — Closed inventory (not verifyctl)

Ticks, recorded in the commit message:

1. `AGENTS.md` and `CLAUDE.md` exist at repo root.
2. `.cursorignore` contains `CLAUDE.md` and `**/CLAUDE.md`.
3. Every new Markdown link to `.agents/` or `.grok/` from Steps 1–5
   resolves.
4. Zero remaining fat-heading citations in the **Step 5 table** that still
   treat `AGENTS.md` as if Working agreements lived there.
5. `README.md` fortress count is 12 including wallet.
6. `rg core-reviewer` still finds the agent; `rg hostile-reviewer` in
   `.claude/` `.agents/` `.grok/` is empty (we did not rename).
7. PreToolUse prompt still lists `core-reviewer` (unchanged).

If a tick fails, fix in this step on files already in scope.

- **(d)** `[inline]`

Commit: `docs(agents): inventory shared/adapter links`

---

## Execution notes

Forced order: **1 extract → 2 thin AGENTS + cursorignore → 3 adapters +
Grok tree + hostile `core-reviewer` body → 4 taxonomy old-rail class →
5 AGENTS-citation docs → 6 inventory**.

Do not rename the reviewer. Do not thin `CLAUDE.md`. Do not
`memory-sync push` after repo-first memory edits. If memory still
describes a mild reviewer after Step 3, update **live then push**.

Reviews of this rollout keep dispatching `core-reviewer`. After Step 3
the persona is the prosecutor contract, same type.

## Out of scope

- ArcGame Python hooks.
- Rewriting historical plans’ lane tags.
- Making Grok skip `CLAUDE.md` via gitignore or `compat.claude.agents`
  (neither works for a committed top-level file).
- Changing `ROOT_DOCUMENTS` / adding `--stage`.
- `verifyctl --fast`.
)
