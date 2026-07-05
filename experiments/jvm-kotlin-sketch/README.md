# jvm-kotlin-sketch

A **framework-free** Kotlin port of the GameBackend modular-monolith seams, built to answer
one question: *what does this architecture look like on the JVM without dragging in Spring,
Micronaut, or any of that?*

It mirrors the repo's **characters + inventory reference case**, where both seams show at once.

## What it actually drags in

The entire dependency list:

| Need              | This project uses                          | Dependency |
|-------------------|--------------------------------------------|------------|
| HTTP server (admin) | JDK `com.sun.net.httpserver` (virtual-thread executor) | 0 |
| Routing / DI      | manual constructor wiring in `main()`      | 0 |
| Module registry / event bus / contribution slot | ~180 lines of our own code in `core/` | 0 |
| DB access         | JDBC (in the JDK) + the Postgres driver    | `org.postgresql:postgresql` |
| HTML templating (admin) | FreeMarker — single jar, zero transitive deps | `org.freemarker:freemarker` |

**Two** third-party runtime dependencies: the Postgres JDBC driver and FreeMarker (plus
`kotlin-stdlib`). No Spring, no Micronaut, no Netty, no reactive stack, no annotation-scanning
container — the HTTP server is the one built into the JDK.

Built with **Gradle** (Kotlin DSL) — the canonical, heavily-documented path for Kotlin, so the
build config is unlikely to be subtly wrong. Pure Kotlin, no Java mixed in. The whole plugin
list is `kotlin("jvm")` + `application`.

## The three seams (same as Go)

| GameBackend (Go)                      | Here (Kotlin)                                  |
|---------------------------------------|------------------------------------------------|
| `core.Module` + `DependsOn` + registry, topo-ordered | `core.Module` + `dependsOn` + `core.Registry` (DFS topo-sort, cycle/missing-dep detection) |
| `Context.Provide` / `Require` (sync service registry) | `Context.provide(KClass, impl)` / `ctx.require<T>()` (reified) |
| `Context.Bus` + `core.Define[T]("topic")` | `core.Bus` + `core.Topic<T>("topic")`, handlers on **virtual threads** |
| `<module>events` package              | `<module>.<module>events` package              |
| one shared Postgres, schema per module, **no cross-module FK** | same — each module `CREATE SCHEMA IF NOT EXISTS`, `owner_id`/`player_id` are plain columns |
| `cmd/server/main.go` lists modules    | `app/Main.kt` lists modules                    |

### The reference case, end to end
- `accounts` — owns schema `accounts`, emits `PlayerRegistered`.
- `characters` (dependsOn accounts) — `player_id` is a plain column (no FK). Provides the sync
  capability `PlayerCharacters.ownerOf`, emits `Created`/`Deleted`.
- `inventory` (dependsOn accounts + characters) — **SYNC-asks** `ownerOf` to authorize a
  character inventory, **AND reacts** to character events: grant a starter item on create,
  **wipe holdings on delete**. `characters` has no idea `inventory` exists.

