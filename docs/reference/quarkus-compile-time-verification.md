# Quarkus build-time verification — what's achievable, and what it actually buys

Durable reference from the "as aggressive as possible" build-time-verification pass on
`experiments/jvm-quarkus-sketch/` (branch `quarkus-per-service`, 2026-07-05). Answers: *how far can you push
a Kotlin+Quarkus app toward Go-style compile-time wiring checks, and where's the honest ceiling?*

## The core correction

Quarkus already moves most wiring validation to **augmentation** (`quarkusBuild`, a Gradle task after
`compileKotlin`) — not runtime. ArC (its CDI) fails augmentation on **unsatisfied** and **ambiguous**
injection points. So the gap vs Go is narrower than "compile vs runtime": it's `kotlinc`/IDE vs a Gradle task
one phase later. No red squiggle, but `./gradlew build`/CI catches it. You cannot get IDE-level DI checking in
Quarkus without leaving its model for Micronaut/Dagger/kotlin-inject (compile-time DI via annotation
processors) — and KSP conflicts with `quarkusGenerateCode` historically (task cycle, fixed in 3.26.2).

## The verification ladder (implemented, all four coexist green)

| Layer | Mechanism | Fails at | Commit | Honest value |
|---|---|---|---|---|
| 0 | ArC + kotlinc strictness flags | `compileKotlin` / augmentation | `3ec5722` | real (canaries) — but 2 of 4 ArC flags unusable here (see below) |
| 1 | Jandex/resolution Gradle task on **resolved** classpath | `check` | `49f47d6` | **highest** — auto-verifies the per-service split, which nothing checked before |
| 2 | Konsist source rules (JUnit) | `test` | `d3a5bac` | defense-in-depth + naming; mostly overlaps L1/graph |
| 3 | Custom Quarkus extension (Java build steps) | `quarkusBuild` augmentation | `4325ea3` | **demo only** — one net-new check; ArC pre-empts the rest |

### Layer 0 — strictness flags
- **Works:** `allWarningsAsErrors`, `progressiveMode`, `explicitApi()` (on contract modules),
  `quarkus.arc.detect-wrong-annotations=true`, `quarkus.arc.fail-on-intercepted-private-method=true`.
- **`-Xjsr305=strict` is near-theater here** — Jakarta/Hibernate/Panache carry no JSR-305 nullness
  annotations, so return types stay Kotlin platform types regardless. Kept as cheap, flagged zero issues.
- **`transform-unproxyable-classes=false` and `strict-compatibility=true` CANNOT be enabled** in this
  codebase (left commented with rationale in `application.properties`). `allopen` already opens every
  `@ApplicationScoped` class, so there's no Kotlin-final-bean bug to catch; instead the flag rejects **all
  ~16 constructor-injected normal-scoped beans** for lacking a synthesized no-arg proxy ctor, AND rejects
  Quarkus's OWN framework beans (`SmallRyeManagedExecutor`, Agroal `DataSources`, health factory). Enabling
  them would require an app-wide bean-scope refactor. **Lesson:** these flags fit apps that don't lean on
  idiomatic constructor-injection DI; they're not free strictness for a Kotlin+Panache app.

### Layer 1 — the real win: verify the per-service split on the RESOLVED classpath
The per-service split (`inventory-service` excludes `characters`/`accounts` impl, etc.) was only ever a
hand-checked `:dependencies` comment. Layer 1 makes it a `check` gate. Two mechanisms (both in root
`build.gradle.kts`):
- **Project composition (transitive-safe):** walk `runtimeClasspath.incoming.resolutionResult`, collect
  `ProjectComponentIdentifier.getProjectPath()` (Gradle 9 API), fail if a forbidden **impl** project is
  present directly OR transitively. Checking *declared* deps instead (the first, rejected design) is a
  false-green — it misses a forbidden module pulled in transitively.
- **Admin parity (Jandex over resolved jar files):** every `AdminDataProvider` implementor must be
  `@ApplicationScoped` (else it silently drops from `@All List<>`) and be referenced by exactly one
  `/admin-data/*` resource. Note: Jandex reads class/annotation structure, NOT method bodies or instance-`val`
  values — so an `id` written in `<init>` is invisible; enforce parity **structurally** (provider↔resource
  wiring), not by string-matching the id.
