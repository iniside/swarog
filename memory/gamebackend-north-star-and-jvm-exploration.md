---
name: gamebackend-north-star-and-jvm-exploration
description: The two architectural north-star goals of GameBackend (pluggable + mechanically extractable to microservices) and why in-process plugin systems fight goal 2
metadata: 
  node_type: memory
  type: project
  originSessionId: 88cdd953-b406-40a0-8ab2-6c7eb07acece
---

GameBackend's modular monolith serves **two stated goals**: (1) anyone can extend it by
writing plugins; (2) plugins should be relatively easy to convert into independent
**microservices** once traffic reaches ~2M/hour. ~2M/h ≈ 556 rps avg — an
**isolation/ownership/independent-deploy trigger, NOT a throughput wall** (any runtime handles
100× that on one box).

The seams (async event bus, sync service registry, per-module schema, **no cross-module FK**)
make goal 2 a near-mechanical extraction: bus → broker, service interface → RPC, module schema
→ own DB. **Tension to remember:** powerful *in-process* plugin systems (JVM classloaders /
OSGi) actively fight goal 2 — they tempt direct cross-plugin calls that can't be cut along a
network boundary. The coherent fit for BOTH goals is plugins that talk **only via bus +
interface (network-shaped) from day one**; at the limit, out-of-process plugins over a wire
protocol collapse goal 1 and goal 2 into one boundary.

(Historical: the goals were first explored in a framework-free Kotlin/JDK26 sketch at
`experiments/jvm-kotlin-sketch/` before the Rust migration — [[decision-migrate-everything-to-rust]].
The one durable cross-language lesson: Go's structural typing lets the *consumer* define the
service interface, whereas Rust/Kotlin are nominal, so the sync contract must live in a
published `<name>api` crate — the sync analogue of `<name>events`.) See
[[store-launch-auth-deferred-to-sdk]].