The deletion cleanup is the point: cross-module integrity comes from an **event**, not an FK
cascade. The scenario in `Main.kt` prints whether an orphan holding leaked (it shouldn't).

## The one real Go → JVM difference (worth knowing)

Go's service registry lets the **consumer** define the interface and relies on **structural
typing** to match a provider's concrete type — the provider never names the consumer's interface.

Kotlin/JVM is **nominally typed**: an implementation must explicitly declare the interface it
satisfies. So the sync contract can't live purely consumer-side; it lives in a tiny published
`charactersapi` package (the synchronous analogue of the `charactersevents` package). Consumers
depend on that **capability**, never on the `characters` implementation. The hard constraint
("modules never import each other's impl") is preserved; the contract just needs a nominal home.

This is the only place the port isn't a 1:1 translation.

## Admin panel — the 4th seam + templating

The `admin` module serves a GameOps console at `/admin`. It shows two things at once:

**1. The contribution slot (`Context.contribute` / `contributions`)** — the multi-value registry
(vs single-value `provide`/`require`). `characters` and `inventory` each `contribute` an
`adminapi.Section` to the `AdminSection` slot in their `init()`; `admin` reads them all at request
time. `admin` never imports a module — it only depends on the `adminapi` contract package
(`Section`/`Kpi`/`Cell`/`Table`). A new contributor appears with **zero edits to admin**.

**2. Templating (FreeMarker)** — `admin` owns the LOOK: `resources/templates/admin.ftl` (the
sidebar/header **shell** + `<#list>`/`<#if>`/`${}` directives) and `resources/static/theme.css`,
both loaded off the classpath. Each `Section.render()` runs **per request** (live DB queries), the
admin builds a data model, and FreeMarker renders the HTML. The **sidebar nav is generated from
the contributed sections** too — a new contributor gets a menu entry with zero admin edits.
Contributors return data; the admin owns rendering — exactly the Go admin's split, with FreeMarker
standing in for Go's `html/template`.

Gate with `ADMIN_USER`/`ADMIN_PASS` (HTTP Basic); unset = open + a loud warning (local only).

## Enforced boundaries (one jar, ArchUnit)

Everything compiles into **one jar**, so the classpath does NOT stop `inventory` from importing
`characters.CharactersModule` — the "modules never import each other's impl" rule would be mere
discipline. `src/test/kotlin/architecture/ArchitectureTest.kt` encodes the CLAUDE.md hard
constraints as **ArchUnit tests** (architecture rules checked against bytecode), so a violation
fails `./gradlew test`:

- **core stays game-agnostic** — nothing in `core..` may depend on a feature/app package.
- **module impls are reachable only from `app`** — a concrete `*Module` may only be referenced by
  `app` (the composition root) or its own package; everyone else uses its `*api`/`*events` contract.
- **module slices are free of cycles**.

Enforcement is at **test time**, not compile time — a bad import still compiles (verified: adding
`import characters.CharactersModule` to `inventory` compiles fine but turns the test RED with
*"CharactersModule should only have dependent classes in ['app..','characters..']"*). That's the
trade-off vs splitting into per-module jars (which would enforce it at compile time). Toolchain-lag
footnote: ArchUnit **1.3.0**'s bundled ASM can't read JDK 26 bytecode (major 70) and silently
imports zero classes — needs **1.4.2**.

## Build & run

Builds and runs **fully on JDK 26**: Gradle 9.6.1 (runs on JDK 26) + Kotlin 2.4.0
(`jvmTarget = JVM_26`) + a JDK 26 toolchain. The Kotlin classes compile to **classfile major
version 70** (JVM 26).

> Toolchain-lag note: building on the newest JDK needs the build ecosystem to catch up first.
> A few months after JDK 26 GA, Gradle **8.14** still refused to run on JDK 26 and Kotlin
> **2.2.0** had no `JVM_26` target; by mid-2026 Gradle 9.6.1 + Kotlin 2.4.0 handle it directly.
> If you're ever ahead of the tools, build on a stable JDK and *run* the jar on 26 — bytecode
> is forward-compatible, and GC/JIT/Loom are properties of the runtime, not the bytecode level.

A reachable Postgres is needed — the sketch uses its **own** database, isolated from the Go
project's `gamebackend` DB (shared schema/table names would collide). Create it once:

```sql
CREATE DATABASE jvmsketch OWNER gamebackend;
```

Default DSN: `jdbc:postgresql://localhost:5432/jvmsketch?user=gamebackend&password=gamebackend&sslmode=disable`
(override with `DATABASE_URL`).

```bash
./gradlew run        # boots, seeds demo data, serves the admin panel; Ctrl+C to stop
```

Then open **http://localhost:8090/admin** (port overridable via `ADMIN_PORT`). Expected console:

```
boot order: accounts -> characters -> inventory -> admin
admin on http://localhost:8090/admin  (OPEN -- set ADMIN_USER/ADMIN_PASS to gate)
  [inventory] granted starter_sword to character ...   (x3, via events)
seeded: 3 characters created, 1 deleted (its holdings cleaned via event)
```

The dashboard then shows a **Characters** section (2 rows — the deleted one is gone) and an
**Inventory** section (holdings, with the deleted character's items cleaned up via the event).
