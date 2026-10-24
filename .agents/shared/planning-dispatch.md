# Planning And Dispatch

## Plan Writing Workflow - MANDATORY

Front-load the thinking. For any plan (plan mode / "write me a plan" / a
`docs/plans/…-plan.md`), run these steps in order — no skipping for "it's
small":

0. Preserve the owner's shape. The Context opens with the requested outcome
   and explicit exclusions in the owner's words. Every unrequested module,
   topic, capability, cache, bridge, or contract names the requirement that
   cannot be met without it, or becomes one blocking question. Extract/move
   work relocates the existing code; it never builds a parallel twin.
1. Map the overlap, then pick the open questions. Any plan proposing a new
   module, service, event, or admin section (or a replacement) documents in
   its Context, per overlapping candidate: what it does, how it differs, and
   an explicit "why not extend / depend on X". Without that the plan is
   incomplete. Before approval, close only questions that change scope,
   architecture, or feasibility; record the rest for the implementer. Fan-out
   follows `docs/reference/research-mode.md` (one owner per question, no
   fixed count). Pass an explicit model/tool choice from the active adapter.
2. Research three non-overlapping angles: API surface, API usages, and
   patterns. Synthesize in the main agent — never write off one subagent.
3. Write concrete specifics: exact files, exact signatures, exact API calls,
   sequencing. Banned phrases ("figure out as we go", "TBD", "investigate
   during implementation", "may need to", "something like") mean a research
   gap — go back to step 2.
4. Structure as an ordered `Step 1 → Step 2 → …` sequence, not a catalog. Each
   step states **(a)** what is touched (exact files/symbols), **(b)** why now /
   order — the dependency forcing it before the next, **(c)** how —
   non-mechanical moves spelled out, **(d)** dispatch tag. A catalog that
   leaves order/topology/per-step actions to "figure as you go" is banned;
   steps need not each compile, but every step must be written out.

   **Tests are their own step(s), never bundled into an implementation step —
   MANDATORY.** If a change touches testable production code, the plan MUST
   carry a separate, later `[test-author]` step for its tests, sequenced
   *after* the implementation step it covers has landed and compiled. Banned:
   any step of the shape "implement X **and** write its tests". The test step
   names: which landed commit/behaviour it covers, the previously-wrong
   branch each test must exercise, and the at-risk topology (split, not just
   monolith). A plan that touches testable production code with **no** test
   step is incomplete unless the user explicitly waived tests.
5. Dispatch `subagent_type: "core-reviewer"` — that field is written first,
   before `description` and before the prompt. Never a generic implementer
   or explore/plan type. Do not hand-author a "grumpy senior engineer"
   persona. Effort is the Agent `effort:` parameter; its value follows
   `.agents/shared/core-rules.md` **Agent Effort**. It checks scope substitution
   first, rejects only with evidence, and lists unverified concerns
   separately. It returns a punch list, never a rewrite. Address it before
   showing the user, or note deferrals with rationale.

Full detail: `docs/reference/plan-writing-workflow.md`.

## Implementation Mode - MANDATORY

Dispatch is decided per plan step, not per session (set at Plan Writing 4d).
Tags describe capability and cost; runtime adapters map them to concrete
tools/models.

Shared tags:

| Tag | Meaning |
|---|---|
| `[inline]` | Main agent. Closed dispatch-threshold list only, or a typo/compile fix in a file this turn already has open. |
| `[independent]` | Top-tier separate context (`core-implementer`; visual/UI uses `mockup-implementer`). |
| `[mechanical]` | Cheap implementation. Visual/UI never this. Tests never this. |
| `[test-author]` | Only lane that writes tests; always a later step. |
| `[docs]` | `docs-writer`. Documentation prose in a planned docs step; a standalone comment edit stays `[inline]`. Runs last, against landed code. |
| `[review]` | `core-reviewer` (read-only). |

Synonym table (adapters translate; new non-Claude plans write the shared
tags; Claude plans may write the left-hand tags):

| Written tag | Shared lane |
|---|---|
| `[opus]` / `[fable]` | `[independent]` |
| `[sonnet]` | `[mechanical]` |

Do not ban `[fable]`.

- `[inline]` is the exception, not the default. Two cases only: the closed
  below-threshold list, and a compile/typo fix inside a file this turn's
  approved step already has the main agent editing. Never `[inline]`, at any
  line count: a new function / type / signature change, a control-flow or
  condition change, anything spanning more than one file, anything a plan
  step lists under (a). "It's only a few lines" is not an argument. When
  unsure which lane, it is not `[inline]`.
- `[independent]` — top-tier / high-capability subagent in a separate
  context. New API design, bus/registry seams, lifecycle ordering,
  cross-module behaviour, security boundaries. For `core/*` internals or
  cross-seam work, the vehicle is `core-implementer`. Separate context is
  the independent-reviewer boundary.
- `[mechanical]` — cheaper implementation lane: renames, scaffolding,
  N-similar edits, applying a fully specified step, compile fixes,
  JSON/config. Never spend a higher tier on a rename. Visual/UI design is
  never `[mechanical]`. Tests are never `[mechanical]`. Documentation prose is
  never `[mechanical]`; it goes through `[docs]`.
- `[test-author]` — the only lane that writes tests, always a separate step
  after the implementation it covers. Cheap/mechanical default; escalate
  only for a novel harness/topology (new splitproof assertion, event-plane
  fixture).
- `[docs]` — the `docs-writer` agent (`.claude/agents/docs-writer.md`). The
  lane for documentation prose written as a planned docs step — root guidance,
  `docs/reference`, `docs/roadmap`, `docs/todo`, plan errata, and Rust doc-comments. A
  standalone comment edit stays `[inline]`. It runs last, against landed code, and deletes false prose as often as it adds
  true prose. Cheap default; escalate only when the doc must judge what a
  seam *means*. A plan's docs step names the exact files — an unnamed file is
  never found. Comment-only review findings are not a `[docs]` dispatch: the
  orchestrator fixes them inline (`core-rules.md` **Comments**).
- `[review]` — reviewer lane; critique only, no rewrites.

The lane is chosen for the whole plan step or named fix; never divide a
feature to fit the inline threshold.

**Dispatch threshold — below it, `[inline]`, and a subagent is banned.** No
subagent call, no plan tag, no approval, no review round for exactly these:
comment edits of any size, log/format/UI strings, include add/reorder, a
literal or typo fix, a rename inside one file, the lead's `docs/todo/` edits
outside a dispatched step (adding owner-deferred leftovers, and moving an item
to `## In progress` before dispatching the step that works on it; the owner
already decided each item, so there is no review round). That list is closed —
it is not a size rule. A change that is not on it goes to a subagent lane however
small it looks.

Every code-writing subagent call carries an explicit model/tool decision from
the active adapter. Do not rely on inheritance. After a multi-subagent
rollout, audit commit trailers against each step's lane.

Tags are approved with the plan. Ask only for untagged/ad-hoc work; an
authorized lane is not reopened. Effort is the Agent `effort:` parameter,
its value per `core-rules.md` **Agent Effort**; prompt wording does not set it. Commit after each task
(subagents may commit their own). Mid-rollout, never flip a step's lane tag silently — ask the owner first.

**Review does not gate the next task — run them in parallel when the tasks
are independent.** Every task still gets its `core-reviewer` pass, but that
pass only blocks the next dispatch when the next step actually depends on
it. Serialize only if the two steps touch any of the same files, or the later
step builds on an API/behaviour the earlier one introduces (plan step (b)
already names that dependency). Otherwise dispatch the reviewer and the next
implementation concurrently, in one message — the reviewer is read-only, so
the sole hazard is two writers in one file.

When a parallel review returns a punch list: fix it in its own commit carrying `Kind: review-fix`. If a
finding touches a file the in-flight task is editing, wait for that task to
land, then fix — never edit a file a live subagent holds.

Details: `docs/reference/implementation-mode.md` and
`docs/reference/subagent-dispatch.md`.

## Adversarial Diff Review - MANDATORY

Every produced diff — a subagent's or the main agent's — is reviewed in
prosecutor mode: guilty until proven correct, default verdict REJECT. The
vibes skim ("roughly matches the plan → pass") is banned.

The review runs in a subagent, never inline. Dispatch `core-reviewer` after
every task commit, on that commit plus the cumulative diff from the
task-start commit to `HEAD`. Prose-only commits (`.agents/`, `docs/`, plans)
are reviewed the same way. Inline self-review is the banned failure mode —
the context that wrote the code cannot judge it. Route by files-touched
through `docs/reference/core-failure-taxonomy.md`, with a different method
than the implementer used. Add `proof-auditor` ONLY
when the diff touches a verify stage (verifyctl / archcheck / conformance /
topiccheck / golden) or the test/gate is itself the risk surface — not for
an ordinary fix that merely adds a unit test (`core-reviewer` already checks
the negative test hits the failing branch).

Every review is a fresh `core-reviewer` — a new spawn, every time. Resuming
or conversation-reuse of a reviewer is banned, including round 2 of the same
task, a re-review after fix-up commits, and "just re-check this one
finding". A reviewer that already issued a verdict has an anchored position.
Independence comes from the empty context. Round 2 gets its own dispatch
with its own diff range.

Mechanics:

1. Read the original request, the approved plan step, and the real diffs
   (`git diff` / `git show`), never the author's self-report.
2. Scope first: the owner's shape kept, every new mechanism necessary, no
   second source of truth or temporary bridge. Then line-by-line against the
   plan step: every (a)-listed symbol touched? anything touched the step did
   not authorize? any part silently skipped, stubbed, or "simplified"?
3. Hunt, do not confirm. Attack the fix's own new seams first (a loop that
   can partially fail, a constant that shadows a config knob, an error class
   folded into success, a resource owned by the wrong scope). Then: what
   input, state, ordering, or partial failure makes this wrong; does the
   negative-path test execute the previously-wrong branch; is the at-risk
   topology split, not just monolith.
