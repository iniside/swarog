---
name: a-green-signal-that-never-ran-the-thing
description: "Verified" must name a command that actually EXECUTED the thing — cargo check compiles tests without running them, and a `| tail` pipeline reports the pipe's exit code, not the tool's
metadata:
  type: feedback
---

Before accepting or claiming a verification, ask what the command actually
OBSERVED. Two shapes produced green signals that had seen nothing (2026-08-31,
notifications #3a):

- **`cargo check -p x --tests` compiles test targets without running them.** A step
  edited `cmd/gateway-svc/src/addrs_tests.rs`'s fixture, verified with `check`, and
  landed 8 broken tests. The sibling file's own doc comment says the quiet part:
  "build passed — build ≠ run". Any step touching a test, fixture, or golden must be
  verified with the command that EXECUTES it (`cargo test -p x`), and the dispatch
  prompt has to say so — subagents run exactly the command they are given.
- **A piped command reports the LAST stage's exit code.** `cargo run -p splitproof
  2>&1 | tail -60` exited 0 while the harness printed "1 assertion(s) failed"; I
  nearly reported a serious non-existent gate bug (harness passes while failing).
  Redirect to a file and echo `$?`, or `set -o pipefail`, whenever the exit code is
  the thing being judged.

**Why:** four blocking-gate cycles in one rollout, three of the four causes OUTSIDE
the module being built — a second set of generator goldens the freshness stage does
not diff, a bare `13` service count, and the fixture above. Every one was found by
`cargo test --workspace`, never by the dedicated gate that looked like it covered it.

**How to apply:** treat "N specialized gates are green" as weaker evidence than one
full workspace test run — the specialized gate usually checks the artifact NEXT to the
one that broke. When a subagent reports "verified", read which command it ran, not that
it says verified. Same family as [[prose-about-code-is-not-evidence]] and
[[cite-a-precedent-only-after-reading-it]]: the claim is a lead, the observation is the
evidence. Reinforces [[timing-sensitive-tests-doctrine]]'s "full-workspace cargo test is
the fragility detector".
