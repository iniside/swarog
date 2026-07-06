package inventory

import characters.charactersapi.PlayerCharacters
import io.quarkus.runtime.StartupEvent
import jakarta.persistence.EntityManager
import java.lang.reflect.InvocationHandler
import java.lang.reflect.Proxy
import java.util.UUID
import javax.sql.DataSource
import org.junit.jupiter.api.Assertions.assertDoesNotThrow
import org.junit.jupiter.api.Test
import platform.RoleConfig

/**
 * P0-ROLES: [InventoryModule.migrate] must consult the role gate and perform NO DDL when the
 * `inventory` role is inactive in this process (its schema is another process's property then). Proven
 * DB-free like [InventoryAuthorizationUnitTest]: `db`/`em` are dynamic proxies that fail the test if
 * ANY method is invoked, so a green run means `migrate()` early-returned at `!isActive("inventory")`
 * BEFORE opening a connection or creating a statement.
 */
class InventoryMigrateGateTest {

    private class NeverOwns : PlayerCharacters {
        override fun ownerOf(characterId: Long): UUID? = null
    }

    @Test
    fun `migrate performs no DDL when the inventory role is inactive`() {
        val inventory = InventoryModule(
            db = untouchable(DataSource::class.java),
            em = untouchable(EntityManager::class.java),
            characters = NeverOwns(),
            roleConfig = RoleConfig(setOf("accounts")),   // inventory NOT active in this process
        )

        assertDoesNotThrow { inventory.migrate(StartupEvent()) }
    }

    /** A dynamic proxy that fails loudly if ANY method is called on it. */
    private fun <T> untouchable(type: Class<T>): T {
        val handler = InvocationHandler { _, method, _ ->
            throw AssertionError("inactive-role migrate must not touch ${type.simpleName}.${method.name}")
        }
        @Suppress("UNCHECKED_CAST")
        return Proxy.newProxyInstance(type.classLoader, arrayOf(type), handler) as T
    }
}