- **Config-cache safety:** capture the resolved `FileCollection`/component set inside `doLast`, never the
  `Project`. Verify with `./gradlew check --configuration-cache`.
- **Do NOT scan for impl→impl *references*** — structurally dead: the Gradle graph makes the reference
  uncompilable, so it can't exist until someone first adds the forbidden dep (which the composition check
  already catches). Check class/project **presence**, not references.

### Layer 2 — Konsist (source-level, no boot)
Konsist 0.17.3 (embeds kotlin-compiler-embeddable 2.0.21) **resolved and ran fine under JDK 26 / Kotlin
2.4** — de-risk with a trivial test first; ArchUnit is the fallback. Rules: source-import module boundary
(defense), naming conventions, `AdminDataProvider` `@ApplicationScoped`. Mostly overlaps L1/the Gradle graph;
earns its keep on naming + readable source-level failures. First to cut for leanness.

### Layer 3 — custom extension: how, and why it's demo-value
- **Build steps MUST be Java** (`arch-rules/deployment/src/main/java`). `quarkus-extension-processor` does not
  index Kotlin `@BuildStep` classes (#35110) → `quarkus-build-steps.list` empty → validation silently never
  runs. A green build proves NOTHING; verify the list is non-empty AND add a standing negative fixture.
- **Layout:** empty `runtime` (`io.quarkus.extension`, `deploymentModule.set("arch-rules-deployment")`) +
  Java `deployment`. Project `name` MUST equal the `deploymentModule` string (mismatch = silent no-op).
  App-shells depend on the runtime.
- **API pins:** `ValidationPhaseBuildItem.getContext().beans().assignableTo(Type)` takes a Jandex `Type`
  (`Type.create(dotName, CLASS)`), not a `DotName`. Produce `ValidationErrorBuildItem(List<Throwable>)`,
  collect-then-produce-once (Scheduler idiom), never throw; consume-only inputs / produce-only errors (avoid
  `ChainBuildException` cycles).
- **Standing negative fixture (mandatory):** a `QuarkusUnitTest` with `.setExpectedException(...)` deploying a
  synthetic app that violates a rule, asserting augmentation fails. Sanity-check it: remove the build step →
  the negative test must go RED (proves it exercises YOUR validator). This permanently guards liveness; the
  build-steps.list canary is necessary but not sufficient.
- **Why demo-value (measured, not assumed):**
  - The "single `PlayerCharacters` producer" rule must be `>1` not `!=1` — `characters-service` hosts the
    producer but injects it nowhere, so ArC's unused-bean removal prunes it to count 0; a literal `!=1` would
    false-fail a legitimate topology.
  - For the ambiguous (2-producer) case, ArC's own `AmbiguousResolutionException` fires **before** the custom
    `ValidationPhase` step runs — so that rule is a clearer-message-only wrapper for the latent
    2-producers-no-consumer case. **Only the `AdminDataProvider`-scope rule is genuinely net-new** (ArC
    doesn't care about a `@Dependent` implementor). Everything else duplicates ArC or Layer 1.

## Bottom line / recommendation
- **Layers 0–1 are the high-value core:** a strict baseline + the per-service split finally auto-verified on
  the resolved classpath. Do these.
- **Layer 2** is cheap breadth (naming, readable source failures); include for defense, cut first for leanness.
- **Layer 3** is a Quarkus-native *demonstration* of architecture-as-augmentation-failure with ~one net-new
  check — worth it to show the mechanism, not for raw safety. Build it correctly (Java, negative fixture) or
  not at all.
- **You cannot get Go/IDE-level compile-time DI in Quarkus** without switching to a compile-time-DI framework.
  What you CAN do is make the Gradle build (compile + augmentation + check) reject every architectural
  violation the module graph and ArC don't already catch — which this ladder does.

Plan: `docs/plans/2026-07-05-2249-quarkus-aggressive-build-verification-plan.md` (v2, reworked after a
grumpy-reviewer pass that killed the v1 boot-smoke/declared-dep/impl-reference designs).
