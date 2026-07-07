---
name: reference-local-postgres
description: "Local Postgres connection details for the GameBackend project (role, db, DSN, superuser)"
metadata: 
  node_type: memory
  type: reference
  originSessionId: 177a4d5a-9a36-469c-9744-6d6a166e60b1
---

Local PostgreSQL 18 on `localhost:5432` (binaries at `C:\Program Files\PostgreSQL\18\bin`).

**GameBackend project DB (use this):**
- Role `gamebackend`, password `gamebackend`, database `gamebackend` (owned by the role).
- `DATABASE_URL=postgres://gamebackend:gamebackend@localhost:5432/gamebackend?sslmode=disable`
- Connect (bash): `PGPASSWORD=gamebackend "/c/Program Files/PostgreSQL/18/bin/psql.exe" -U gamebackend -h localhost -d gamebackend`

**Superuser** (only for admin/provisioning): user `postgres`, password `qwerty`.
- This password was set in another local project; the default `postgres`/`postgres` does NOT work here.

Full logical isolation: each module owns a schema in this single shared db, no
cross-module foreign keys. See the project's README for the rule.
