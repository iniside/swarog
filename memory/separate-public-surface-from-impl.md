---
name: separate-public-surface-from-impl
description: "A module's public/consumed-by-others surface must be structurally separate from its impl folder; and when multiplying a convention, stop and question the convention itself"
metadata: 
  node_type: memory
  type: feedback
  originSessionId: 9daf9937-49a2-46ca-88f2-a2c9a48ebd40
---

I placed every public package of each module — `<name>api` (contract), `<name>events`,
and the generated rpc glue (`<name>playerrpc`/`<name>adminrpc`/`<name>rpc`) — INSIDE
`modules/<name>/`. So a consumer imports `modules/characters/charactersapi` — textually
reaching into another module's private folder. The user caught it on a glance; I'd
entrenched it across the whole unified-operation-transport program (I even wrote the plan
codifying `modules/<name>/<name>api/`) without once questioning the layout.

**Why it's wrong:** co-locating the public contract inside the impl folder erases the
public/private boundary. "Depend on a capability, not a package" (rule 4) is undermined
when depending on the capability *looks* like reaching into the provider's guts. And I
extended a 1-package convention (`<module>events`) into 3–4 packages per module on
autopilot — the multiplication was the signal to stop and re-question, and I missed it.

**How to apply:**
1. A module's surface that OTHER modules consume (contract interfaces, events, and the
   glue clients other processes call) belongs in a location that READS as public —
   a separate top-level area / standalone `*api` modules — not nested in the impl folder.
2. When you find yourself creating the Nth instance of a convention (here: the Nth
   co-located sub-package), STOP and question the convention itself before multiplying it.
   Don't extend a smell just because a precedent exists.
3. This is a layout the user wants restructured. Map it (Research-before-planning) before
   proposing: separate truly-public (api/events, consumed cross-module) from private
   impl + generated server-side glue. See [[unified-operation-transport]],
   [[scope-claims-to-what-was-verified]].
