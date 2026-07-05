package architecture

import com.tngtech.archunit.core.domain.JavaClasses
import com.tngtech.archunit.core.importer.ClassFileImporter
import com.tngtech.archunit.core.importer.ImportOption
import com.tngtech.archunit.lang.syntax.ArchRuleDefinition.classes
import com.tngtech.archunit.library.dependencies.SlicesRuleDefinition.slices
import org.junit.jupiter.api.Test

/**
 * The CLAUDE.md hard constraints, encoded as architecture tests — the SAME rules as the
 * framework-free sketch, proving the constraints outlive the framework swap. Changes:
 *  - the "core stays game-agnostic" rule is gone WITH core: the container replaced the registry
 *    and no core-like package remains (startup needs no cross-module ordering — each module's
 *    migration is self-contained because the architecture forbids cross-module FKs/schemas).
 *  - CDI discovers beans instead of `app` listing them, so `app` shrank to the seed.
 *  - the boundaries are now ALSO physical (one Gradle module each): `characters -> inventory` is
 *    impossible to even write. ArchUnit stays as defense-in-depth and adds a no-cross-module-
 *    entity-import rule — the one coupling the ORM would happily let you introduce.
 */
class ArchitectureTest {

    private val imported: JavaClasses = ClassFileImporter()
        .withImportOption(ImportOption.DoNotIncludeTests())
        .importPackages("accounts", "characters", "inventory", "admin", "platform", "app")

    /**
     * A module's implementation is private. Only `app` (the seed — all that's left of the
     * composition root) may reference a concrete *Module bean; everyone else depends on the
     * module's *api / *events contract, never on its impl. Same-package access is fine.
     */
    @Test
    fun `module impls are reachable only from app`() {
        val modules = mapOf(
            "AccountsModule" to "accounts",
            "CharactersModule" to "characters",
            "InventoryModule" to "inventory",
            "AdminResource" to "admin",
        )
        modules.forEach { (name, pkg) ->
            classes().that().haveSimpleName(name)
                .should().onlyHaveDependentClassesThat().resideInAnyPackage("app..", "$pkg..")
                .allowEmptyShould(true)
                .because("$name is implementation; only app wires it, others use its contract")
                .check(imported)
        }
    }

    /** No dependency cycles between module slices. */
    @Test
    fun `module slices are free of cycles`() {
        slices().matching("(*)..").namingSlices("module \$1")
            .should().beFreeOfCycles()
            .check(imported)
    }

    /**
     * No cross-module @Entity coupling. Each module's entity classes are private to it — an
     * association ACROSS module boundaries (`@ManyToOne` to another module's entity) is exactly
     * what the ORM would let you introduce and the no-cross-module-FK rule forbids. Only the
     * owning package (and Hibernate/Panache infra) may depend on an entity; no other module may.
     */
    @Test
    fun `entities are not imported across modules`() {
        val entities = mapOf(
            "Player" to "accounts",
            "Character" to "characters",
            "Holding" to "inventory",
            "HoldingId" to "inventory",
        )
        entities.forEach { (name, pkg) ->
            classes().that().haveSimpleName(name).and().resideInAPackage("$pkg..")
                .should().onlyHaveDependentClassesThat().resideOutsideOfPackages(
                    "accounts..", "characters..", "inventory..", "admin..", "app..",
                ).orShould().resideInAPackage("$pkg..")
                .allowEmptyShould(true)
                .because("$name is $pkg's @Entity; no OTHER module may import it (no cross-module entity coupling)")
                .check(imported)
        }
    }
}
