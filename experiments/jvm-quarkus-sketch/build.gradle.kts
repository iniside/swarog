import io.gitlab.arturbosch.detekt.Detekt
import io.gitlab.arturbosch.detekt.extensions.DetektExtension
import org.gradle.api.artifacts.component.ComponentIdentifier
import org.gradle.api.artifacts.component.ProjectComponentIdentifier
import org.gradle.api.artifacts.result.ResolvedComponentResult
import org.gradle.api.artifacts.result.ResolvedDependencyResult
import org.jboss.jandex.DotName
import org.jboss.jandex.IndexView
import org.jboss.jandex.Indexer
import org.jetbrains.kotlin.gradle.dsl.JvmTarget
import org.jetbrains.kotlin.gradle.dsl.KotlinJvmProjectExtension
import java.util.jar.JarFile

// Jandex on the BUILDSCRIPT classpath so the verification tasks below can index the RESOLVED jar
// files of each service (Layer 1). It rides only the build's own classpath — no project depends on
// it. (Coords are `io.smallrye:jandex`, the current home of the `org.jboss.jandex.*` v3 API.)
buildscript {
    repositories { mavenCentral() }
    dependencies { classpath("io.smallrye:jandex:3.2.7") }
}

// Parent build: the module boundaries are now PHYSICAL (one Gradle module per architectural
// module). Only `app` applies `io.quarkus`; library modules compile against Quarkus via the BOM.
// Plugins are declared here `apply false` so subprojects resolve them by id without a version
// (versions come from settings.gradle.kts pluginManagement).
plugins {
    kotlin("jvm") apply false
    kotlin("plugin.allopen") apply false
    kotlin("plugin.jpa") apply false
    id("io.quarkus") apply false
    id("io.gitlab.arturbosch.detekt") apply false
}

// Common config for every Kotlin module — repositories, jvmTarget=JVM_26, java toolchain 26,
// JUnit platform for tests. Applied reactively when the kotlin-jvm plugin lands on a subproject.
subprojects {
    repositories { mavenCentral() }

    plugins.withId("org.jetbrains.kotlin.jvm") {
        extensions.configure<KotlinJvmProjectExtension> {
            compilerOptions {
                jvmTarget = JvmTarget.JVM_26
                // Strict-compile canaries (verification Layer 0): every warning is a build
                // failure, progressive mode opts into the newest deprecation/resolution rules,
                // and -Xjsr305=strict treats JSR-305 nullability as hard (cheap, low-yield here —
                // Jakarta/Panache carry no JSR-305 annotations, so it is not a null-interop fix).
                allWarningsAsErrors.set(true)
                progressiveMode.set(true)
                freeCompilerArgs.add("-Xjsr305=strict")
            }
        }
        extensions.configure<JavaPluginExtension> {
            toolchain { languageVersion = JavaLanguageVersion.of(26) }
        }
        tasks.withType<Test>().configureEach { useJUnitPlatform() }

        // Verification Layer 5 — detekt static analysis (bugs/smells), complementing the strict
        // compile flags above (Layer 0) rather than duplicating them. Applied to every Kotlin
        // subproject via the same "apply reactively on the kotlin-jvm plugin id" pattern used
        // for compilerOptions/toolchain, so a new module gets the gate for free.
        apply(plugin = "io.gitlab.arturbosch.detekt")
        extensions.configure<DetektExtension> {
            buildUponDefaultConfig = true
            config.setFrom(rootProject.file("config/detekt/detekt.yml"))
            baseline = file("detekt-baseline.xml")
            parallel = true
        }
        tasks.withType<Detekt>().configureEach {
            // Detekt 1.23.8 (latest stable; verified 1.23.7 too) embeds its own (older) Kotlin
            // compiler front-end for PSI parsing, which caps --jvm-target at 22 — it does not yet
            // know about JVM_26. This is purely the internal parser target for detekt's own
            // analysis and is independent of the project's real compile target (JvmTarget.JVM_26,
            // set above via compilerOptions); it does not relax anything this project compiles/runs
            // with.
            jvmTarget = "21"
            reports {
                html.required.set(true)
                xml.required.set(false)
                txt.required.set(false)
                sarif.required.set(false)
                md.required.set(false)
            }
            // Detekt 1.23.8's bundled `org.jetbrains.kotlin.com.intellij.util.lang.JavaVersion`
            // (a vendored, very old IntelliJ util shipped alongside its own embedded Kotlin
            // compiler front-end) throws IllegalArgumentException("26.0.1") parsing the HOST JVM's
            // `java.version` at analysis start — that parser predates JDK feature releases in the
            // 20s and was never patched upstream (still true in the latest 1.23.8; there is no 2.x
            // detekt release yet). Detekt runs THIS task in-process (a cached URLClassLoader, not a
            // forked worker — confirmed from the stacktrace), and the task exposes no javaLauncher/
            // jdkHome hook that changes which JVM the analysis runs in. Spoofing `java.version` for
            // the duration of the task action is the narrowest fix: it only affects what detekt's
            // vendored parser reads (nothing this project compiles, runs, or tests reads this
            // property), and detekt's own PSI parsing is JDK-feature-agnostic (jvmTarget above
            // already pins its target release independently).
            doFirst { System.setProperty("java.version", "21.0.1") }
        }
        // Real gate: `check` (and therefore `build`) fails on a detekt finding, same as the
        // composition/admin-parity verification tasks below.
        tasks.named("check") { dependsOn(tasks.withType<Detekt>()) }
    }
}

