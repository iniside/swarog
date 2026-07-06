package charactersclient

import org.junit.jupiter.api.Assertions.assertThrows
import org.junit.jupiter.api.Test

/**
 * Pure unit test of [EdgeRemotePlayerCharacters]'s `host:port` parsing. `init` validates and assigns
 * `host`/`port` BEFORE the `transport` property (an `MsQuicClientTransport`, which eagerly loads the
 * native msquic library) is initialized, so a malformed target fails fast here without ever touching
 * the native transport — no msquic runtime dependency for this test. A well-formed target is
 * deliberately NOT exercised here: it would construct the real native transport, which is edge's
 * concern (see edge's MsQuicFoundationTest/MsQuicEchoTest), not this parsing rule's.
 */
class EdgeRemotePlayerCharactersTest {

    @Test
    fun `rejects a target with no colon at all`() {
        assertThrows(IllegalArgumentException::class.java) { EdgeRemotePlayerCharacters("localhost") }
    }

    @Test
    fun `rejects a target with a trailing colon and no port`() {
        assertThrows(IllegalArgumentException::class.java) { EdgeRemotePlayerCharacters("localhost:") }
    }

    @Test
    fun `rejects a target with a leading colon and no host`() {
        assertThrows(IllegalArgumentException::class.java) { EdgeRemotePlayerCharacters(":9100") }
    }
}
