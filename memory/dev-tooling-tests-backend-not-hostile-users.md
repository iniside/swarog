---
name: dev-tooling-tests-backend-not-hostile-users
description: "Dev tooling (devctl/verifyctl/splitproof/processctl) tests the backend, it is not a security product — keeps only the cargo-audit versioning delta not in CLAUDE.md"
metadata: 
  node_type: memory
  type: feedback
  originSessionId: 88cdd953-b406-40a0-8ab2-6c7eb07acece
---

The threat model (trusted local operator, one OS account; no custom crypto, same-user
defenses, ACL/reparse hardening, or daemon-grade protocols unless a concrete backend-test
failure requires it) is fully in CLAUDE.md `## Dev tooling scope — MANDATORY`. This memory
keeps only the delta not there:

**cargo-audit / helper CLIs:** prefer any already-installed version and install the latest
when missing. Do NOT pin an older tool version merely because a previous script did; pin only
when a demonstrated compatibility/reproducibility constraint requires it. When review
proposes tooling hardening outside the threat model, record or reject it — don't recursively
turn a test harness into a security-tool project.