// ============================================================================================
// Verification Layer 1 — auto-verify the PER-SERVICE SPLIT on the RESOLVED classpath.
//
// Why this exists: the module split is enforced at COMPILE time only for what a service directly
// declares — `inventory-service` cannot reference an `characters` impl type because `:characters`
// isn't on its compile classpath. But a forbidden impl can still arrive TRANSITIVELY (some allowed
// dep grows a `project(":characters")` edge), and nothing caught that: the ArchUnit test runs on
// `app`'s classpath only and never sees the split jars, and the old ":inventory-service:dependencies"
// comment was hand-checked. These tasks close that gap by inspecting the RESOLVED runtime graph, so
// a violation fails `./gradlew check`. Two distinct mechanisms, each fit to what it can actually see:
//
//   A. Project composition (transitive-safe, no bytecode needed) — walk the resolved runtime
//      resolution result and collect every `ProjectComponentIdentifier` path. A forbidden IMPL
//      project appearing anywhere in that set (direct OR transitive) fails. Checks PRESENCE, never
//      impl->impl references (those are structurally dead — the reference can't compile until the
//      forbidden project is already on the classpath, which is exactly what this catches first).
//   B. Admin parity (needs class/annotation structure -> Jandex over the resolved jar FILES) — every
//      `AdminDataProvider` bean must be `@ApplicationScoped` (else it silently drops from the admin's
//      `@All List<AdminDataProvider>`) and must be wired to a `@Path("/admin-data/<id>")` resource
//      (else it is unreachable when the module is remote).
// ============================================================================================

// --- Mechanism A: task + helper -------------------------------------------------------------

// Config-cache-safe: resolves lazily and yields a plain Set<String> of project paths. Captures the
// `rootComponent` provider (a supported provider source), NOT the Project — nothing here is queried
// at configuration time.
fun projectPathsOf(config: Configuration): Provider<Set<String>> =
    config.incoming.resolutionResult.rootComponent.map { root ->
        val paths = sortedSetOf<String>()
        val seen = hashSetOf<ComponentIdentifier>()
        fun walk(component: ResolvedComponentResult) {
            if (!seen.add(component.id)) return
            (component.id as? ProjectComponentIdentifier)?.let { paths.add(it.projectPath) }
            component.dependencies.filterIsInstance<ResolvedDependencyResult>().forEach { walk(it.selected) }
        }
        walk(root)
        paths
    }

