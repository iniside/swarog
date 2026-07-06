package characters

import io.quarkus.runtime.StartupEvent
import java.util.Optional
import org.junit.jupiter.api.Assertions.assertDoesNotThrow
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Test
import platform.RoleConfig

/**
 * P0-ROLES: [CharactersEdgeServer.start] must return BEFORE touching cert/transport when this process
 * does not host a split characters QUIC server — i.e. the monolith (`roles=all`, takes the in-process
 * local branch) or any process without the `characters` role. If the gate were broken, `start()` would
 * reach `certThumbprint.orElseThrow` (no cert configured here) and throw, or build an
 * `MsQuicServerTransport`. Asserting NO throw with NO cert set + `transport` staying null proves the
 * early return, with zero native QUIC setup.
 */
class CharactersEdgeServerStartGateTest {

    @Test
    fun `monolith start returns before any cert or transport setup`() {
        val server = edgeServer(RoleConfig(setOf("all")))   // isMonolith() == true

        assertDoesNotThrow { server.start(StartupEvent()) }
        assertNull(server.transportForTest(), "monolith must not stand up a QUIC transport")
    }

    @Test
    fun `a process without the characters role also skips the QUIC server`() {
        val server = edgeServer(RoleConfig(setOf("inventory")))   // not monolith, characters inactive

        assertDoesNotThrow { server.start(StartupEvent()) }
        assertNull(server.transportForTest(), "a non-characters process must not stand up a QUIC transport")
    }

    /** No cert thumbprint configured — the branch under test must never reach the cert requirement. */
    private fun edgeServer(roleConfig: RoleConfig): CharactersEdgeServer =
        CharactersEdgeServer(
            roleConfig = roleConfig,
            local = LocalPlayerCharacters(),
            port = 9100,
            certThumbprint = Optional.empty(),
        )
}
