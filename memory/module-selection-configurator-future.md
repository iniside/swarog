---
name: module-selection-configurator-future
description: "FUTURE (wanted 2026-11-11, not planned): installer/compose configurator where the user ticks modules, deps are resolved automatically, and the result compiles (or downloads prebuilt binaries) and runs"
metadata:
  node_type: memory
  type: project
  originSessionId: fb0529fe-4b60-43b5-a11a-9d60ec9ddba8
  modified: 2026-11-11T16:59:35.525Z
---

Lukasz wants, LATER (after Docker/compose support, [[docker-is-distribution-and-multihost-bench]]),
a module-selection configurator as part of the installer or the compose path:
- the user ticks which modules they want — UI either a console menu in the style of
  Linux kernel `make menuconfig`, or a web UI (undecided);
- dependencies between modules are resolved/checked AUTOMATICALLY (ticking X pulls in
  or demands what X requires; an unsatisfiable set is refused up front);
- the selected set then either compiles or downloads prebuilt binaries, and runs;
- must be dead simple for the user: tick options → it works.

**Why:** prospective repo users expect to assemble "only what I need" without knowing
the module graph — same motive as compose support.

Facts to start from when this is planned (verify first, these were true 2026-11-11):
- modules declare deps as a manifest (`requires()`, `core/lifecycle`); a process whose
  module set lacks a capability fails at STARTUP — the configurator needs that graph
  at selection time, BEFORE build, so it must come from a statically readable source
  (the existing `tools/checkmodules` links cmd/server as a library — a possible lead).
- composition roots `cmd/*` are hand-written module lists; a selected monolith needs
  a generated root or cargo features — must not become config-file magic
  ([[config-as-code-anti-magic]]).
- in the SPLIT, picking modules ≈ picking which prebuilt `*-svc` binaries to start —
  prebuilt download maps naturally there; the monolith subset is what needs compiling.

**How to apply:** don't start this before compose lands; when planning it, keep both
topologies supported ([[never-monolith-only-features]]).
