package gateway

import io.quarkus.runtime.StartupEvent
import java.util.Optional
import org.junit.jupiter.api.Assertions.assertDoesNotThrow
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Test
import platform.RoleConfig

/**
 * Role-gating twin of `CharactersEdgeServerStartGateTest`: [GatewayEdgeServer.start] must return BEFORE
 * touching cert/transport/[RoutedBackend] when this process is not a gateway — the monolith (`roles=all`
 * ⇒ isMonolith) or any process without the `gateway` role. If the gate were broken, `start()` would
 * reach `certThumbprint.orElseThrow` (no cert here) or eagerly build native transports. Asserting NO
 * throw with NO cert + `transport` staying null proves the early return with zero native QUIC setup.
 */
class GatewayEdgeServerStartGateTest {

    @Test
    fun `monolith start returns before any cert or transport setup`() {
        val server = gateway(RoleConfig(setOf("all")))   // isMonolith() == true

        assertDoesNotThrow { server.start(StartupEvent()) }
        assertNull(server.transportForTest(), "monolith must not stand up a gateway QUIC transport")
    }

    @Test
    fun `a process without the gateway role also skips the router`() {
        val server = gateway(RoleConfig(setOf("inventory")))   // not monolith, gateway inactive

        assertDoesNotThrow { server.start(StartupEvent()) }
        assertNull(server.transportForTest(), "a non-gateway process must not stand up a gateway QUIC transport")
    }

    /** No cert thumbprint configured — the branch under test must never reach the cert requirement. */
    private fun gateway(roleConfig: RoleConfig): GatewayEdgeServer =
        GatewayEdgeServer(
            roleConfig = roleConfig,
            port = 9200,
            charactersTarget = "localhost:9100",
            inventoryTarget = "localhost:9101",
            certThumbprint = Optional.empty(),
        )
}
