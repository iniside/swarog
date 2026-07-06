package inventory

import characters.charactersapi.PlayerCharacters
import jakarta.persistence.EntityManager
import java.lang.reflect.InvocationHandler
import java.lang.reflect.Proxy
import java.util.UUID
import javax.sql.DataSource
import org.junit.jupiter.api.Assertions.assertThrows
import org.junit.jupiter.api.Test
import platform.RoleConfig

/**
 * Pure unit test (no DB, no Quarkus container): [InventoryModule.add]'s authorization check for a
 * CHARACTER owner is a decision made BEFORE any persistence call — `characters.ownerOf(...) == null`
 * short-circuits to `error(...)` and never reaches `grant()`. Proven here by wiring `db`/`em` to
 * dynamic proxies that fail the test the instant a single method is invoked on them: a green run
 * means the rejection path is provably DB-free, not merely "the exception happened to be thrown".
 */
class InventoryAuthorizationUnitTest {

    private class NeverOwns : PlayerCharacters {
        override fun ownerOf(characterId: Long): UUID? = null
    }

    @Test
    fun `add rejects an unknown character without touching persistence`() {
        val inventory = InventoryModule(
            db = untouchable(DataSource::class.java),
            em = untouchable(EntityManager::class.java),
            characters = NeverOwns(),
            roleConfig = RoleConfig(setOf("all")),
        )

        assertThrows(IllegalStateException::class.java) {
            inventory.add(Owner(OwnerType.CHARACTER, "42"), "sword", 1)
        }
    }

    /** A dynamic proxy that fails loudly if ANY method is called on it. */
    private fun <T> untouchable(type: Class<T>): T {
        val handler = InvocationHandler { _, method, _ ->
            throw AssertionError("must not touch ${type.simpleName}.${method.name} in the rejection branch")
        }
        @Suppress("UNCHECKED_CAST")
        return Proxy.newProxyInstance(type.classLoader, arrayOf(type), handler) as T
    }
}
