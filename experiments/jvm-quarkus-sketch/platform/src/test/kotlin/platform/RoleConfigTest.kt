package platform

import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

/** Pure unit test — no container, no DB: [RoleConfig] is a plain string-set parser. */
class RoleConfigTest {

    @Test
    fun `default roles=all activates every module and is the monolith`() {
        val cfg = RoleConfig(setOf("all"))
        assertTrue(cfg.isActive("characters"))
        assertTrue(cfg.isActive("inventory"))
        assertTrue(cfg.isActive("anything-not-a-real-module"))
        assertTrue(cfg.isMonolith())
    }

    @Test
    fun `a scoped role set activates only its own modules and is not the monolith`() {
        val cfg = RoleConfig(setOf("characters", "accounts"))
        assertTrue(cfg.isActive("characters"))
        assertTrue(cfg.isActive("accounts"))
        assertFalse(cfg.isActive("inventory"))
        assertFalse(cfg.isMonolith())
    }

    @Test
    fun `roles=ALL uppercase activates every module and is still the monolith`() {
        // §Bugs #6 regression: a case-sensitive membership check made `ROLES=ALL` (or any non-lowercase
        // spelling) activate NOTHING — a silent ops footgun. Normalization must treat it as the monolith.
        val cfg = RoleConfig(setOf("ALL"))
        assertTrue(cfg.isActive("inventory"))
        assertTrue(cfg.isActive("characters"))
        assertTrue(cfg.isMonolith())
    }

    @Test
    fun `a mixed-case scoped set is matched case-insensitively`() {
        val cfg = RoleConfig(setOf("Characters", "ACCOUNTS"))
        // Configured value case does not matter...
        assertTrue(cfg.isActive("characters"))
        assertTrue(cfg.isActive("accounts"))
        // ...nor does the queried module's case.
        assertTrue(cfg.isActive("CHARACTERS"))
        assertFalse(cfg.isActive("inventory"))
        assertFalse(cfg.isMonolith())
    }
}
