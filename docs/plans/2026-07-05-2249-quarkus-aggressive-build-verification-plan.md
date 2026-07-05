# Plan: aggressive build-time verification for the Quarkus sketch

> Branch `quarkus-per-service` (`experiments/jvm-quarkus-sketch/`). Goal: push wiring/architecture
> verification from **runtime → build time**, layered, so a violation fails `./gradlew build`. Directive:
> "pełna weryfikacja, tak agresywna jak wściekły imigrant w Paryżu" — maximal, defense-in-depth, but
> ordered by value/effort and **honest about what each layer actually buys**.
>
> **v2 — reworked after a grumpy-reviewer pass (think-hard).** The review killed three things the v1
> plan over-sold: split-service `@QuarkusTest` boot-smokes (unbootable AND self-defeating), a declared-dep
> classpath check (false-green on transitive), and an impl→impl reference scan (structurally dead under the
> module split). The Quarkus custom extension was demoted from marquee to opt-in demo. v2 leads with a plain
> Jandex Gradle task on the **resolved** classpath — the reviewer's 80/20. Full punch-list resolution at the
> bottom.

## Context — what already verifies (MANDATORY: don't add a twin)

Go catches wiring in `go build`; Quarkus catches it at **augmentation** (`quarkusBuild`, a Gradle task after
`compileKotlin`), not in `kotlinc`/IDE. Existing surfaces we EXTEND, not duplicate:

- **The Gradle module graph is the primary, compiler-enforced boundary.** `characters` physically cannot
  reference an `inventory` type — `:inventory` isn't on its compile classpath, so it wouldn't compile. This
  makes any "no impl→impl *reference*" check (ArchUnit slices, a Jandex constant-pool scan) **structurally
  dead**: the reference can't exist until someone first adds the forbidden `project(...)` dep. So the real
  invariant to guard is **"a forbidden dep does not appear on a service's resolved classpath"** — a property
  of the resolved graph, caught before any reference is written.
- **ArC (CDI) automatic validation** already fails augmentation on *unsatisfied* and *ambiguous* injection
  points. "Exactly one `PlayerCharacters` producer" is therefore *already* enforced (two → ambiguous, zero →
  unsatisfied). Any explicit check adds only a **clearer message**, not net safety — say so, don't oversell.
- **ArchUnit test** (`app/src/test/kotlin/architecture/ArchitectureTest.kt`) — runs ONLY on `app`'s
  classpath; never sees `characters-client`, `edge`, contracts, or the split jars. So **nothing today
  automatically verifies the per-service split** (the `inventory-service` "no characters/accounts impl"
  claim is a hand-checked `:dependencies` comment). Closing that is Layer 1's main job.
- **Three seams** map to `@Produces`/`@Inject` by type, HTTP-path event sinks, `@All List<AdminDataProvider>`.

## Research citations (6 subagents, synthesized)

