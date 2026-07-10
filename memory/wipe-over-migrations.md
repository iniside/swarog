---
name: wipe-over-migrations
description: DB wipes are acceptable — no data-migration machinery in this phase; seed scripts with fake data replace migrations when dev data is needed
metadata: 
  node_type: memory
  type: project
  originSessionId: e19a37a3-1d37-4547-a1dc-cacff9969c71
---

Decision (Lukasz, 2026-07-10, stated during the event-log v2 rollout and then
generalized): the project is local with no production data, so **wiping tables /
schemas / the whole DB is 100% acceptable**. When a schema or contract change
would need a data migration, drop and boot fresh.

**Why:** migration machinery (bridges, dual-writes, backfills, cutover
choreography) is high-risk throwaway code with zero durable value here — the
2202 event-log plan carried ~40% such machinery and was deliberately rewritten
without it (fresh-start plan `docs/plans/2026-07-09-2234-durable-event-log-fresh-plan.md`).

**How to apply:** never propose data-migration steps, versioned data migrations,
or zero-downtime cutover protocols for this repo; module `migrate` = idempotent
DDL only. If dev data is needed after a wipe, write/extend a **seed script**
minting fake data (pattern: `APIKEYS_DEV_SEED` self-healing upsert). Documented
in CLAUDE.md ("Database"). Revisit only if real persistent users appear. See
[[durable-event-plane-bus-owned]].
