# 4. `resource-owned-by-wrong-scope` — *a handle/lease/connection/permit owned by the wrong thing*
A resource is owned implicitly by a library type or an async frame when the code actually
needs typed, cancellation-correct ownership — so it leaks, is returned to a pool poisoned, or
is released on cancel while the work keeps running.
- **Recurred:** pooled connection returned in ABORTED-TRANSACTION state after failed DDL,
  then the recovery ROLLBACK's own failure discarded (`23a8aee → 4b7d41c`); Windows process
  handle owned opaquely by `std::process::Child` when the tool needed it for job/console-ctrl
  (`02ff417`); ambient env read via ad-hoc `std::env::vars()` at multiple sites instead of one
  captured snapshot (`8e96c15`); argon2 permit owned by the async handler frame instead of the
  blocking closure (accounts/admin — see `.claude/rules/domain-modules.md` admin entry); scheduler aborted task's
  JoinHandle never awaited (`0431926`); the whole lock/lease chain.
- **Attack:** trace **who actually calls `Drop`/close/release** on this resource, and whether
  two code paths (owner vs borrower, async frame vs blocking closure, two `env::vars()` calls)
  can observe it in different states. **Attack the fix's OWN recovery action** — "what if the
  rollback / the cleanup / the abort itself fails".
- **Authority:** one owning type (`EnvironmentSnapshot`, `PlatformChild`, a unified rollout-
  lock owner); on recovery failure, `detach()` + drop rather than return-to-pool.
