package edge.msquic

import org.junit.jupiter.api.Assertions.assertThrows
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

/**
 * Pure-unit test of the [MsQuicServerTransport] ctor `require`. The `thumbprint` property is initialized
 * FIRST (before `lib = MsQuicLibrary()` and any upcall stub), so a wrong-length thumbprint fails the
 * `require` before a single native call — making this constructable and assertable with no msquic.dll.
 */
class MsQuicServerTransportCtorTest {

    @Test
    fun `a wrong-length cert thumbprint is rejected before any native call`() {
        // "ABCD" parses to 2 bytes; a valid SHA-1 thumbprint is 20.
        val ex = assertThrows(IllegalArgumentException::class.java) {
            MsQuicServerTransport(port = 0, certThumbprintHex = "ABCD")
        }
        assertTrue(requireNotNull(ex.message).contains("cert thumbprint must be 20 bytes"))
    }
}
