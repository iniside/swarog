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
}
