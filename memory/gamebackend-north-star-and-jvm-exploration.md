---
name: gamebackend-north-star-and-jvm-exploration
description: The two architectural north-star goals of GameBackend + the framework-free Kotlin/JDK26 port exploration
metadata: 
  node_type: memory
  type: project
  originSessionId: eb8d3819-1e67-4a42-9058-589f90144fc1
---

GameBackend's modular monolith exists to serve **two stated goals**: (1) anyone can
extend it by writing plugins; (2) plugins should be relatively easy to convert into
independent **microservices** once traffic reaches ~2M/hour. Note: ~2M/h ≈ 556 rps avg —
an **isolation/ownership/independent-deploy trigger, NOT a throughput wall** (any of
Go/JVM/.NET handles 100× that on one box).

The existing Go seams (async event bus, sync service registry, per-module schema, **no
cross-module FK**) already make goal 2 a near-mechanical extraction: bus → message broker,
service interface → RPC, module schema → own DB. **Tension to remember:** powerful
*in-process* plugin systems (JVM classloaders / OSGi) actively fight goal 2, because they
tempt direct cross-plugin calls that can't be cut along a network boundary. The coherent
fit for BOTH goals is plugins that talk **only via bus + interface (network-shaped) from
day one**; at the limit, **out-of-process plugins over a wire protocol** (HashiCorp/gRPC
style) collapse goal 1 and goal 2 into a single boundary and are language-agnostic.

The user is exploring a **JVM** option mainly because two Go projects already exist and he
wanted to see the architecture in a JVM language that does NOT drag in Spring/Micronaut.
Not historically a Java fan (overengineering: build files, patterns, boilerplate) — but
since Claude writes the code, human-ergonomics weigh less, so **chose Kotlin over Java**.

A framework-free **Kotlin / JDK 26** port of the characters+inventory reference case lives
at `experiments/jvm-kotlin-sketch/` — proves the 3 seams translate ~1:1 with **one runtime
dependency** (Postgres JDBC driver). DECISION: staying with **Kotlin, pure, no Java
mixed in**, built with **Gradle** (Kotlin DSL). Briefly trialled Maven then reverted to Gradle —
rationale: Gradle+Kotlin is the canonical, heavily-documented path, so config is less likely to
be subtly wrong (matters since Claude writes it); Maven makes Kotlin a second-class citizen.
Builds AND runs fully on JDK 26 (Gradle 9.6.1 + Kotlin 2.4.0, `jvmTarget = JVM_26`, classfile
major 70; wrapper pinned to 9.6.1). Now also has an **admin panel** (`admin` module) serving
`/admin` (default :8090) on the JDK's built-in `com.sun.net.httpserver`, demonstrating the **4th
seam** (`Context.contribute`/`contributions` multi-value slot — characters + inventory each
contribute an `adminapi.Section`; admin reads them without importing them) and **templating** via
**FreeMarker** (single jar, zero transitive deps; `resources/templates/admin.ftl` + `static/theme.css`).
The admin theme/shell is translated **verbatim from `UILayout/GameOps Admin.dc.html`** (Public Sans +
IBM Plex Mono, the exact GameOps palette, 256px sidebar + 64px header chrome) — see
[[follow-uilayout-mockup-faithfully]].
So the sketch now has 2 third-party deps: postgres driver + freemarker. `gradle run` boots, seeds
demo data, serves until Ctrl+C.

**Boundary enforcement in ONE jar**: `src/test/kotlin/architecture/ArchitectureTest.kt` uses
**ArchUnit** (test scope) to encode the CLAUDE.md hard constraints as tests — core stays
game-agnostic, concrete `*Module` impls are reachable only from `app` (composition root) or their
own package, module slices are cycle-free. Enforcement is at TEST time (a bad
`import characters.CharactersModule` in inventory still COMPILES in a single jar, but turns
`gradle test` red) — the alternative for compile-time enforcement is the per-module jar split
(api/impl separate subprojects), which the user understands but deferred. Toolchain-lag again:
ArchUnit 1.3.0's ASM can't read JDK 26 bytecode (major 70) → imports zero classes silently; needs
**1.4.2**. Discussed but NOT built: pluggable Bus transport (in-process vs broker) as the concrete
goal-#2 demonstration.

The admin **navigable contribution model** (Section-as-panel → Item{section,label,render} grouped
into a sidebar, `/admin/<slug>` per item, COMING SOON block for unbuilt mockup items) was built in
the Go project (merged to master) AND mirrored to the Kotlin sketch — both verified via curl. Go
took the full ceremony (plan doc + 3 research subagents + grumpy reviewer + per-lane subagent
commits); the Kotlin mirror was done inline (faster, but hit the MSYS `/g/` vs Windows `G:/`
classpath trap running `java -cp`). Still undecided Go-vs-Kotlin for "fun".

**Back-port to the real Go project**: the one idea worth taking from the JVM detour was enforced
module boundaries. Added **`go-arch-lint`** (`.go-arch-lint.yml` at repo root) to the main Go
project — machine-checks: core imports no module, a module's impl is reachable only from `cmd`,
modules talk only via `<module>events`/`adminapi` contracts. (Cycles are free in Go — compiler
rejects them.) Verified green; the existing code already obeyed every rule. Chosen over depguard
(more widely used via golangci-lint, but repo has no golangci-lint so that edge didn't apply;
go-arch-lint is the purpose-built declarative fit). Run `go-arch-lint check`; documented in
CLAUDE.md Commands + `docs/reference/architecture-enforcement.md`. Decision stands: **Go for the
product, Kotlin sketch kept as portability proof / future JVM-plugin-host seed.**

Also added **golangci-lint v2** (`.golangci.yml`) as the correctness/leak/security gate (the user's
"I don't trust you" guardrail for AI-written code) — curated high-signal set (errcheck, staticcheck,
gosec, bodyclose, sqlclosecheck, rowserrcheck, errorlint, exhaustive), NOT a style gate. First run
found a REAL issue: G112 Slowloris (production http.Server missing ReadHeaderTimeout) — FIXED
(added `ReadHeaderTimeout: 10*time.Second`). Tuned 2 gosec false-positives (G101 local test DSN,
G115 argon2 length). Remaining at last check: 15 errcheck (mostly idiomatic unchecked
`defer rows.Close()`/`tx.Rollback()` + a few HTTP `w.Write`/`json.Encode`) — proposed an explicit
`_ =` sweep to green, pending user go-ahead. golangci-lint 2.12.2 built with go1.26.1 — no
toolchain-lag this time. Run with
`mvn -q compile exec:java`. Toolchain-lag lesson confirmed empirically: a few months post-GA,
Gradle 8.14 refused to run on JDK 26 and Kotlin 2.2.0 had no `JVM_26` target — building on the
newest JDK needs the build tools to ship support first. It uses its own DB `jvmsketch`
(isolated from the Go `gamebackend` DB to avoid schema/table collisions).

A plain-Java twin was built briefly for a side-by-side language comparison then DELETED once the
user settled on Kotlin (concrete delta observed at the time: Java 20 files / 652 lines vs Kotlin
13 / 553; the loud gap was Java's one-public-type-per-file rule, the real friction checked
exceptions — `SQLException` try/catch around every JDBC call; `record`s + `var` closed most of the
rest). Kotlin is the chosen language. **The one real Go→JVM difference:** Go's structural typing
lets the *consumer* define the service interface; Kotlin is **nominal**, so the sync
contract must live in a published `<module>api` package (the sync analogue of
`<module>events`). See [[store-launch-auth-deferred-to-sdk]] for the related accounts model.
