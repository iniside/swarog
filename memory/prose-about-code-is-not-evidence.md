---
name: prose-about-code-is-not-evidence
description: "A comment / status doc / memory that DESCRIBES code behavior is a claim, not evidence — open the code before repeating it"
metadata:
  node_type: memory
  type: feedback
  originSessionId: 31c06266-64af-4bcb-82be-f14d3b988287
  modified: 2026-08-31T00:00:00.000Z
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

4. (2026-08-11, accounts seq #2a Step 3) "An empty audience list in `jsonwebtoken` means
   'any'." FALSE — `set_audience` stores `Some(set)` unconditionally and validation is
   `!correct_aud.contains(aud)`, so an empty set rejects EVERY token: fail-closed, the
   exact inverse. **New shape: the false sentence was in the PLAN I wrote, and the
   implementing subagent copied it into a doc comment as the stated justification for a
   security guard.** Prose about a THIRD-PARTY dependency is the same class as prose about
   our own code — and a plan is not a citation just because I wrote it. Caught by the
   adversarial review, verified by me in `~/.cargo/registry/.../validation.rs`.

5. (2026-08-26, accounts seq #2a Steps 5-7) A gate's own DATA field can be unexecuted prose.
   `tools/conformance`'s `InputPolicy::Validated { basis }` reads like a proof obligation; it
   is consumed by a non-blank check and a `println!`, and is not in the diffed golden. An
   auditor's receipt: a fully GREEN run in which the basis said "65536" and "guest's 128-byte
   ticket" while the real widest cap was 999999. **New shape: prose inside a verification
   tool, where the surrounding machinery makes it look executed.** Same session: deleting BOTH
   cap guards from the production handler left every gate green and `cargo test` passing.

6. (2026-08-31, mail seq #3b Steps 2-3) **Three in one rollout, all in the plan I wrote.**
   `HistoryPolicy::Days(7)` — no such variant (`MinRetention { days }` / `KeepForever`).
   Step 1's "compare against `USABLE_PG_SESSIONS + 3`" — would have refused a monolith
   rollout that fits. And the load-bearing one: "a failed checkpoint UPDATE aborts the whole
   worker pass, starving every other subscription" — `core/asyncevents/src/worker.rs:437-446`
   catches the `Err` and breaks only THAT subscription's quantum. The requirement (validate
   before any statement) was right; the stated reason was invented. The real consequence is
   stronger — the success arm returns before `record_failure`, so nothing backs off or
   pauses and the event hot-loops forever with `/readyz` green. **The implementing subagent
   copied the false sentence into a code comment as the step's justification, exactly as in
   #4.** Caught by review, verified by me in the worker source.

**Why:** prose drifts from code silently — nothing recompiles a comment. A false comment is
worse than none, because it *stops* the next reader from checking. And I was the next reader.
This is also why `docs/reference/weles-design.md` exists: a design that lives only in agent
memory gets reconstructed wrong (I proposed a client crate in `weles/`, contradicting the
decided wire-only contract).

**How to apply:** when about to state how code behaves, ask "did I read the code, or read
*about* the code?" A comment, status doc, plan, review verdict, or memory is a lead, not a
citation — verify before repeating, and say which one you did. **This binds hardest when
writing a plan**: a claim I put in a plan gets executed by a subagent that treats it as
settled, so an unverified sentence there becomes a comment in shipped code. Verify
dependency behaviour against the vendored source (`~/.cargo/registry/src/...`) before
asserting it in a plan, not after review catches it. When a comment turns out to
be false, fix it in the same rollout (a lie that survives will be repeated by the next
context). Corollary for reviews: the least-tested guards are often the ones the comments
claim most confidently. Related: [[scope-claims-to-what-was-verified]],
[[adversarial-subagent-review]], [[historical-docs-are-archives]],
[[mini-orchestrator-native-no-containers]].
