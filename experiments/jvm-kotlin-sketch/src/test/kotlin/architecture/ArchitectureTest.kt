package architecture

import com.tngtech.archunit.core.domain.JavaClasses
import com.tngtech.archunit.core.importer.ClassFileImporter
import com.tngtech.archunit.core.importer.ImportOption
import com.tngtech.archunit.lang.syntax.ArchRuleDefinition.classes
import com.tngtech.archunit.lang.syntax.ArchRuleDefinition.noClasses
import com.tngtech.archunit.library.dependencies.SlicesRuleDefinition.slices
import org.junit.jupiter.api.Test

/**
 * The CLAUDE.md hard constraints, encoded as architecture tests. Everything compiles into ONE jar,
 * so the boundary is NOT enforced by the classpath — these rules enforce it at TEST time instead.
 * Break a rule (e.g. import another module's impl) and `gradle test` goes red, naming the violation.
 */
class ArchitectureTest {

    private val imported: JavaClasses = ClassFileImporter()
        .withImportOption(ImportOption.DoNotIncludeTests())
        .importPackages("core", "accounts", "characters", "inventory", "admin", "app")

    /** Constraint #1: the core never imports a module. Dependency only points module -> core. */
    @Test
    fun `core stays game-agnostic`() {
        noClasses().that().resideInAPackage("core..")
            .should().dependOnClassesThat()
            .resideInAnyPackage("accounts..", "characters..", "inventory..", "admin..", "app..")
            .because("the core has no game knowledge; dependency only ever points module -> core")
            .check(imported)
    }

    /**
     * Constraints #1/#2/#4: a module's implementation is private. Only `app` (the composition root,
     * = Go's cmd/server/main.go) may reference a concrete *Module; everyone else depends on the
     * module's *api / *events contract, never on its impl. Same-package access (its own lambdas) is fine.
     */
    @Test
    fun `module impls are reachable only from app`() {
        val modules = mapOf(
            "AccountsModule" to "accounts",
            "CharactersModule" to "characters",
            "InventoryModule" to "inventory",
            "AdminModule" to "admin",
        )
        modules.forEach { (name, pkg) ->
            classes().that().haveSimpleName(name)
                .should().onlyHaveDependentClassesThat().resideInAnyPackage("app..", "$pkg..")
                .allowEmptyShould(true)
                .because("$name is implementation; only app wires it, others use its contract")
                .check(imported)
        }
    }

    /** Constraint #3: no dependency cycles between module slices. */
    @Test
    fun `module slices are free of cycles`() {
        slices().matching("(*)..").namingSlices("module \$1")
            .should().beFreeOfCycles()
            .check(imported)
    }
}
