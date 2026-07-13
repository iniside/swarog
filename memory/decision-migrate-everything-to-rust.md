---
name: decision-migrate-everything-to-rust
description: "DECIDED + DONE 2026-07-08 — the whole backend is Rust; supersedes the Quarkus/Go/JVM deliberation. Don't re-litigate or assert them as the target."
metadata: 
  node_type: memory
  type: project
  originSessionId: 88cdd953-b406-40a0-8ab2-6c7eb07acece
---

**DECIDED and COMPLETED 2026-07-08: everything is Rust.** The full Go→Rust port finished
the same day (plan `docs/plans/2026-07-08-1517-go-to-rust-full-port-plan.md`, 15 steps:
fortress refactor + all modules + core infra + tiered verify net). The Rust workspace at
repo root is the ONLY developed project; `experiments/go-sketch/` + `jvm-*-sketch/` are
archived references (kept, not deleted — "do not evolve"). Do NOT re-open Go-vs-Rust or
assert Quarkus/Go/JVM as the target.

**Why Rust won:** for this project's profile (split-from-start, agent-written, correctness
over iteration speed) it's equal-or-better. The rule-4 cost (nominal traits → consumers
import the provider's contract crate) is bounded and clean; build speed was a non-issue
(~7s); crate-per-module gives compiler-enforced physical separation that serves the
"extractable to microservices" north star ([[gamebackend-north-star-and-jvm-exploration]])
better than Go's lint-enforced boundaries.

What's built is now the living truth in CLAUDE.md + the code — this memory is the DECISION,
not the spec. Keep the split-first, no-hacks discipline
([[dont-descope-transport-for-simplicity]], [[never-monolith-only-features]]).
