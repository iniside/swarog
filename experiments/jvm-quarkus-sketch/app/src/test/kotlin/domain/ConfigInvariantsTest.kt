package domain

import io.quarkus.test.junit.QuarkusTest
import org.eclipse.microprofile.config.ConfigProvider
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

/**
 * P0.5 — regression tests for two of THIS session's own bug fixes, read at RUNTIME from the resolved
 * config (not from a log line):
 *  - Seed-off-under-test: `app.seed.enabled` must resolve to false in the @QuarkusTest config, so the
 *    destructive demo [app.Seed] observer (it TRUNCATEs every module's tables) can never re-run under
 *    `./gradlew test` and wipe the dev DB.
 *  - ArC-flags-active: the two build-time ArC canaries must resolve to boolean `true`. A `.properties`
 *    file has NO inline comments — a trailing `# ...` becomes part of the value, so `true # x` parses
 *    to boolean false and SILENTLY disables the gate (SRCFG01008). Reading them as Boolean here fails
 *    the moment that mistake is re-introduced.
 */
@QuarkusTest
class ConfigInvariantsTest {

    private fun boolean(name: String): Boolean =
        ConfigProvider.getConfig().getValue(name, Boolean::class.javaObjectType)

    @Test
    fun `app_seed_enabled resolves false under test so Seed never truncates the dev DB`() {
        assertFalse(boolean("app.seed.enabled"))
    }

    @Test
    fun `the ArC strictness flags resolve to boolean true (no inline-comment reparse to false)`() {
        assertTrue(boolean("quarkus.arc.detect-wrong-annotations"), "detect-wrong-annotations must be true")
        assertTrue(boolean("quarkus.arc.fail-on-intercepted-private-method"), "fail-on-intercepted-private-method must be true")
    }
}
