# Testing — placement and fixture-isolation conventions

This file owns only where tests live and how they stay isolated on the shared
database. Everything else has an owner:

- Test database (strict `testdb` gate, local Postgres, no Docker/testcontainers):
  `.agents/shared/gamebackend.md` **Database**.
- Stage map and how to run the gates: `.agents/shared/gamebackend.md` **Commands**.
- One rollout at a time: `.agents/shared/gamebackend.md` **One test rollout at a
  time** and the `safe-verification` skill
  (`.agents/skills/safe-verification/SKILL.md`).

## Placement

- Unit and live-Postgres tests live in sibling files, never inline in impl files
  (`.agents/shared/gamebackend.md`, persistence rule). The impl file wires them with
  `#[cfg(test)] #[path = "x_tests.rs"] mod x_tests;` (e.g.
  `core/asyncevents/src/retention.rs`), or `#[cfg(test)] mod tests;` for the
  crate-wide `src/tests.rs`.
- Integration tests that boot a real `main` live in `cmd/<name>/tests/` (today
  `cmd/server/tests/tls_front.rs`, `cmd/gateway-svc/tests/boots.rs`,
  `cmd/gateway-svc/tests/stub_swap.rs`).

## Fixture isolation on the shared database

- Tests run in parallel against one Postgres. Each test creates its own rows or
  topics under a unique name (`unique_topic` in `core/asyncevents/src/tests.rs`,
  `unique` in `retention_tests.rs`) and asserts only on those, never on global
  counts.
- When a test needs something global, serialize it with a named guard instead of
  relying on timing. Both live in `core/asyncevents/src/` and their doc comments are
  the contract:
  - `GLOBAL_SWEEP` (`retention_tests.rs`): tests that sweep database-wide take
    `.write()`, tests that depend on their fixtures surviving take `.read()`.
  - `WRITER_LOCK_CHOREOGRAPHY` (`store_tests.rs`): taken by any test that holds an
    uncommitted write across an await on another connection or takes a conflicting
    lock on `asyncevents.events`. Lock order: `GLOBAL_SWEEP` first.
- Tests prove correctness, not speed. Wait on an explicit signal (for example
  `Notify`) wrapped in a `tokio::time::timeout` with generous headroom, a hang-guard
  and not a latency bound (`core/asyncevents/src/tests.rs`,
  `stop_terminates_active_backend_…`); never use a sleep as proof.
