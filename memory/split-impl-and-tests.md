---
name: split-impl-and-tests
description: "tests are a separate, later plan step (the [test-author] lane) — never bundled into the implementation step that produced the code"
metadata: 
  node_type: memory
  type: feedback
  originSessionId: c652fcd6-7214-4c23-9374-4facf2b1c91e
  modified: 2026-07-23T11:02:04.795Z
---

Tests get their **own** plan step, sequenced *after* the implementation step it
covers has landed and compiled, dispatched to the `[test-author]` lane
(`.claude/agents/test-author.md`). Banned: any step shaped "implement X **and**
write its tests". Ported from ArcGame 2026-07-22 (user asked to transplant it
here); encoded in CLAUDE.md Plan Writing step 4 (MANDATORY sub-bullet) +
Implementation Mode ([test-author] = the ONLY test-writing lane, 5th lane).

**Why:** one agent holding impl + tests re-processes the whole implementation
through the compile/test-fix loop each iteration — burns tokens. A test-author
starting from a fixed, landed signature does not re-derive the impl; it just
proves it. This formalizes discipline the repo already valued (constraint #10
tests-in-separate-files, Fix-the-Authority rule 5 prove-the-failing-branch,
[[specialized-core-agents]] proof-auditor/core-reviewer branch-coverage checks).

**How to apply:** a plan touching testable production code with NO separate
`[test-author]` step is incomplete unless the user explicitly waived tests. The
test step names: the landed commit/behaviour, the previously-wrong branch each
test must exercise, and the at-risk topology (split via `tools/splitproof`, not
just monolith). Test lane is never `[sonnet]` — it goes through `[test-author]`.
**Sonnet is the preferred/default model for the test-author lane** (it works from
a landed, compiling diff, so pattern-following test work is Sonnet work); escalate
to opus/fable only for a novel harness/topology. Absolute rule, no carve-out for
trivial single-branch proofs.
