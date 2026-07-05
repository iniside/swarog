package edge.msquic

import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import java.lang.foreign.Arena
import java.lang.foreign.MemorySegment
import java.lang.foreign.ValueLayout.ADDRESS
import java.lang.foreign.ValueLayout.JAVA_INT
import java.lang.foreign.ValueLayout.JAVA_LONG
import java.lang.foreign.ValueLayout.JAVA_SHORT

/**
 * Krok 1 gate: prove the whole FFM foundation is ABI-correct against the real msquic.dll WITHOUT any
 * transport or server certificate. The sequence is:
 *   MsQuicLibrary load → RegistrationOpen(NULL) → ConfigurationOpen(real 144B QUIC_SETTINGS + ALPN
 *   "edge") → ConfigurationLoadCredential(client NONE|NO_VALIDATION) → close everything.
 *
 * A wrong QUIC_SETTINGS size or a mis-laid IsSetFlags/field offset makes ConfigurationOpen return
 * INVALID_PARAMETER (negative), so a green run validates the settings layout end-to-end.
 */
class MsQuicFoundationTest {

    private fun hex(status: Int) = "0x%08x".format(status)

    @Test
    fun layoutSizesMatchHeader() {
        // Fails loudly if any struct drifted; also independently asserts the load-bearing 144.
        Layouts.assertSizes()
        assertEquals(144L, Layouts.QUIC_SETTINGS.byteSize(), "QUIC_SETTINGS must be 144 bytes")
        assertEquals(28L, Layouts.QUIC_ADDR.byteSize(), "QUIC_ADDR must be 28 bytes")
        assertEquals(56L, Layouts.QUIC_CREDENTIAL_CONFIG.byteSize())
        assertEquals(16L, Layouts.QUIC_BUFFER.byteSize())
    }

    @Test
    fun quicAddrIpv4WritesFamilyPortAndLoopback() {
        Arena.ofConfined().use { arena ->
            val addr = Layouts.quicAddrIpv4(arena, 9100)
            assertEquals(28L, addr.byteSize())
            assertEquals(Constants.QUIC_ADDRESS_FAMILY_INET, addr.get(JAVA_SHORT, Layouts.ADDR_FAMILY_OFF))
            // 9100 = 0x238C → network order bytes 0x23, 0x8C
            assertEquals(0x23.toByte(), addr.get(java.lang.foreign.ValueLayout.JAVA_BYTE, 2))
            assertEquals(0x8C.toByte(), addr.get(java.lang.foreign.ValueLayout.JAVA_BYTE, 3))
            assertEquals(127.toByte(), addr.get(java.lang.foreign.ValueLayout.JAVA_BYTE, 4))
            assertEquals(1.toByte(), addr.get(java.lang.foreign.ValueLayout.JAVA_BYTE, 7))
        }
    }

    @Test
    fun opensRegistrationConfigurationAndLoadsClientCredential() {
        MsQuicLibrary().use { lib ->
            val api = lib.api
            Arena.ofConfined().use { arena ->
                // --- RegistrationOpen(NULL config → defaults) ---------------------------------------
                val regOut = arena.allocate(ADDRESS)
                val regStatus = api.registrationOpen(MemorySegment.NULL, regOut)
                assertTrue(Constants.succeeded(regStatus)) { "RegistrationOpen ${hex(regStatus)}" }
                val registration = regOut.get(ADDRESS, 0)
                assertTrue(registration.address() != 0L) { "null registration handle" }

                // --- ALPN "edge" as a QUIC_BUFFER (Length=4, no NUL) --------------------------------
                val alpnBytes = "edge".toByteArray(Charsets.US_ASCII)
                assertEquals(4, alpnBytes.size)
                val alpnData = arena.allocate(alpnBytes.size.toLong())
                MemorySegment.copy(alpnBytes, 0, alpnData, java.lang.foreign.ValueLayout.JAVA_BYTE, 0, alpnBytes.size)
                val alpnBuf = arena.allocate(Layouts.QUIC_BUFFER)
                alpnBuf.set(JAVA_INT, Layouts.BUFFER_LENGTH_OFF, alpnBytes.size)
                alpnBuf.set(ADDRESS, Layouts.BUFFER_BUFFER_OFF, alpnData)

                // --- real 144-byte QUIC_SETTINGS ----------------------------------------------------
                val settings = arena.allocate(Layouts.QUIC_SETTINGS) // zero-initialized
                val isSet = (1L shl Layouts.SETTINGS_ISSET_IDLE_TIMEOUT_MS) or
                    (1L shl Layouts.SETTINGS_ISSET_KEEPALIVE_INTERVAL_MS) or
                    (1L shl Layouts.SETTINGS_ISSET_PEER_BIDI_STREAM_COUNT)
                settings.set(JAVA_LONG, Layouts.SETTINGS_ISSETFLAGS_OFF, isSet)
                settings.set(JAVA_LONG, Layouts.SETTINGS_IDLE_TIMEOUT_MS_OFF, 30_000L)
                settings.set(JAVA_INT, Layouts.SETTINGS_KEEPALIVE_INTERVAL_MS_OFF, 15_000)
                settings.set(JAVA_SHORT, Layouts.SETTINGS_PEER_BIDI_STREAM_COUNT_OFF, 1)

                val cfgOut = arena.allocate(ADDRESS)
                val cfgStatus = api.configurationOpen(
                    registration, alpnBuf, 1, settings, Layouts.SETTINGS_SIZE, MemorySegment.NULL, cfgOut,
                )
                assertTrue(Constants.succeeded(cfgStatus)) { "ConfigurationOpen ${hex(cfgStatus)}" }
                val configuration = cfgOut.get(ADDRESS, 0)
                assertTrue(configuration.address() != 0L) { "null configuration handle" }

                // --- client credential: Type=NONE, Flags=CLIENT|NO_CERTIFICATE_VALIDATION (0x5) -----
                val cred = arena.allocate(Layouts.QUIC_CREDENTIAL_CONFIG) // zeroed
                cred.set(JAVA_INT, Layouts.CRED_TYPE_OFF, Constants.QUIC_CREDENTIAL_TYPE_NONE)
                cred.set(JAVA_INT, Layouts.CRED_FLAGS_OFF, Constants.QUIC_CREDENTIAL_FLAGS_CLIENT_NO_VALIDATION)
                val credStatus = api.configurationLoadCredential(configuration, cred)
                assertTrue(Constants.succeeded(credStatus)) { "LoadCredential ${hex(credStatus)}" }

                // --- teardown (reverse order) -------------------------------------------------------
                api.configurationClose(configuration)
                api.registrationClose(registration)
            }
        }
    }
}
