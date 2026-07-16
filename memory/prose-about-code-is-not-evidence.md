---
name: prose-about-code-is-not-evidence
description: "A comment / status doc / memory that DESCRIBES code behavior is a claim, not evidence — open the code before repeating it"
metadata:
  node_type: memory
  type: feedback
  originSessionId: 31c06266-64af-4bcb-82be-f14d3b988287
---

Three false assertions in ONE session (2026-07-16, weles M1 design), all from the same
root: I repeated prose that described code, without opening the code.

1. "The readiness ⊥ restart invariant is held by the dedicated poller thread." FALSE — it
   is held by a type boundary (`readiness_for -> Readiness` has no constructor into
   `Observed`/`Directive`) plus a pure match arm (`Phase::Healthy => { Exited => crash,
   _ => Stay }`). The thread is pure latency isolation. **Source of the error: the code's
   OWN doc comment said "observe/step never see a probe" — and that comment was a lie.**
   `observe()` probes in `WaitingHealthy` and feeds `step()`.
2. "weles being std-only was never a decision, just an emergent property." FALSE — it was
   finding #13 in the M0 plan review. (The reverse error: a readiness review called it an
   *invariant*, which I ALSO repeated. It was neither — a decision whose rationale was
   scoped to M0.)
3. "Agents are dumb spawn/kill/status executors" (from memory) — undersells the agent; the
   restart policy and local supervision live there, and that is weles's differentiator.

**Why:** prose drifts from code silently — nothing recompiles a comment. A false comment is
worse than none, because it *stops* the next reader from checking. And I was the next reader.
This is also why `docs/reference/weles-design.md` exists: a design that lives only in agent
memory gets reconstructed wrong (I proposed a client crate in `weles/`, contradicting the
decided wire-only contract).

**How to apply:** when about to state how code behaves, ask "did I read the code, or read
*about* the code?" A comment, status doc, plan, review verdict, or memory is a lead, not a
citation — verify before repeating, and say which one you did. When a comment turns out to
be false, fix it in the same rollout (a lie that survives will be repeated by the next
context). Corollary for reviews: the least-tested guards are often the ones the comments
claim most confidently. Related: [[scope-claims-to-what-was-verified]],
[[adversarial-subagent-review]], [[historical-docs-are-archives]],
[[mini-orchestrator-native-no-containers]].
