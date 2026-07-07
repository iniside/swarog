---
name: decision-quarkus-as-final
description: "Go-vs-Kotlin direction: back in flux (2026-07-07), now LEANING GO again — the 2026-07-05 'Quarkus is final' is NO LONGER firm; do not assert either as settled"
metadata: 
  node_type: memory
  type: project
  originSessionId: 2dde7081-732d-49f5-b0aa-ce19637ba5f1
---

**CURRENT (2026-07-07): the Go-vs-Kotlin decision is UNSETTLED again and the user is LEANING BACK TO
GO.** After walking the admin-panel composition in both ports, the user's tentative verdict ("chyba
zostaniemy przy go"): Go fits the modular-monolith + split goal better, *despite* disliking the
language ("strasznie chujowy"). Concrete reasons that swung it, all surfaced this session:
- **Admin stays list-free even for remotes in Go** — the runtime slot (`Contributions`) lets
  `remote.Stub` append in `Init`; Quarkus's closed-world ArC can't add runtime-config beans to
  `@All`, so it needs an `admin.modules` config list the admin itself reads (the "admin knows what
  should be registered" smell). See [[go-parity-additive-dual-deploy]].
- **Split-build falls out of the Go linker for free** — each `cmd/<svc>` imports only its modules, Go
  drops the rest; Quarkus is one fat artifact gated by `ROLES`.
- Structural typing makes the cross-process seam (`remote.*Client` satisfies the consumer iface) clean
  without impl imports.
Language gripe acknowledged as real (verbose `if err != nil`, no sum-types, `nil`) but judged the price
for the explicitness that makes the split work. NOT a hard commit yet ("chyba") — treat as the current
lean, not a locked decision. This DOWNGRADES the 2026-07-05 "Quarkus is final" verdict below.

---

**Superseded verdict (2026-07-05): Kotlin + Quarkus as FINAL/main backend.** Kept for history — it is
NO LONGER the standing direction (see CURRENT above). At the time it superseded the Go-leaning parity
verdict after a second Opus reviewer called continue-Quarkus and the user adopted it.

**State of the Quarkus sketch when this was decided (branch `quarkus-per-service`, off
`quarkus-dual-deploy`, NOT merged to master):**
- Full dual-deploy: monolith (`app` fast-jar) OR real per-service split (`characters-service` +
  `inventory-service`) via `install.ps1 -Mode microservices`. Per-service = REAL separate fast-jars,
  each links only its own modules (proven: `inventory-service/build/quarkus-app/lib/main/` has
  `characters-client` + contract jars but NO `characters.jar`/`accounts.jar` impl; `characters-service`
  has NO `inventory.jar`/`admin.jar`/`characters-client.jar`). CDI crux: remote `PlayerCharacters`
  producer extracted to a `characters-client` module (imports `characters-api`+`edge`, never the
  characters impl); bean PRESENCE decides local-vs-remote, exactly one producer per topology.
- Broker-less async (outbox HTTP fanout → inventory sink), sync ownerOf over native QUIC (msquic/FFM),
  admin fan-out over REST/Stork. All VERIFIED LIVE on the split (char create → starter grant across two
  jars; ownerOf owned 200 / unowned 400; admin fan-out; admin degrades to error-card when A down).
- **Fixed live (commit `32dc8d6`):** the remote ownerOf seam conflated provider-down with "not owned"
  (both → 400 — a silent failure). Widened like Go: `CharactersUnavailableException` in `characters-api`,
  thrown by `characters-client` on transport failure, mapped to **503** in `InventoryResource` (400 stays
  for genuinely-not-owned). Verified: A killed → owned grant now 503, not 400. Sketch now matches Go's
  failure-mode behavior.

**Build-time verification ladder added (2026-07-05, branch `quarkus-per-service`, commits `3ec5722`→
`fe7faae`):** 4 layers pushing wiring/architecture checks from runtime→build. L0 strict compile+ArC flags
(but `transform-unproxyable-classes=false`/`strict-compatibility=true` are UNUSABLE with this codebase's
constructor-injection DI — left commented). **L1 = the real win**: a Gradle task verifying the per-service
split on the RESOLVED classpath (transitive-safe) + admin parity — the split was previously only a
hand-checked comment. L2 Konsist source rules (works under JDK26). L3 custom Quarkus extension (Java build
steps + standing `QuarkusUnitTest` negative fixture) = demo-value only (ArC pre-empts most; one net-new
check). Full write-up: `docs/reference/quarkus-compile-time-verification.md`; plan
`docs/plans/2026-07-05-2249-quarkus-aggressive-build-verification-plan.md`. Ceiling: no Go/IDE-level
compile-time DI without leaving Quarkus for a compile-time-DI framework.

Open follow-on if promoting out of `experiments/`: pick the branch to become main, decide monolith-default
vs split, and carry the 503 seam-widening pattern to any other sync seam added later.
