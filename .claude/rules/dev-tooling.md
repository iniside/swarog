---
paths:
  - "tools/**"
  - "weles/**"
---

# Dev tooling scope

Everything under `tools/` exists to develop, inspect, generate for, exercise, or
verify the game backend. These programs are development utilities, not production
services or products. Keep them simple and purpose-built for backend development.
They do not need certification, security-product hardening, encrypted local control
or state, custom cryptography, or defenses against a malicious local operator.

`devctl`, `verifyctl`, `splitproof`, and `processctl` specifically must start the
intended binaries with typed configuration, serialize rollouts, detect failures,
preserve useful logs/state, stop owned processes, avoid unrelated-process
kills/orphans, and report the backend test result accurately.

Assume a trusted local operator running under one OS account. Use ordinary OS-local
permissions and do not expose secrets in argv, logs, or state, but do not add
encryption-at-rest machinery, custom cryptography, same-user attack defenses,
elaborate ACL/reparse-point hardening, or daemon-grade control protocols unless a
concrete backend-test failure requires it.
Control paths must be bounded enough that accidental partial input cannot hang a
rollout; they do not need to resist a malicious user who can already kill/debug the
process. Review dev tooling against this functional threat model and treat unrelated
security hardening as out of scope.
