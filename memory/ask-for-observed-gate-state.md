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