- Extension API: runtime+deployment Gradle subprojects, unpublished, in-repo; **build steps MUST be Java**
  (`quarkus-extension-processor` skips Kotlin `@BuildStep` → `quarkus-build-steps.list` empty, validation
  silently no-ops, [#35110](https://github.com/quarkusio/quarkus/issues/35110));
  `ValidationErrorBuildItem(List<Throwable>)` produce-not-throw, aggregated
  ([ValidationPhaseBuildItem.java](https://github.com/quarkusio/quarkus/blob/main/extensions/arc/deployment/src/main/java/io/quarkus/arc/deployment/ValidationPhaseBuildItem.java),
  templates [Mailer](https://github.com/quarkusio/quarkus/blob/main/extensions/mailer/deployment/src/main/java/io/quarkus/mailer/deployment/MailerProcessor.java)/[Scheduler](https://github.com/quarkusio/quarkus/blob/main/extensions/scheduler/deployment/src/main/java/io/quarkus/scheduler/deployment/SchedulerProcessor.java));
  in-repo unpublished OK ([#31999](https://github.com/quarkusio/quarkus/discussions/31999)), name must equal
  `deploymentModule.set(...)`; cycle risk [#39660](https://github.com/quarkusio/quarkus/issues/39660);
  Kotlin+composite-build issues [#47430](https://github.com/quarkusio/quarkus/issues/47430).
- ArC strictness (config, fails augmentation): `transform-unproxyable-classes=false` (Kotlin-final bean →
  build error not silent rewrite — **the real canary**), `detect-wrong-annotations=true`,
  `strict-compatibility=true`, `fail-on-intercepted-private-method=true` ([cdi-reference](https://quarkus.io/guides/cdi-reference)).
- Config: plain `quarkusBuild` does NOT run `STATIC_INIT`, so `@ConfigProperty`/`@ConfigMapping` validation
  is *startup*-time; only a booting `@QuarkusTest` (or `BUILD_TIME`-phase config) pulls it into `build`.
- Kotlin gates: `allWarningsAsErrors` + `progressiveMode` (real work); `-Xjsr305=strict` only bites where
  JSR-305 annotations exist on the Java API — **Jakarta/Hibernate/Panache don't carry them**, so it's near-
  theater here (keep, cheap, but no false "fixes the `UUID?` seam" claim). `explicitApi()` on contracts.
  **Konsist** (Kotlin-source-aware, less boilerplate than ArchUnit) for source-level rules. KSP task-cycle
  fixed in 3.26.2 (we're on 3.37) but still isolate KSP off Quarkus modules.
- Sketch state: root `build.gradle.kts` = `JvmTarget.JVM_26`, `useJUnitPlatform()`, **no** strictness flags,
  **no** detekt/Konsist, **no** `@QuarkusTest`, **no** native, **no** CI. `characters-service` base config
  `roles=accounts,characters` → `CharactersEdgeServer.start(@Observes StartupEvent)` fires, needs
  `EDGE_CERT_THUMBPRINT` + msquic FFM + binds :9100; `@Scheduled` outbox relays fire on boot. **These make a
  split-service boot-smoke unbootable** (reviewer BLOCKER — see resolution).

## Target end-state — the verification ladder (v2)

| Layer | Gate | Fails at | Covers | Net-new? |
|---|---|---|---|---|
| 0 | ArC + kotlinc strictness flags | `compileKotlin` / augmentation | Kotlin-final beans, wrong CDI annos, warnings, CDI strict-compat | yes (canaries) |
| 1 | **Jandex Gradle task on resolved classpath** | `check` | per-service impl exclusion (transitive-safe), admin `@Path`/provider parity, `edge` leaf | **yes — the split is unverified today** |
| 2 | Konsist (source) | `test` | `AdminDataProvider` impls are `@ApplicationScoped`, naming, source-import boundary (defense) | partly (scope check net-new) |
| 3 (opt-in) | Quarkus validation extension | `quarkusBuild` augmentation | same checks on the augmented **bean graph** + standing negative fixture | **demo only** — near-zero net safety over L1/ArC |
| 4 (opt) | Native-image CI job | nightly | GraalVM closed-world + static-init | yes, periodic |

Config validation for the **split-service profiles** stays a startup concern (honest gap: boot-smokes can't
cover them — resolution #1/#2 below). `app`-only boot-smoke is a marginal add, gated behind a decision.

---

## Implementation sequence (v2)

### Step 0 — persist plan `[inline]`
This file. Stay on `quarkus-per-service`.

### Step 1 — Layer 0: strictness flags + fix fallout `[opus]`
**(a) What:** root `build.gradle.kts` `compilerOptions {}` (all subprojects): `allWarningsAsErrors.set(true)`,
`progressiveMode.set(true)`, `freeCompilerArgs.add("-Xjsr305=strict")` (cheap, low-yield — no null-interop
claim). `explicitApi()` on the four contract modules only. Each app-shell `application.properties` (ArC):
`quarkus.arc.transform-unproxyable-classes=false`, `detect-wrong-annotations=true`,
`strict-compatibility=true`, `fail-on-intercepted-private-method=true`.
**(b) Why first:** foundation + canary — `transform-unproxyable-classes=false` and `allWarningsAsErrors`
surface EXISTING latent issues; must be a clean strict baseline before layering.
**(c) How:** `transform-unproxyable-classes=false` is the risky knob — any `@ApplicationScoped` bean not
covered by the `allOpen` list now FAILS. Fix per class (minimal: add scope anno to the module's `allOpen`
block, or `@Dependent`, or `open`) — NOT a blanket `open`. `-Xjsr305=strict` unlikely to flag anything here;
if it does, fix at the call site. Expect a handful of impl touches.
**(d) `[opus]`** — fallout-fixing is scope/proxy judgment.
**Verify:** `./gradlew build` green with flags on; diff = flags + minimal per-class fixes only.

### Step 2 — Layer 1: Jandex verification task on the RESOLVED classpath `[opus]`
**(a) What:** a Gradle verification task (root convention, applied to app-shells) that, for each app-shell,
resolves `runtimeClasspath` and runs Jandex over the **assembled** jar set to assert:
- **Per-service impl exclusion (transitive-safe):** `inventory-service`'s resolved classpath contains NO
  class from the `characters`/`accounts` impl packages (only their `*api`/`*events` contracts + `characters-
  client`); `characters-service` contains NO `inventory`/`admin`/`characters-client` classes. This is the
  per-topology "assembled index" view — the one thing the reviewer agreed is genuinely valuable, done as a
  plain task, not an extension.
- **Admin parity:** for every class implementing `admin.adminapi.AdminDataProvider` on the classpath, assert
  (i) it carries `@ApplicationScoped` (else it silently drops from `@All`), and (ii) a sibling
  `@Path("/admin-data/<id>")` class exists with matching `id`.
- **`edge` leaf:** `edge`'s resolved classpath resolves no `*-api`/`*-events` project.
Wire into `check`.
**(b) Why now / order:** this is the PRIMARY gate and the thing missing today (auto-verify the split).
Resolved-graph based → transitive-safe (fixes the v1 false-green). Independent of Step 1. Subsumes v1's
Step-2 dep-check AND the useful part of v1's extension, without the deployment-module machinery.
**(c) How — the traps the reviewer named:** resolve via
`configurations.runtimeClasspath.get().incoming.resolutionResult.allComponents` filtered to
`ProjectComponentIdentifier` for the **project-composition** check (transitive), OR — to actually read
CLASSES for the admin-parity/Jandex checks — resolve `runtimeClasspath.get().files` and index the jars with
`org.jboss.jandex.Indexer` at task time inside `doLast` (config-cache-safe: capture the FileCollection, not
the Project). Use Gradle 9 API `ProjectComponentIdentifier.getProjectPath()` (not the deprecated
`dependencyProject`). Add Jandex as a `buildscript`/task classpath dep. **Do NOT scan for impl→impl
*references*** (dead per Context) — scan for *class presence* by package.
**(d) `[opus]`** — Jandex indexing + Gradle-9 resolution API + config-cache correctness is judgment, not
mechanical.
**Verify:** `./gradlew check` green; add `implementation(project(":characters"))` to `inventory-service` →
task fails naming the forbidden class; make an `AdminDataProvider` `@Dependent` → parity check fails; revert.

### Step 3 — Layer 2: Konsist for the non-dead source rules `[sonnet]`
**(a) What:** a Konsist test source set (extend `app`'s test module or a small `architecture-test` module)
with rules that are NOT already dead/covered: every `AdminDataProvider` impl declares `@ApplicationScoped`;
naming conventions (module/package/`*Resource`/`*Producer`); as *defense-in-depth* the source-import
boundary ("no impl file imports another impl package"). NO boot, no DB.
**(b) Why now:** cheap source-level net for the admin-scope rule (a `@Dependent` provider compiles fine but
vanishes from `@All` — neither Gradle nor Layer 1's presence-check catches the SCOPE, Layer 1 does via
Jandex anno-check too, so this overlaps L1 on that one; keep Konsist for naming + the source boundary +
readable failures). **Honest:** much of this overlaps Layer 1/ArC; its unique earn is naming + Kotlin-source
readability. Include because "aggressive/defense-in-depth" is the directive — but it's the first candidate to
cut if leanness wins.
**(c) How:** `Konsist.scopeFromProject()`, `.classes().withParentInterface(AdminDataProvider).assertTrue {
it.hasAnnotationOf(ApplicationScoped::class) }`, import-boundary via `.files.assertFalse { imports... }`.
**FIRST verify the Konsist version resolves under the JDK-26 toolchain / Kotlin 2.4** (it embeds a K2 front-
end; bleeding-edge JDK is unproven) — if it doesn't resolve, fall back to extending ArchUnit for these rules.
**(d) `[sonnet]`** — rules from a documented pattern; version-resolve check up front.
**Verify:** `./gradlew test` green; make a provider `@Dependent` → red; import across impl → red.

### Step 4 — Layer 3 (OPT-IN, user go/no-go): Quarkus validation extension as a demo `[opus]`
**(a) What:** IF the user opts in — two subprojects `arch-rules/{runtime,deployment}` (runtime empty +
`io.quarkus.extension`; deployment **Java** build steps). Re-implements Layer-1's checks against ArC's
**augmented bean graph** (`ValidationPhaseBuildItem.beans()`) + Jandex, failing `quarkusBuild`.
**(b) Why opt-in, why last:** the reviewer is right — over ArC's automatic ambiguous/unsatisfied + Layer 1's
resolved-classpath Jandex, this adds **almost no net safety**; its value is (i) a Quarkus-native
demonstration of architecture-as-augmentation-failure and (ii) running at `quarkusBuild` rather than `check`.
High effort + risk (Java-in-Kotlin-repo, `deploymentModule` name trap, #35110 empty-list silent no-op, #39660
cycle, #47430 Kotlin composite-build). **Default = SKIP.** Present as an explicit go/no-go.
**(c) How — if built:** Java `deployment` sources only; produce `ValidationErrorBuildItem(List<Throwable>)`,
consume-only of `ValidationPhaseBuildItem`/`CombinedIndexBuildItem`; empty `runtime` must exist; project name
== `deploymentModule.set(...)`. **Liveness is under test, not verified once by hand:** add a permanent
`negative-fixture` source set / module that deliberately violates one rule and a test asserting `quarkusBuild`
FAILS on it (else a name/index mismatch = silent green, which the non-empty-`build-steps.list` canary does
NOT catch — canary is necessary, not sufficient).
**(d) `[opus]`** — new API surface, correctness-critical (silent no-op worse than nothing).
**Verify:** the standing negative-fixture test is red-when-removed; break-each-invariant manually once; the
list is non-empty. All three, or the layer proves nothing.

### Step 5 — Layer 4 (OPTIONAL): native-image periodic gate `[sonnet]` / drop
Document a `-Pnative` command for the two services as a nightly/CI-only gate (GraalVM closed-world + static-
init). Flag native+msquic-FFM as a known unknown. Drop if not wanted.

### Step 6 — reference doc + per-layer commits `[inline]`
`docs/reference/quarkus-compile-time-verification.md` (ladder + citations). Commit after each of Steps 1–3
(and 4 if built) separately, bisectable.

---

## Risks / conscious tradeoffs (v2)
- **Config validation for split-service profiles is NOT gated at build** (boot-smokes unbootable). Accepted
  gap; documented. Mitigation if it matters later: `BUILD_TIME`-phase config or a non-booting SmallRye-Config
  parse test — out of scope here.
- **Step 1 fallout unknown until run** — canary, first, `[opus]`. If `transform-unproxyable-classes=false`
  cascades badly, fix per-class; the setting is already app-shell-scoped (it lives in augmentation) — there
  is no broader scope to pull back.
- **Layer 2 heavily overlaps Layer 1/ArC** — kept for defense-in-depth per the "aggressive" directive; first
  to cut for leanness. Called out, not hidden.
- **Layer 3 is demo-value** — explicit user go/no-go; default skip; if built, the negative-fixture test is
  mandatory (green build ≠ validator ran).
- **Diminishing returns are real:** Layers 0–1 are the high-value core (strict baseline + the split finally
  auto-verified); 2 is cheap defense; 3 is a demonstration. The honest recommendation is **do 0–1, add 2 for
  breadth, treat 3 as optional showmanship.**

## Grumpy-reviewer punch-list resolution
- **#1/#2 (BLOCKER) split-service boot-smoke unbootable + self-defeating** → **boot-smokes cut entirely.**
  Config-for-split-profiles declared an accepted gap. Konsist (Step 3) does the source rules without booting.
- **#3 (BLOCKER) declared-dep check false-green on transitive** → Step 2 now resolves
  `resolutionResult.allComponents` / indexes resolved jar files — transitive-safe.
- **#4 (BLOCKER) impl→impl reference scan structurally dead** → removed; Context now states the rule is
  class-*presence* on the resolved classpath, not references.
- **#5 (SHOULD) other extension checks near-covered** → acknowledged in the ladder ("net-new? demo only") and
  Step 4(b); single-producer = clearer-message-only, stated.
- **#6 (SHOULD) extension over-engineering vs Jandex task** → Jandex Gradle task is now Layer 1 (default); the
  extension is opt-in Step 4 with a go/no-go. Reviewer's 80/20 adopted.
- **#7 (SHOULD) build-steps.list canary insufficient** → Step 4 now mandates a standing negative-fixture test;
  canary demoted to "necessary precondition."
- **#8 (SHOULD) Dev Services/H2 hand-wave** → moot (boot-smokes cut). No DB needed by any retained gate.
- **#9 (SHOULD) outbox relays fire on boot** → moot (no boots).
- **#10 (SHOULD) `-Xjsr305=strict` theater** → claim removed; kept as cheap, labeled low-yield;
  `allWarningsAsErrors`/`transform-unproxyable`/`strict-compatibility` named as the real canaries.
- **#11 (NIT) transform-unproxyable scope mitigation confused** → corrected (already augmentation-scoped).
- **#12 (NIT) Step 3 first to boot / re-tag** → no step boots now; Step 2 (the real judgment work) is `[opus]`.
- **#13 (NIT) Konsist on JDK26/K2 unverified** → Step 3 verifies the version resolves first; ArchUnit fallback.

## Dispatch summary (for approval)
Step 1 `[opus]` · Step 2 `[opus]` · Step 3 `[sonnet]` · Step 4 `[opus]` **only if opted-in** · Step 5
`[sonnet]`/drop · Step 6 `[inline]`. Each subagent gets an effort level (ask at dispatch) + nav guidance.
Commit per layer. **Recommended scope: Steps 1–2 (core), +3 (breadth); 4 is opt-in demo.**