// Fails if a forbidden IMPL project is present on the resolved runtime classpath. `forbiddenExact`
// is matched by exact project-path equality (so `:characters` forbidden does NOT trip on the allowed
// contract `:characters-api`). `forbidContractProjects` additionally bans ANY `*-api`/`*-events`
// project (for `edge`, which must stay a transport-agnostic leaf that knows no feature contract).
abstract class VerifyServiceCompositionTask : DefaultTask() {
    @get:Input abstract val serviceName: Property<String>
    @get:Input abstract val resolvedProjectPaths: SetProperty<String>
    @get:Input abstract val forbiddenExact: SetProperty<String>
    @get:Input abstract val forbidContractProjects: Property<Boolean>

    @TaskAction
    fun verify() {
        val paths = resolvedProjectPaths.get()
        val violations = mutableListOf<String>()

        forbiddenExact.get().filter { it in paths }.sorted().forEach {
            violations += "forbidden impl project $it is on the resolved runtime classpath (directly or transitively)"
        }
        if (forbidContractProjects.get()) {
            paths.filter { it.endsWith("-api") || it.endsWith("-events") }.sorted().forEach {
                violations += "contract project $it is on the resolved runtime classpath ($name must stay a transport-agnostic leaf)"
            }
        }

        if (violations.isNotEmpty()) {
            throw GradleException(
                "Per-service split violated for ${serviceName.get()}:\n  - " + violations.joinToString("\n  - ") +
                    "\n(resolved projects: ${paths.joinToString(", ")})",
            )
        }
        logger.lifecycle("verifyServiceComposition: ${serviceName.get()} OK (${paths.size} resolved projects, no forbidden dep)")
    }
}

// --- Mechanism B: admin parity task ---------------------------------------------------------

// Indexes the RESOLVED runtime jar files with Jandex and asserts admin parity from class structure.
//
// Note on the id<->path check: an `AdminDataProvider`'s `id` is a Kotlin instance `val` (not a
// `const`), so its literal value is set in `<init>` and is NOT in the constant pool — Jandex (which
// reads class/annotation structure, not method bodies or instance-field values) cannot read it. So
// parity is enforced STRUCTURALLY, which is exactly how the code wires it: the `@Path("/admin-data/<id>")`
// resource takes the provider as a constructor property (`XAdminDataResource(val provider: XAdminData)`)
// and builds the wire DTO from `provider.id`. We therefore require each provider to be referenced by
// exactly one `@Path("/admin-data/...")` resource (via a field of the provider's type) — guaranteeing
// it is reachable on the wire — plus the `@ApplicationScoped` scope that keeps it in `@All`.
abstract class VerifyAdminParityTask : DefaultTask() {
    @get:Classpath abstract val classpath: ConfigurableFileCollection

