package edge.msquic

import java.lang.foreign.Arena
import java.lang.foreign.MemorySegment
import java.lang.foreign.ValueLayout.ADDRESS
import java.lang.foreign.ValueLayout.JAVA_BYTE
import java.lang.foreign.ValueLayout.JAVA_INT
import java.lang.foreign.ValueLayout.JAVA_LONG
import java.lang.foreign.ValueLayout.JAVA_SHORT

/**
 * Shared native-memory builders and event helpers used by both [MsQuicServerTransport] and
 * [MsQuicClientTransport]. Keeping the (identical) 144-byte SETTINGS and ALPN construction here means
 * server and client cannot drift apart on the settings that MUST match on both ends (KeepAlive, idle
 * timeout, ALPN "edge").
 */
object MsQuicSupport {

    /**
     * The event segment handed to an upcall arrives as a zero-length address; widen it so every union
     * branch we read is in bounds. 64 bytes comfortably covers the largest branch we touch (STREAM
     * RECEIVE reaches offset 8+28=36). No lifecycle — valid only for the callback's duration.
     */
    const val EVENT_MAX_BYTES = 64L

    fun reinterpretEvent(event: MemorySegment): MemorySegment = event.reinterpret(EVENT_MAX_BYTES)

    /**
     * Allocates the 144-byte [Layouts.QUIC_SETTINGS] both sides use: IdleTimeoutMs=30000,
     * KeepAliveIntervalMs=15000 (MUST be set on the client too, or an idle connection times out between
     * calls), PeerBidiStreamCount=1 (one bidi stream per connection). IsSetFlags enables exactly those
     * three fields.
     */
    fun buildSettings(arena: Arena): MemorySegment {
        val settings = arena.allocate(Layouts.QUIC_SETTINGS) // zero-initialized
        val isSet = (1L shl Layouts.SETTINGS_ISSET_IDLE_TIMEOUT_MS) or
            (1L shl Layouts.SETTINGS_ISSET_KEEPALIVE_INTERVAL_MS) or
            (1L shl Layouts.SETTINGS_ISSET_PEER_BIDI_STREAM_COUNT)
        settings.set(JAVA_LONG, Layouts.SETTINGS_ISSETFLAGS_OFF, isSet)
        settings.set(JAVA_LONG, Layouts.SETTINGS_IDLE_TIMEOUT_MS_OFF, 30_000L)
        settings.set(JAVA_INT, Layouts.SETTINGS_KEEPALIVE_INTERVAL_MS_OFF, 15_000)
        settings.set(JAVA_SHORT, Layouts.SETTINGS_PEER_BIDI_STREAM_COUNT_OFF, 1)
        return settings
    }

    /**
     * Builds a single [Layouts.QUIC_BUFFER] pointing at the ALPN bytes (e.g. "edge", Length=4, NO NUL).
     * The returned buffer AND the ALPN bytes are allocated in [arena]; both must outlive the
     * Configuration/Listener that reference the ALPN, so [arena] is the transport-scoped arena.
     */
    fun buildAlpn(arena: Arena, alpn: String): MemorySegment {
        val bytes = alpn.toByteArray(Charsets.US_ASCII)
        val data = arena.allocate(bytes.size.toLong())
        MemorySegment.copy(bytes, 0, data, JAVA_BYTE, 0L, bytes.size)
        val buf = arena.allocate(Layouts.QUIC_BUFFER)
        buf.set(JAVA_INT, Layouts.BUFFER_LENGTH_OFF, bytes.size)
        buf.set(ADDRESS, Layouts.BUFFER_BUFFER_OFF, data)
        return buf
    }
}
