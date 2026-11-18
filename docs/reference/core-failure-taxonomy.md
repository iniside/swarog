# Core Failure Taxonomy

Catalog of the failure classes `core-implementer` and `core-reviewer` key on.
Update it when a NEW class shows up in a real commit, not from guesswork. Derivation
and the "why double review failed" evidence: [history.md](core-failure-taxonomy/history.md).
Companion rules: `.agents/shared/planning-dispatch.md` (**Fix the Authority, Not the
Symptom**, **Adversarial Diff Review**).

Each class file holds: definition, where it recurred, the attack, the authority move.
A passing test is the #1 disguise in this catalog.

## Classes

1. [`error-folded-into-success`](core-failure-taxonomy/01-error-folded-into-success.md) — a real failure absorbed into an apparent success.
2. [`unbounded-operation`](core-failure-taxonomy/02-unbounded-operation.md) — no timeout/cap/grace, or a bound on the wrong phase.
3. [`ordering-not-structural`](core-failure-taxonomy/03-ordering-not-structural.md) — a required order/unit enforced only by luck.
4. [`resource-owned-by-wrong-scope`](core-failure-taxonomy/04-resource-owned-by-wrong-scope.md) — handle/lease/connection/permit owned by the wrong thing.
5. [`constant-shadows-config-knob`](core-failure-taxonomy/05-constant-shadows-config-knob.md) — a threshold duplicated or hardcoded beside its real source.
6. [`coverage-gap` / `false-pass`](core-failure-taxonomy/06-coverage-gap-false-pass.md) — a gate that cannot see the thing it gates.
7. [`notapplicable-hides-gap`](core-failure-taxonomy/07-notapplicable-hides-gap.md) — a "does not apply" rationale that is false.
8. [`hand-maintained-list-drift`](core-failure-taxonomy/08-hand-maintained-list-drift.md) — an enumeration with no self-check against its source.
9. [`topology-blind-violation`](core-failure-taxonomy/09-topology-blind-violation.md) — correct in the monolith, bypassed in the split (highest severity).
10. [Long tail](core-failure-taxonomy/10-long-tail.md) — retry semantics, budget starvation, unsupervised task death, torn writes, sentinel collision, lost update, cache TTL, security binding, policy drift, silent truncation, codegen governance.
11. [`old-rail` / `dual-path`](core-failure-taxonomy/11-old-rail-dual-path.md) — the replacement landed and the old rail still runs.
12. [`scope-substitution` / `fix-forward-loop`](core-failure-taxonomy/12-scope-substitution.md) — coherent code, wrong product.
13. [`dispatch-process-discipline`](core-failure-taxonomy/13-dispatch-process-discipline.md) — lane, model, trailer and commit hygiene.

---

## Cross-cutting review checklist — keyed to what the change touched

The point of specialized review is NOT generic hostility — it's loading the right 3-5 attacks
for the files in the diff. Route by what changed:

- **Touches `core/app::run` / lifecycle sequencing / teardown/drain** → classes 3, 2, 4. Ask:
  which caller needs this constraint (prod vs test)? does drain cancel work spawned *inside*
  the dropped future? is membership frozen at boot or recomputed per request? does every stop
  path have its bound?
- **Touches `core/asyncevents` / retention / delivery** → classes 1, 5, 2. Ask: does the
  caller see this error (readyz stamp)? is this threshold a duplicate of a configured value?
  is the lock/handler bounded? does the poison/failing branch have a live-PG test?
- **Touches `core/edge` / `core/remote` / RPC glue** → classes 2, 1 (retry), 4. Ask: dial
  bound vs RPC bound? is the retry classifying on the raw error or a collapsed status? is the
  twin plane (public vs internal) swept? does serialize failure become `Internal` or `null`?
- **Touches `tools/` rollout (processctl/devctl/splitproof)** → classes 4, 1, 8, 6. Ask: is
  this the 2nd+ commit on this authority this week (STOP, redesign)? who owns the
  handle/lease/env? does teardown status see cleanup failures? does the list self-check? does
  the harness prove the seam or just a 200?
- **Touches `tools/` verify (verifyctl/archcheck/conformance/topiccheck/golden)** → classes 6,
  7, 8. Ask: does the test call the real gate fn or a bypass fixture? would a rename make this
  rule vacuous? does a NotApplicable/Opaque stance hold against the handler? is the gate's own
  tool-acquisition failure a FAIL not a green SKIP?
- **Touches `modules/*` (domain)** → classes 2, 9, 5, and the long tail. Ask: is an env gate
  traced through edge + gateway routing, not just the monolith `if`? is this proven on split?
  is a cap duplicated across fortresses? render-then-submit without a concurrency token?
- **Touches instruction files / `.agents/` / a replacement of a path** → class 11, then
  5 and 8. Ask: what did this introduce, what must be dead, is the old rail still
  loaded (second entry file, alias agent, dual lane vocabulary without an adapter
  map)? Does a hand-maintained citation list still teach the fat heading?
- **Any plan, and any diff against its request** → class 12, first. Ask: is every
  new mechanism one the owner asked for or one a stated requirement forces? does
  a fix exist only to harden an unrequested addition?
- **The diff itself — how it was dispatched, tagged, and committed** → class 13.
  Ask: does each commit's lane, `model:`, and trailer match its plan step? one
  task per commit, with the plan-step suffix? does each review-fix commit carry
  `Kind: review-fix`?
