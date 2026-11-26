# Core lifecycle

Last updated: 2026-11-26

## In progress

_None._

## Open

- [ ] **`Ddl` takes only `&'static str`**
  A migrate that fills a schema name into its DDL must leak the strings: accounts' `on_schema`
  (modules/accounts/src/signing_keys.rs:221-226) leaks three per `migrate_signing_keys` call, once per
  boot in production. That is bounded and harmless, but it is the first `Box::leak` outside test code
  in modules/. Fix: `Ddl` takes `Cow<'static, str>`, and the leak goes.
  Source: core-reviewer and proof-auditor of 309d8708 (oidc-signing-keys step 2, 2026-11-26).

- [ ] **Generic bound on Module::start**
  CORE-LIFECYCLE-01. Only `stop` is time-bounded; `App::start` awaits
  `m.start(&self.ctx)` with no timeout (`core/lifecycle/src/app.rs:241`, read at
  HEAD; docs/reviews/2026-07-13-2138-audit-remediation-status.md:68,
  2026-07-13).