4. Findings as `class` · `file:line` · failing scenario · what it should be.
   "Looks fine" / "mostly matches" / "pass with reservations" / "minor nits"
   without a list are banned phrases. REJECT only for a concrete defect, a
   scope violation, or missing required validation, each with a source
   anchor and a failing scenario or affected requirement. Unverified
   concerns go in a separate list and do not change the verdict. No demand
   for unrelated hardening (`gamebackend.md` **Dev tooling scope**), no
   reopening the approved design.
5. Binary verdict: PASS with the taxonomy class list attacked for the files
   touched, or a punch list. No "pass with reservations". Bounce findings;
   never silently absorb them. A punch list of only comment findings ends
   the round; the orchestrator deletes the flagged comments inline.

If a fix exists only because the task introduced an unrequested mechanism,
the verdict is `REJECT — scope loop`: remove it or return to the approved
boundary. A fix that needs a new abstraction, module, storage layer, or
cross-module contract beyond the request/plan is a blocking owner question,
not an automatic fix.

Dispatch always to `core-reviewer` (explicit model ≥ the author's tier —
tone does not inherit; effort is the `effort:` parameter). The review is
mandatory but not a barrier — independent steps proceed while a fix lands
(see Implementation Mode).

Hard cap: 2 review rounds per task. A task is one plan step (or, off-plan,
one user request) — not one commit. Fix-up commits inside a task do not
restart the count. Round 1 reviews the diff; round-one fixes are committed
before round 2, which reviews them (fix commits carry `Kind: review-fix`); there is no round 3: stop, report what
stands open plus the recommended fix, wait. Within the 2 rounds fix by severity, not list order:
correctness first, comment/doc-truth only if it fits; leftovers go under
**Leftovers for owner** in the report, and the owner-deferred ones are written
to `docs/todo` by the lead.

