# Dev tooling tests the backend; it is not a security product

`devctl`, `verifyctl`, `splitproof`, and `processctl` exist to start, exercise,
verify, and clean up the game backend. Optimize them for accurate backend coverage,
one rollout at a time, useful diagnostics, bounded accidental failures, exact owned
process cleanup, and no secrets in argv/logs/state.

Assume a trusted local operator under one OS account. Do not expand work into custom
cryptography, malicious same-user defenses, elaborate ACL/reparse hardening, or
daemon-grade protocols unless a concrete backend-test failure requires it. A local
control path only needs ordinary OS-local permissions and bounds that prevent an
accidental partial client from hanging the rollout.

When review finds tooling hardening outside this threat model, record or reject it
instead of automatically implementing it. Review against a frozen functional
acceptance list so fixes do not recursively create a security-tool project.

For helper CLIs such as `cargo-audit`, prefer any already-installed version and
install the latest available release when missing. Do not pin an older tool version
merely because a previous script did; pin only when a demonstrated compatibility or
reproducibility constraint requires it.
