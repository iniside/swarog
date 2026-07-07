---
name: local-postgres-is-the-test-db
description: "For tests/integration, the local Postgres IS the DB — do not frame it as a Docker/Testcontainers fallback"
metadata: 
  node_type: memory
  type: feedback
  originSessionId: 2dde7081-732d-49f5-b0aa-ce19637ba5f1
---

The project runs a real local Postgres (`gamebackend` DB at repo root; `jvmsketch` for the Quarkus sketch —
see [[reference_local_postgres]]). It's up and works. For integration tests, **that local Postgres is the
correct target** — not a compromise.

**Why:** the user pushed back sharply ("postgres normalnie działa, weź się kurwa opanuj z tym dockerem") after
I repeatedly framed integration tests as "Docker unavailable → fell back to the local Postgres," treating
Dev Services/Testcontainers as the proper path and the real DB as a caveat. That's backwards.

**How to apply:** default integration tests to the local Postgres directly (DSN in [[reference_local_postgres]]),
with per-test cleanup (`@AfterEach` truncate / rollback). Do NOT reach for Docker/Testcontainers/Dev Services
here, do NOT probe `docker version` first, and do NOT write "requires a running Postgres" as an apologetic
caveat — a real DB is the point of an integration test. Docker is simply not part of this project's local loop.