Diffs under the dispatch threshold get no review round at all.

## Fix the Authority, Not the Symptom - MANDATORY

The implementation twin of adversarial review. A patch that corrects the
outcome while the flawed authority survives is banned.

1. **Locate the authority first.** Before writing a fix, name the single
   place that *decides* the behaviour (the config parser, the contract type,
   the one enum, the one SQL statement). The fix goes THERE.
2. **No hack-on-hack.** If a fix would add a second special case beside an
   earlier fix (another env fallback, another `if`, another wrapper around a
   wrapper), STOP: the authority itself is wrong — replace it. Preserve the
   good invariant from the earlier fix; do not revert-and-redo.
3. **Minimal sufficient closure.** State (in one sentence) what concrete
   defect this change closes and what the *minimal* closure is. Below that
   line is under-fixing; above it is gold-plating. Both directions burned
   this repo.
4. **Semantic changes are recorded, never smuggled.** Reversing a documented
   decision, changing a metric's semantics, or deviating from an approved
   plan gets named in the commit message AND an errata note in the
   plan/reference doc — in the same rollout, not "later".
5. **Prove the failing branch.** Every fix ships with a test that executes
   the branch that used to be wrong (not a test that merely exists near it),
   on the topology that's actually at risk (split, not just monolith). A
   negative path proven by construction beats one asserted by absence of
   errors.
6. **Sweep for siblings before leaving.** While the defect class is loaded
   in context, search for its siblings (same pattern, other call sites, the
   adjacent lifecycle owner) and either fix them in the same rollout or
   list them under **Leftovers for owner**.

These six rules are encoded in the `core-implementer` agent. Dispatch it for
authority-first work rather than restating them per prompt. It refuses to
finish without naming the authority and the failing-branch proof.

## Refactor Safety

- Verify dependency and wiring changes through the canonical runner:
  `cargo run -p verifyctl -- --fast`. Do not trust a grep saying "no
  consumers found"; make the Rust compiler and architecture checkers prove
  it. Follow the `safe-verification` skill first — one rollout at a time on
  the shared Postgres.
- Cargo rejects crate cycles, but not every forbidden acyclic edge. A direct
  module-to-module implementation dependency may compile, so the `archcheck`
  and fortress stages remain mandatory. Resolve cross-domain work through
  the durable bus (async) or a provider contract trait resolved from the
  registry (sync), never by importing the other module's implementation
  crate.
- Delete through dying chains. If a file depends on a dying type, ask
  whether the consumer is still meaningful instead of shimming it around the
  dead API.