    @Suppress("DEPRECATION") // getAllKnownImplementors(DotName) — the non-index-building overload is what we want here
    @TaskAction
    fun verify() {
        val indexer = Indexer()
        classpath.files.forEach { file ->
            when {
                file.isDirectory ->
                    file.walkTopDown().filter { it.isFile && it.extension == "class" }
                        .forEach { c -> c.inputStream().use { runCatching { indexer.index(it) } } }
                file.name.endsWith(".jar") ->
                    JarFile(file).use { jar ->
                        jar.entries().asSequence().filter { it.name.endsWith(".class") }.forEach { entry ->
                            jar.getInputStream(entry).use { runCatching { indexer.index(it) } }
                        }
                    }
                file.name.endsWith(".class") ->
                    file.inputStream().use { runCatching { indexer.index(it) } }
            }
        }
        val index: IndexView = indexer.complete()

        val providerIface = DotName.createSimple("admin.adminapi.AdminDataProvider")
        val appScoped = DotName.createSimple("jakarta.enterprise.context.ApplicationScoped")
        val jaxrsPath = DotName.createSimple("jakarta.ws.rs.Path")

        val providers = index.getAllKnownImplementors(providerIface)
        if (providers.isEmpty()) {
            throw GradleException(
                "admin parity: no AdminDataProvider implementors found on the resolved classpath — " +
                    "index or wiring is wrong (expected the module admin dashboards).",
            )
        }

        // Every @Path class whose value is /admin-data/<non-empty> — the per-module wire endpoints.
        // (The admin's own AdminDataClient carries @Path("/admin-data") exactly, so it does not match.)
        val adminDataResources = index.knownClasses.filter { ci ->
            ci.declaredAnnotation(jaxrsPath)?.value()?.asString()?.matches(Regex("^/admin-data/.+")) == true
        }

        val problems = mutableListOf<String>()
        providers.sortedBy { it.name().toString() }.forEach { provider ->
            if (!provider.hasDeclaredAnnotation(appScoped)) {
                problems += "${provider.name()} implements AdminDataProvider but is not @ApplicationScoped " +
                    "(it would silently drop from the admin's @All List<AdminDataProvider>)"
            }
            val wiring = adminDataResources.filter { r -> r.fields().any { it.type().name() == provider.name() } }
            when (wiring.size) {
                0 -> problems += "${provider.name()} has no @Path(\"/admin-data/<id>\") resource wiring it — " +
                    "it is unreachable when the module is hosted in another process"
                1 -> {} // exactly one wire endpoint, as required
                else -> problems += "${provider.name()} is wired by multiple admin-data resources: " +
                    wiring.map { it.name() }
            }
        }

        if (problems.isNotEmpty()) {
            throw GradleException("Admin parity violated on the resolved classpath:\n  - " + problems.joinToString("\n  - "))
        }
        logger.lifecycle("verifyAdminParity: ${providers.size} AdminDataProvider(s) OK (each @ApplicationScoped + one /admin-data endpoint)")
    }
}

// --- Wiring: register the checks on the split services + `edge`, and admin parity on `app` -----

// The forbidden set per service is DERIVED FROM the actual module taxonomy (impl projects vs
// `*-api`/`*-events`/`*-client` contracts), not from memory:
//   :inventory-service  — must not host the characters/accounts IMPLS (it reaches characters over
//                         QUIC via :characters-client); their contracts are allowed.
//   :characters-service — must not host inventory/admin, nor the REMOTE :characters-client (it hosts
//                         the LOCAL characters producer instead).
//   :edge               — a transport leaf: no feature contract project at all.
data class CompositionRule(val forbidden: Set<String>, val forbidContracts: Boolean)

val compositionRules = mapOf(
    ":inventory-service" to CompositionRule(setOf(":characters", ":accounts"), forbidContracts = false),
    ":characters-service" to CompositionRule(setOf(":inventory", ":admin", ":characters-client"), forbidContracts = false),
    ":edge" to CompositionRule(emptySet(), forbidContracts = true),
)

compositionRules.forEach { (path, rule) ->
    // Defer until the subproject applies kotlin-jvm — only then do `runtimeClasspath` and `check`
    // (via the base plugin) exist. Root configuration otherwise races ahead of the subproject.
    project(path).plugins.withId("org.jetbrains.kotlin.jvm") {
        val checkTask = project(path).tasks.register<VerifyServiceCompositionTask>("verifyServiceComposition") {
            group = "verification"
            description = "Fails if a forbidden project is on $path's RESOLVED runtime classpath (transitive-safe)."
            serviceName.set(path)
            resolvedProjectPaths.set(projectPathsOf(project(path).configurations.getByName("runtimeClasspath")))
            forbiddenExact.set(rule.forbidden)
            forbidContractProjects.set(rule.forbidContracts)
        }
        project(path).tasks.named("check") { dependsOn(checkTask) }
    }
}

// Admin parity runs on `app`, the one shell where ALL admin providers are co-located.
project(":app").plugins.withId("org.jetbrains.kotlin.jvm") {
    val parityTask = project(":app").tasks.register<VerifyAdminParityTask>("verifyAdminParity") {
        group = "verification"
        description = "Fails if an AdminDataProvider on app's RESOLVED classpath is not @ApplicationScoped or lacks its /admin-data endpoint."
        classpath.from(project(":app").configurations.named("runtimeClasspath"))
    }
    project(":app").tasks.named("check") { dependsOn(parityTask) }
}
