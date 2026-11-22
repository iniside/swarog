---
name: demoseed-client-simulator-decisions
description: "Owner 2026-11-22: demoseed becomes a concurrent client simulator over every public gateway op; a new plan replaces admin-redesign Step 24b"
metadata:
  node_type: memory
  type: project
  originSessionId: 7420721d-9d24-4215-a860-f8978a9d6d23
  modified: 2026-11-22T11:28:41.113Z
---

Owner decisions (2026-11-22) for widening demoseed into a client simulator that both seeds and tests the backend through the gateway's public API:

1. A new standalone plan replaces Step 24b of the admin-control-plane-redesign plan.
2. Errors do not stop the run: it continues and reports every error at the end. Coverage self-check against `opscatalog::OPERATIONS`: a public op no scenario calls fails the run.
3. Destructive ops (account delete, character/storage/notification delete, friend remove/decline, group leave, party kick/leave) run on throwaway players; the seeded data stays.
4. Modules with no public route (mail, config, apikeys, rating, scheduler, audit) fill only through events; no new public routes in this plan.
5. Monolith only for now; manual run, not wired into verifyctl or devctl.
6. Many clients in parallel, as far as the gateway allows.

**Why:** owner: "zeby demo seed byl w stanie robic takie requesty do kazdego modulu tak jak robiloby to klient, dzieki temu bedziemy mogli caly backend przetestowac i jednoczesnie zaseedowac".
**How to apply:** keep public-API-only ([[demoseed-is-the-one-seeder]]); do not add split runs or a verify stage without asking.
