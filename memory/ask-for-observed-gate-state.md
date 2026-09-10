---
name: ask-for-observed-gate-state
description: "Telling subagents \"expect these red gates\" converts unknown red into expected background; ask what they OBSERVED, and run the suite between steps"
metadata: 
  node_type: memory
  type: feedback
  originSessionId: c7d0412d-b72c-4fd3-8e8d-fcbef1316c22
  modified: 2026-09-06T10:53:03.484Z
---

In a multi-step rollout I told every implementer "expect red gates, don't chase them"
and listed the ones I predicted from the plan. The list was shorter than reality:
`codegen-freshness` and `test` were red for **seven commits** because a new contract
crate's DTO name collided cross-crate, and nobody reported it — each agent had my
ready-made explanation for the red it saw.

**Why:** a warning about expected failure is also permission to stop looking. The
instruction is well-intentioned (don't burn budget on another step's mess) but it makes
me the authority on a fact I have not checked.

**How to apply:**
- Never ask "which gates did your step redden?" — that collects predictions and requires
  running nothing. Ask: **"run the suite and report verbatim what failed, or state that
  nothing did."**
- Run `cargo test --workspace --exclude verifyctl --no-fail-fast` **between steps**, not
  only at the end. In this rollout one such run found a missing authority
  (`apikeys::DEV_CLIENT_POLICY`) that four attempts to assemble the list by *reading*
  had all missed — and whose absence would have failed the split-proof assertions with a
  403 that looked like a key-policy bug.
- If a red gate really is expected, say **which commit introduced it and when it clears**,
  so a wider red is visibly different from the predicted one.
- Same discipline for "red by design" claims about a stage: verify it. I asserted
  split-proof stayed red for the new module; it was **green and blind to it** — worse,
  because a PASS looks like proof.

Related: [[scope-claims-to-what-was-verified]], [[a-green-signal-that-never-ran-the-thing]],
[[prose-about-code-is-not-evidence]], [[didnt-forget-scripts-must-self-check]].

**Recydywa 2026-09-10, w drugą stronę: I asserted a red window that did not exist.** In
the `groups` plan (2026-09-06) I wrote "there is no red window there" — four blocking
gates were red from the first commit, forcing a six-commit gate-retarget detour. Four
days later, in the account-deletion plan, I wrote the mirror image: "`--durability-strict`
is red until Step 4 lands, which is why Steps 3 and 4 are one rollout." Also false —
Step 2's `audit` list edit becomes a real `on_tx_raw` subscription pinned to version 1,
so the topic is subscribed from Step 2. The plan contradicted itself two steps later.

**Why this is its own habit, not an instance of [[prose-about-code-is-not-evidence]]:**
both errors are *predictions about gate state used to justify a structural decision* —
a step boundary, a rollout shape. The cost is not a wrong sentence; it is a wrong plan
skeleton that agents then execute faithfully.

**How to apply:** a claim about what a gate does after step N is only writable in a plan
after **running that gate**, or after reading the gate's own predicate and naming it
(`unsubscribed()` keys on `(topic, version)`; `on_tx_raw` pins version 1). Never derive
it from what the step "obviously" leaves incomplete. If it cannot be executed at
planning time, write the open question instead of an assertion — a plan that says
"verify before Step 3 whether X is red" costs nothing and cannot mislead.
