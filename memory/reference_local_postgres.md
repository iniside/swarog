---
name: reference-local-postgres
description: "Local Postgres connection details for GameBackend (role, db, DSN, superuser) + tests target it directly, never Docker/Testcontainers"
metadata: 
  node_type: memory
  type: reference
  originSessionId: 88cdd953-b406-40a0-8ab2-6c7eb07acece
---

Local PostgreSQL 18 on `localhost:5432` (binaries at `C:\Program Files\PostgreSQL\18\bin`).

**GameBackend project DB (use this):**
- Role `gamebackend`, password `gamebackend`, database `gamebackend` (owned by the role).
- `DATABASE_URL=postgres://gamebackend:gamebackend@localhost:5432/gamebackend?sslmode=disable`
- Connect (bash): `PGPASSWORD=gamebackend "/c/Program Files/PostgreSQL/18/bin/psql.exe" -U gamebackend -h localhost -d gamebackend`

**Superuser** (only for admin/provisioning): user `postgres`, password `qwerty` — set in
another local project; default `postgres`/`postgres` does NOT work here. (This cred is
deliberately NOT committed to CLAUDE.md.)

**Integration tests target THIS local Postgres directly — not a Docker/Testcontainers
fallback.** The user pushed back sharply ("weź się kurwa opanuj z tym dockerem") on framing
the real DB as a compromise: a real DB is the point of an integration test. Don't probe
`docker version`, don't write "requires a running Postgres" as an apologetic caveat, use
per-test cleanup (truncate/rollback). Full logical isolation: each module owns a schema, no
cross-module FKs.
