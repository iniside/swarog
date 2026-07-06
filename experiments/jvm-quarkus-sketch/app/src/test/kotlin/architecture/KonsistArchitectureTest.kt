package architecture

import admin.adminapi.AdminDataProvider
import com.lemonappdev.konsist.api.Konsist
import com.lemonappdev.konsist.api.ext.list.withAnnotationOf
import com.lemonappdev.konsist.api.ext.list.withFunction
import com.lemonappdev.konsist.api.ext.list.withNameEndingWith
import com.lemonappdev.konsist.api.ext.list.withParentOf
import com.lemonappdev.konsist.api.verify.assertTrue
import io.quarkus.runtime.annotations.RegisterForReflection
import jakarta.enterprise.context.ApplicationScoped
import jakarta.enterprise.inject.Produces as CdiProduces
import jakarta.ws.rs.Path
import org.junit.jupiter.api.Test

/**
 * Layer 2 of the build-verification ladder (source-level rules, no boot, no DB) — see
 * docs/plans/2026-07-05-2249-quarkus-aggressive-build-verification-plan.md. These are the rules
 * NOT already covered by the Gradle module graph (physical, compiler-enforced) or Layer 1's
 * Jandex task on the RESOLVED classpath (root build.gradle.kts): they exist for a READABLE,
 * source-line failure the moment a rule is violated, as defense-in-depth. Each test's KDoc says
 * exactly what it overlaps and why it earns its keep anyway.
 *
 * De-risk: Konsist 0.17.3 embeds a K2 front-end (`kotlin-compiler-embeddable` 2.0.21); its
 * compatibility with this project's JDK-26 toolchain + Kotlin 2.4.0 was unproven before this
 * suite ran green — confirmed by running a trivial `Konsist.scopeFromProject().classes()
 * .assertTrue { true }` first. Konsist resolves and scans cleanly; no ArchUnit fallback needed.
 */
class KonsistArchitectureTest {

    /** The impl packages (NOT their `*api`/`*events`/`adminapi` contract sub-packages, which are
     *  the deliberately shared surface — see CLAUDE.md constraint 5). */
    private val implPackages = setOf("accounts", "characters", "inventory", "admin")

    /**
     * Source-import module boundary. The Gradle module graph ALREADY makes this physically
     * impossible today — no impl project depends on another impl project, so the forbidden
     * import couldn't even resolve. This test is pure defense-in-depth: it gives a readable,
     * named failure at the exact source line the moment someone adds the forbidden
     * `project(...)` dependency, instead of a wall of "unresolved reference" compiler errors
     * scattered across whichever files happened to add the import first.
     */
    @Suppress("UnreachableCode") // detekt false-positive: its control-flow analysis treats the
    // labeled `return@flatMap emptyList()` inside the outer lambda as exiting the whole function,
    // so it marks every statement after it (in the SAME lambda body, on later iterations) as
    // unreachable. It is not — `return@flatMap` only short-circuits that one element's mapping; the
    // test passes and the surrounding `flatMap` genuinely runs across all `scope.files`.
    @Test
    fun `impl module files do not import another impl module`() {
        val scope = Konsist.scopeFromProject()
        val violations = scope.files.flatMap { file ->
            val ownPackage = implPackages.firstOrNull { file.hasPackage(it) } ?: return@flatMap emptyList()
            file.imports
                .filter { import ->
                    val importedPackage = import.name.substringBeforeLast('.', missingDelimiterValue = "")
                    importedPackage in implPackages && importedPackage != ownPackage
                }
                .map { import -> "${file.path} (package '$ownPackage') imports '${import.name}'" }
        }
        assert(violations.isEmpty()) {
            "impl module imports another impl module's package directly — react via the " +
                "*events/adminapi contract or a service interface instead:\n" + violations.joinToString("\n")
        }
    }

    /**
     * JAX-RS resource naming: a class named `*Resource` is expected to carry `@Path` — the
     * convention this codebase actually follows (`CharactersResource`, `InventoryResource`,
     * `AdminResource`, `CharactersAdminDataResource`, `InventoryAdminDataResource`). Deliberately
     * NOT the converse: other `@Path`-annotated classes exist under distinct, intentional names
     * (`InventoryEventSink`, the REST-client interface `AdminDataClient`), so checking "every
     * `@Path` class ends in Resource" would be false today and this only checks one direction.
     *
     * Scoped to the HTTP-serving impl packages: `Resource` is a generic suffix, so it legitimately
     * names non-web utilities elsewhere (`edge.CachedResource` is a concurrency cache, not an
     * endpoint). The JAX-RS convention only holds where JAX-RS resources actually live.
     */
    @Test
    fun `classes named Resource are JAX-RS resources`() {
        Konsist.scopeFromProject().classes()
            .withNameEndingWith("Resource")
            .filter { klass -> implPackages.any { klass.resideInPackage("$it..") } }
            .assertTrue { it.hasAnnotationOf(Path::class) }
    }

    /**
     * CDI producer naming, both directions — verified true for the current codebase: exactly
     * `characters.LocalPlayerCharactersProducer` and `charactersclient.CharactersClientProducer`
     * carry a `jakarta.enterprise.inject.Produces`-annotated function, and both already end in
     * `Producer`. The import is aliased to `CdiProduces` so it can't be confused with
     * `jakarta.ws.rs.Produces` — an unrelated JAX-RS media-type annotation on resource methods
     * that happens to share the simple name `Produces`.
     */
    @Test
    fun `CDI producer classes are named Producer, and vice versa`() {
        val scope = Konsist.scopeFromProject()
        scope.classes().withFunction { it.hasAnnotationOf(CdiProduces::class) }.assertTrue {
            it.hasNameEndingWith("Producer")
        }
        scope.classes().withNameEndingWith("Producer").assertTrue {
            it.hasFunction { fn -> fn.hasAnnotationOf(CdiProduces::class) }
        }
    }

    /**
     * Event payloads: every `@RegisterForReflection` class — the wire-serde marker this codebase
     * uses exclusively for published event payloads (`PlayerRegistered`, `CharacterCreated`,
     * `CharacterDeleted`) — lives in a package ending `events` (`accountsevents`,
     * `charactersevents`), never alongside impl code.
     */
    @Test
    fun `event payloads live in an events package`() {
        Konsist.scopeFromProject().classes().withAnnotationOf(RegisterForReflection::class).assertTrue {
            it.packagee?.name?.endsWith("events") == true
        }
    }

    /**
     * Cheap defense, overlapping Layer 1's Jandex admin-parity task (root build.gradle.kts),
     * which checks the identical invariant on the RESOLVED classpath via bytecode: every
     * [AdminDataProvider] implementor must be `@ApplicationScoped`, else it silently drops from
     * admin's `@All List<AdminDataProvider>`. Kept here too for a source-level, IDE-visible
     * failure — the honest overlap is called out, not hidden.
     */
    @Test
    fun `AdminDataProvider implementors are ApplicationScoped`() {
        Konsist.scopeFromProject().classes().withParentOf(AdminDataProvider::class).assertTrue {
            it.hasAnnotationOf(ApplicationScoped::class)
        }
    }
}
