---
name: reference-local-postgres
description: "Local Postgres connection details for GameBackend (role, db, DSN) + tests target it directly, never Docker/Testcontainers; per-OS psql path lives in platform-notes"
metadata: 
  node_type: memory
  type: reference
  originSessionId: 88cdd953-b406-40a0-8ab2-6c7eb07acece
  modified: 2026-07-18T07:54:54.221Z
---

Local PostgreSQL 18 on `localhost:5432`, on whichever machine is primary (the port
is now developed on all three platforms; **macOS is the current primary box** since
the 2026-07-17 darwin port).

**GameBackend project DB (use this):**
- Role `gamebackend`, password `gamebackend`, database `gamebackend` (owned by the role).
- `DATABASE_URL=postgres://gamebackend:gamebackend@localhost:5432/gamebackend?sslmode=disable`
- **The per-OS `psql` invocation (Homebrew macOS / Windows installer / Linux) lives
  in [platform notes](docs/reference/platform-notes.md), not here — read it there so
  this memory never drifts per machine.** On the macOS box Homebrew keeps `psql` off
  `PATH` at `/opt/homebrew/opt/postgresql@18/bin/psql`.

**Local auth is `trust` (macOS Homebrew box):** `pg_hba.conf` trusts local
connections, so `PGPASSWORD` is NOT checked on connect — a wrong/blank password still
connects. This is why a "the DSN password failed" claim is unobservable here (the
lesson in [[scope-claims-to-what-was-verified]]); prove password-layer changes at the
storage layer (`pg_authid.rolpassword`), not by a connect attempt. Superuser
provisioning uses the OS superuser role under trust; the old Windows `postgres`/`qwerty`
superuser was that machine's setup, not this one.

**Integration tests target THIS local Postgres directly — not a Docker/Testcontainers
fallback.** The user pushed back sharply ("weź się kurwa opanuj z tym dockerem") on framing
the real DB as a compromise: a real DB is the point of an integration test. Don't probe
`docker version`, don't write "requires a running Postgres" as an apologetic caveat, use
per-test cleanup (truncate/rollback). Full logical isolation: each module owns a schema, no
cross-module FKs.
