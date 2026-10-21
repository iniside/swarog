---
name: add-game-module
description: Full rollout of a new domain module (fortress) in this repo — from seam classification through contracts, module impl, cmd/<name>-svc, monolith registration, stubs, admin/ops, to split-proof assertions. Use whenever adding a new domain/feature module, a new capability another module will consume, a new durable event flow, or when the user says "dodaj moduł", "new module", "add <domain> to the backend". Also use when EXTENDING a module with a new capability/event/player operation — the relevant sections apply. Prevents the classic failure: a feature that accidentally ships monolith-only.
---

Read and follow the canonical skill at `../../../.agents/skills/add-game-module/SKILL.md`. Do not maintain a second copy.
