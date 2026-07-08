---
name: dont-descope-transport-for-simplicity
description: "Never descope the gateway / durable transport / RPC codegen to \"minimum viable\" or \"simpler\" — it repeatedly produces per-module hacks and tech debt; port the full seam"
metadata: 
  node_type: memory
  type: feedback
  originSessionId: df367cfa-2fb8-48f2-aac5-11559e2ce7f6
---

When scoping ports/plans, do NOT cut the gateway, the durable event plane, or the RPC codegen (`rpcgen`/`#[rpc]` proc-macro) in the name of a "minimum honest split" or "hand-write it, it's simpler." User caught me (2026-07-08) accepting a reviewer's descope of gateway + proc-gen RPC to a later milestone and rejected it flatly: "ostatnio bez niego najebałeś hacków po modułach" (last time without the gateway I scattered hacks across modules), and hand-written glue = "znowu napierdolisz długu bo prościej" (you'll pile up debt again 'because simpler').

**Why:** The full transport (gateway front-door + auth-once + op-routing, durable messaging, generated RPC glue) IS the "split-from-start, no hacks" directive — not gold-plating. Descoping it pushes per-module HTTP shims and topology branches that violate [[never-monolith-only-features]] and accrue debt. Codegen prevents the copy-paste glue drift that hand-writing invites.

**How to apply:** Keep gateway + durable messaging + RPC codegen IN the first milestone of any port/rebuild. Accept the larger upfront cost. A reviewer recommending "descope for minimum viable" is optimizing the wrong metric here — push back. Genuine technical blocker fixes (compile-correctness, ordering, mTLS rigor) are still worth taking; only the scope-cutting is wrong. Relates to [[scope-claims-to-what-was-verified]], [[verify-the-at-risk-path-not-the-safe-one]].
