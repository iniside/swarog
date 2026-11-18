# Planning And Dispatch

## Plan Writing Workflow

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
   follows `research-navigation.md` **Research / Search Mode**.
2. Research three non-overlapping angles: API surface (public signatures,
   `lifecycle::Module` methods, contract traits, `bus::define` descriptors),
   API usages (who constructs and consumes, `registry::require` callers,
   `on_tx` subscribers, slot contributors), and patterns (how existing
   modules register, migrate, emit/subscribe, contribute admin). Synthesize
   in the main agent — never write off one subagent; a search-lane answer is
   a lead (`research-navigation.md` **Navigation**).
3. Write concrete specifics: exact files, exact signatures, exact API calls,
   sequencing. Banned phrases ("figure out as we go", "TBD", "investigate
   during implementation", "may need to", "something like") mean a research
   gap — go back to step 2.
4. Structure as an ordered `Step 1 → Step 2 → …` sequence, not a catalog. Each
   step states **(a)** what is touched (exact files/symbols), **(b)** why now /
   order — the dependency forcing it before the next, **(c)** how —
   non-mechanical moves spelled out (`requires()`, `registry::provide` /
   `registry::require` wiring, edge glue, registration in `cmd/server` and
   the service composition root), **(d)** dispatch tag. A catalog that
   leaves order/topology/per-step actions to "figure as you go" is banned;
   steps need not each compile, but every step must be written out.

   **Step size.** One step is one mechanism in one module or one consumer
   group, sized so one implementer finishes it in about 60 tool calls:
   roughly eight files, or one subsystem port. About 100 calls is where the
   implementer stops and hands off. A larger unit is split into ordered
   steps when the plan is written, each still a whole slice of the feature
   (never split to get under the inline threshold or to keep an old path
   alive).

   **Tests.** The implementer writes the tests in the same step as the code,
   to the bar "revert the fix and the test must fail". A separate, later
   `[test-author]` step is only for verify gates, splitproof assertions and
   new test harnesses; it names the landed commit/behaviour, the
   previously-wrong branch each test must exercise, and the at-risk topology
   (split, not just monolith). A plan that touches testable production code
   with no tests is incomplete unless the user explicitly waived tests.
5. Dispatch `subagent_type: "core-reviewer"` — that field is written first,
   before `description` and before the prompt. Never a generic implementer
   or explore/plan type. Do not hand-author a "grumpy senior engineer"
   persona. A plan gets one review, before approval. It checks scope
   substitution first, then hunts logical holes, missing pieces (a module's
   `migrate`, a `requires()` that does not match the real synchronous
   dependency, an event mutated instead of evolved additively, an import of
   another module's implementation crate, a cross-module foreign key),
   ambiguity and unstated assumptions. It rejects only with evidence, lists
   unverified concerns separately, and returns a punch list, never a
   rewrite. Address it before showing the user, or note deferrals with
   rationale.

## Implementation Mode

Dispatch is decided per plan step (Plan Writing 4d), not per session. Tags
describe capability and cost; `.agents/adapters/claude.md` maps them to
agents and models. Claude plans may write `[opus]` / `[fable]` for
`[independent]` and `[sonnet]` for `[mechanical]`; do not ban `[fable]`.

- `[inline]` — the main agent; the exception, not the default. Two cases only: the closed
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
  never `[mechanical]`, with one exception: a provider port's view part
  (`b`) whose `mockup-implementation` checklist is complete and has no open
  contract gap runs `[mechanical]` on the `mockup-implementer` persona.
  Checklist extraction, contract decisions and review of that part stay at
  the top tier. A gap found during the view part goes back to the backend
  part. A port that goes badly is fixed in the skill, its tooling and its
  tests, never by moving the view part to a higher tier. Documentation prose
  is never `[mechanical]`; it goes through `[docs]`.
- `[test-author]` — verify gates (golden render coverage, `admindiff`,
  admincheck and conformance findings), splitproof assertions and new test
  harnesses (a new event-plane fixture), always a separate step after the
  implementation it covers. Other tests belong to the implementation step.
  Top tier by default; cheap tier when it copies a reference written at the
  top tier. A cheap-tier test that goes badly is fixed in the reference
  file, skill and tooling, never by moving it to a higher tier.
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

Every Agent call passes an explicit model from the adapter; nothing
inherits. Tags are approved with the plan. Ask only for untagged/ad-hoc
work; an authorized lane is not reopened. Mid-rollout, never flip a step's
lane tag silently — ask the owner first. Effort is the Agent `effort:`
parameter, its value per `core-rules.md` **Agent Effort**; prompt wording
does not set it. Commit after each task (subagents may commit their own).

**Prompts and hand-offs.** A subagent starts from a fresh context: give it
the task facts and source anchors, not the conversation. A prompt states the
goal, authority, owned files, verification and commit boundary, plus the
navigation chain for code-touching work (`research-navigation.md`
**Navigation**). A hand-off states the outcome, verified anchors, what was
and was not validated, open questions, and **Leftovers for owner** (format in
`core-rules.md` **Plans And Status Docs**). A subagent's commit trailer names
the executing model (adapter **Commit Trailers**); after a multi-subagent
rollout, audit the trailers against each step's lane.

**Step parts.** An implementer that returns a part hand-off
(`.claude/agents/core-implementer.md` **Long steps**) is followed by a fresh
one given the step text and that hand-off. A part gets no review of its own;
the review runs on the step's cumulative diff once the step is done.

**Review does not gate the next task — run them in parallel when the tasks
are independent.** Every reviewed task still gets its `core-reviewer` pass, but that
pass only blocks the next dispatch when the next step actually depends on
it. Serialize only if the two steps touch any of the same files, or the later
step builds on an API/behaviour the earlier one introduces (plan step (b)
already names that dependency). Otherwise dispatch the reviewer and the next
implementation concurrently, in one message — the reviewer is read-only, so
the sole hazard is two writers in one file.

When a parallel review returns a punch list, triage it by origin
(**Adversarial Diff Review**): local findings get one fix commit carrying
`Kind: review-fix`; structural findings go to the owner. If a fix touches a
file the in-flight task is editing, wait for that task to land, then fix —
never edit a file a live subagent holds.

## Adversarial Diff Review

Every produced code diff — a subagent's or the main agent's — is reviewed
adversarially: hunt implementation gaps and structural problems, and
challenge the plan itself. A defect that follows from the plan is reported as
a plan defect, never passed as "matches the plan". The vibes skim ("roughly
matches the plan → pass") is banned.

The review runs in a subagent, never inline. Dispatch `core-reviewer` after
every task commit, on that commit plus the cumulative diff from the
task-start commit to `HEAD`; tests written in the step are reviewed with it.
Prose-only commits (`docs/`, `.agents/`, `.claude/agents/`, `docs/plans/`,
memory) get no review, except the one plan review before approval; gate
baselines and goldens (`docs/reference/contract-golden/`,
`docs/reference/public-api-baseline/`) and `tools/admindiff/accept/*.txt`
accept lists are not prose and are reviewed with `proof-auditor`; `.rs` doc
comments on `api/` and `core/` contracts are code. Inline self-review is the banned failure mode —
the context that wrote the code cannot judge it. Route by files-touched
through `docs/reference/core-failure-taxonomy.md`, with a different method
than the implementer used. Add `proof-auditor` ONLY
when the diff touches a verify stage (verifyctl / archcheck / conformance /
topiccheck / golden) or the test/gate is itself the risk surface — not for
an ordinary fix that merely adds a unit test (`core-reviewer` already checks
the negative test hits the failing branch).

Every review is a fresh `core-reviewer` — a new spawn, every time. Never
resume a `core-reviewer` or `proof-auditor`; resuming
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
4. Findings as `class` · origin (`local` or `structural`, below) · `file:line`
   · failing scenario · what it should be.
   "Looks fine" / "mostly matches" / "pass with reservations" / "minor nits"
   without a list are banned phrases. REJECT only for a concrete defect, a
   scope violation, or missing required validation, each with a source
   anchor and a failing scenario or affected requirement; style, comment and
   wording findings are not REJECT grounds. Unverified
   concerns go in a separate list and do not change the verdict. No demand
   for unrelated hardening (`.claude/rules/dev-tooling.md`). A defect in the
   approved design is reported as a structural plan finding for the owner;
   it is neither suppressed nor patched.
5. Binary verdict: PASS with the taxonomy class list attacked for the files
   touched, or a punch list. No "pass with reservations". Bounce findings;
   never silently absorb them. A punch list of only comment findings ends
   the round; the orchestrator deletes the flagged comments inline.

Origin means what produced the finding, not only its symptom: a local slip,
or a structural cause. Structural causes include a second copy of a rule (a
test, gate or tool that restates production logic, a hand list beside the real
source), a mechanism where each fix adds another special case, a seam where
new code meets differently-shaped old code, the wrong layer, a flawed plan,
and a plan scope that leaves part of the subsystem behind. A finding in a
mechanism that an earlier round of the same task already fixed is structural
by default. Before dispatching any fix, the main session groups the findings
by origin. A structural finding stops the loop: it gets no review-fix round
and is never patched one by one; the main session proposes a fix for the
cause (fix the plan, finish the subsystem, replace the mechanism, delete the
copy, derive from the real source) and brings it to the owner. No further
step of the same plan is dispatched until the owner rules.

If a fix exists only because the task introduced an unrequested mechanism,
the verdict is `REJECT — scope loop`: remove it or return to the approved
boundary. A fix that needs a new abstraction, module, storage layer, or
cross-module contract beyond the request/plan is a blocking owner question,
not an automatic fix.

Dispatch always to `core-reviewer` (explicit model ≥ the author's tier —
tone does not inherit; effort is the `effort:` parameter). The review is
mandatory but not a barrier — independent steps proceed while a fix lands
(see Implementation Mode).

One review round per task by default. A task is one plan step (or, off-plan,
one user request) — not one commit. Fix-up commits inside a task do not
restart the count. A second round only when the task touches `core/`,
split/edge wiring, or a verify gate (verifyctl, archcheck, conformance,
splitproof, goldens); it reviews the committed round-one fixes (fix commits
carry `Kind: review-fix`). There is no further round: stop, report what
stands open plus the recommended fix, wait. Fix by severity, not list order:
correctness first, comment/doc-truth only if it fits; leftovers go under
**Leftovers for owner** in the report, and the owner-deferred ones are written
to `docs/todo` by the lead.

Diffs under the dispatch threshold get no review round at all.

## Fix the Authority, Not the Symptom

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
