# Planning And Dispatch

## Plan Writing Workflow

Research, plan, present, then edit, per the `research-plan-present` skill
(`.agents/skills/research-plan-present/SKILL.md`). A plan gets one
`core-reviewer` review, before approval; nothing is edited before the owner
approves.

## Implementation Mode

Lanes, models, prompts and hand-offs: the `implementation-dispatch` skill
(`.agents/skills/implementation-dispatch/SKILL.md`). The binding part:

- The lane is decided per plan step, approved with the plan, and never
  flipped mid-rollout without asking the owner. Every Agent call passes an
  explicit model; nothing inherits.
- **Dispatch threshold — below it, `[inline]`, and a subagent is banned.** No
  subagent call, no plan tag, no approval, no review round for exactly these:
  comment edits of any size, log/format/UI strings, include add/reorder, a
  literal or typo fix, a rename inside one file, the lead's `docs/todo/`
  edits outside a dispatched step (adding owner-deferred leftovers, and
  moving an item to `## In progress` before dispatching the step that works
  on it). That list is closed — it is not a size rule. A change that is not
  on it goes to a subagent lane however small it looks; never divide a
  feature to fit the threshold.

## Adversarial Diff Review

The method is the `diff-review` skill (`.agents/skills/diff-review/SKILL.md`).
The dispatch rules:

- Every produced code diff — a subagent's or the main agent's — is reviewed by
  a fresh `core-reviewer` subagent, never inline: the context that wrote the
  code cannot judge it. Dispatch it after every task commit, on that commit
  plus the cumulative diff from the task-start commit to `HEAD`; tests
  written in the step are reviewed with it. Model ≥ the author's tier.
- Never resume a `core-reviewer` or `proof-auditor`, including round 2, a
  re-review after fix-up commits, and "just re-check this one finding".
- No review: prose-only commits (`docs/`, `.agents/`, `.claude/agents/`,
  `docs/plans/`, memory), except the one plan review before approval; diffs
  under the dispatch threshold. Not prose: `.rs` doc comments on `api/` and
  `core/` contracts are code; gate baselines and goldens
  (`docs/reference/contract-golden/`, `docs/reference/public-api-baseline/`)
  and `tools/admindiff/accept/*.txt` are reviewed with `proof-auditor`.
- Add `proof-auditor` only when the diff touches a verify stage (verifyctl /
  archcheck / conformance / topiccheck / golden) or the test/gate is itself
  the risk surface — not for an ordinary fix that adds a unit test.
- One review round per task (one plan step, or one off-plan request; fix-up
  commits do not restart the count). A second round only when the task
  touches `core/`, split/edge wiring, or a verify gate (verifyctl, archcheck,
  conformance, splitproof, goldens); there is no third.
- Every finding names its origin: `local` (this diff) or `structural` (wrong
  layer, flawed plan, half-ported subsystem, second source of truth). A
  structural finding gets no review-fix round and is never patched one by
  one: the main session proposes a fix for the cause and brings it to the
  owner, and no further step of the same plan is dispatched until the owner
  rules.

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
