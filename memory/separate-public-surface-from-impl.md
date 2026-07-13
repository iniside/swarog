---
name: separate-public-surface-from-impl
description: "A module's public/consumed surface must be structurally separate from its impl folder; and when multiplying a convention, stop and question the convention itself"
metadata: 
  node_type: memory
  type: feedback
  originSessionId: 88cdd953-b406-40a0-8ab2-6c7eb07acece
---

I placed every public package of each module (`<name>api`, `<name>events`, the rpc glue)
INSIDE `modules/<name>/`, so a consumer imported `modules/characters/charactersapi` —
textually reaching into another module's private folder. The user caught it on a glance; I'd
entrenched it across a whole program (I even wrote the plan codifying
`modules/<name>/<name>api/`) without once questioning the layout. (Resolved: the public
surface now lives in a top-level `api/<name>/` tree next to `modules/<name>/` — the current
layout is documented in CLAUDE.md.)

**The two lessons (why this stays):**
1. A module's surface that OTHER modules consume belongs where it READS as public (a separate
   top-level area), not nested in the impl folder — otherwise "depend on a capability, not a
   package" is undermined when depending *looks* like reaching into the provider's guts.
2. When you catch yourself creating the Nth instance of a convention (here: the Nth
   co-located sub-package), STOP and question the convention itself before multiplying it.
   Don't extend a smell because a precedent exists.

See [[scope-claims-to-what-was-verified]].
