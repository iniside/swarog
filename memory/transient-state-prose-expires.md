---
name: transient-state-prose-expires
description: "A prose correction describing a transient state (stub, deferred pairing, unwired path) expires when the step that changes that state lands — sequence it after, or describe the invariant"
metadata:
  type: feedback
---

In the `storage` rollout I dispatched a docs pass to correct three claims that described
Step 3/4 behaviour in the present tense. It was right: at that moment `Player::put` really
was a stub. **Eleven minutes later Step 3 landed the real write path** and two of those
corrected strings were false again, in the opposite direction — now claiming a stub that
no longer existed. The adversarial review returned REJECT on an otherwise-clean diff, and
its first punch-list item was my own freshly-landed prose.

Worse: the twin sentences *inside* the module were updated by the implementing commit, so
the module and the conformance tool contradicted each other about the same probe.

**Why:** I treated "this comment is false" as sufficient reason to fix it now. But the
comment was false *about a stage*, and I was about to dispatch the step that ends the
stage. Correcting stage-description prose immediately before the stage changes is work
with a built-in expiry date.

**How to apply:**
- Before dispatching a prose correction, ask **"is this describing a stage or an
  invariant?"** Stage-description (a stub, a deferred pairing, a not-yet-wired path,
  an observed-red gate) ⇒ either sequence it AFTER the step that changes it, or fold it
  into that step's own docs pass.
- Prefer rewriting to the **invariant**: "the CHECK is a backstop measuring normalized
  jsonb, not a twin of the Rust cap" survives every step; "no statement exists yet"
  survives one.
- `basis`/`rationale` strings in `tools/conformance/src/policy.rs` are checked only for
  non-blankness — nothing executes them, so nothing catches their drift. Same hazard
  class as [[not-pinned-lists-are-unexecuted-prose]].
- When the implementing commit updates a sentence, grep for its **twin in the other
  crate** — the module and the tool describe the same probe in two places.

Related: [[prose-about-code-is-not-evidence]], [[ask-for-observed-gate-state]],
[[not-pinned-lists-are-unexecuted-prose]].
