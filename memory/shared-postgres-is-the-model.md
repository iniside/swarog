---
name: shared-postgres-is-the-model
description: "One shared Postgres on a powerful bare-metal box is the decided persistence/ops model — not a stepping stone to DB-per-service"
metadata:
  node_type: memory
  type: project
  originSessionId: 88cdd953-b406-40a0-8ab2-6c7eb07acece
---

**DECIDED (2026-08-12, Lukasz).** Persistence is **one shared Postgres**, schema-per-module,
no cross-module FKs. This is the product/ops model, not an incomplete extraction.

**Why:** the value of this backend is that you can deploy on **bare metal** and run
**one Postgres on a powerful machine**. Logical isolation (schema per fortress, plain
id columns, durable events / sync capabilities for cross-module relations) is the
boundary. Splitting to DB-per-module, a broker instead of the Postgres log, or
Docker/Testcontainers "because that's how microservices scale" is a misread.

**What goal 2 actually extracts:** a module to its own `cmd/<name>-svc` process
(independent deploy, isolation, ownership). The registry swap and the shared event
log already make that cut. It does **not** extract the database.

**Do not:** treat shared Postgres as a gap, propose DB-per-service as the next
maturity step, or re-open "bus → broker / schema → own DB" from the original
north-star sentence (corrected in [[gamebackend-north-star-and-jvm-exploration]]).
Wipe-is-migration stays the current-phase schema strategy; that is separate from
this ops decision.

See CLAUDE.md hard constraint 10, [[reference_local_postgres]],
[[mini-orchestrator-native-no-containers]].
