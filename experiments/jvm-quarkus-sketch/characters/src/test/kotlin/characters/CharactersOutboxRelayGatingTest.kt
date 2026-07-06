package characters

import java.lang.reflect.InvocationHandler
import java.lang.reflect.Proxy
import java.util.Optional
import javax.sql.DataSource
import org.junit.jupiter.api.Assertions.assertDoesNotThrow
import org.junit.jupiter.api.Test
import platform.RoleConfig

/**
 * P0-ROLES (behavioral gating, NOT schema state): a process that does NOT run the `characters` role
 * must NOT drain `characters.outbox` — otherwise, since every module shares ONE Postgres, an
 * inventory-only process would double-publish characters events. Proven DB-free (à la
 * [inventory.InventoryAuthorizationUnitTest]): the [DataSource] is a dynamic proxy that fails the test
 * the instant ANY method is called on it, so a green run means `drain()` returned at the `isActive`
 * gate BEFORE it could read/mark a single outbox row (and thus before any HTTP fan-out).
 */
class CharactersOutboxRelayGatingTest {

    @Test
    fun `drain short-circuits when the characters role is inactive - no DB, no HTTP`() {
        val relay = CharactersOutboxRelay(
            db = untouchable(DataSource::class.java),
            roleConfig = RoleConfig(setOf("inventory")),   // characters NOT active in this process
            createdSubscribers = Optional.empty(),
            deletedSubscribers = Optional.empty(),
        )

        assertDoesNotThrow { relay.drain() }
    }

    /** A dynamic proxy that fails loudly if ANY method is called on it. */
    private fun <T> untouchable(type: Class<T>): T {
        val handler = InvocationHandler { _, method, _ ->
            throw AssertionError("inactive-role drain must not touch ${type.simpleName}.${method.name}")
        }
        @Suppress("UNCHECKED_CAST")
        return Proxy.newProxyInstance(type.classLoader, arrayOf(type), handler) as T
    }
}
