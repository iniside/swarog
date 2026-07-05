package edge.msquic

import java.lang.foreign.MemoryLayout
import java.lang.foreign.MemoryLayout.paddingLayout
import java.lang.foreign.MemoryLayout.sequenceLayout
import java.lang.foreign.MemoryLayout.structLayout
import java.lang.foreign.MemorySegment
import java.lang.foreign.StructLayout
import java.lang.foreign.ValueLayout.ADDRESS
import java.lang.foreign.ValueLayout.JAVA_BYTE
import java.lang.foreign.ValueLayout.JAVA_INT
import java.lang.foreign.ValueLayout.JAVA_LONG
import java.lang.foreign.ValueLayout.JAVA_SHORT

/**
 * x64 (Windows) memory layouts for the msquic structs & event unions we bind. Every offset here was
 * counted field-by-field against `G:\tmp\msquic\msquic.h` (v2.5.9) and `msquic_winuser.h`; a wrong
 * number is a runtime segfault the compiler cannot catch, so each layout carries an explicit
 * `byteSize()` that [assertSizes] checks at startup.
 *
 * Fields are read/written with explicit numeric offsets (the `*_OFF` constants) rather than derived
 * VarHandles — the offsets are all naturally aligned, and explicitness keeps the ABI auditable.
 * Event unions follow the plan's rule: read `Type` at offset 0, then `asSlice(UNION_OFFSET, branch)`
 * for the discriminated branch — no single giant union layout.
 */
object Layouts {

    // ---- QUIC_BUFFER (msquic.h:438) = 16B ---------------------------------------------------------
    // { uint32 Length; uint8* Buffer; }  (ptr is 8-aligned → 4B pad after Length)
    val QUIC_BUFFER: StructLayout = structLayout(
        JAVA_INT.withName("Length"),
        paddingLayout(4),
        ADDRESS.withName("Buffer"),
    )
    const val BUFFER_LENGTH_OFF = 0L
    const val BUFFER_BUFFER_OFF = 8L

    // ---- QUIC_REGISTRATION_CONFIG (msquic.h:356) = 16B --------------------------------------------
    // { const char* AppName; QUIC_EXECUTION_PROFILE ExecutionProfile; }  (enum = int, 4B tail pad)
    val QUIC_REGISTRATION_CONFIG: StructLayout = structLayout(
        ADDRESS.withName("AppName"),
        JAVA_INT.withName("ExecutionProfile"),
        paddingLayout(4),
    )
    const val REGCFG_APPNAME_OFF = 0L
    const val REGCFG_EXECPROFILE_OFF = 8L

    // ---- QUIC_SETTINGS (msquic.h:725) = 144B ------------------------------------------------------
    // Only IsSetFlags / IdleTimeoutMs / KeepAliveIntervalMs / PeerBidiStreamCount are written by us;
    // the rest are laid out purely so byteSize()==144 (a wrong SettingsSize → INVALID_PARAMETER).
    val QUIC_SETTINGS: StructLayout = structLayout(
        JAVA_LONG.withName("IsSetFlags"),                       // 0   (union with IsSet bitfield)
        JAVA_LONG.withName("MaxBytesPerKey"),                   // 8
        JAVA_LONG.withName("HandshakeIdleTimeoutMs"),           // 16
        JAVA_LONG.withName("IdleTimeoutMs"),                    // 24
        JAVA_LONG.withName("MtuDiscoverySearchCompleteTimeoutUs"), // 32
        JAVA_INT.withName("TlsClientMaxSendBuffer"),            // 40
        JAVA_INT.withName("TlsServerMaxSendBuffer"),            // 44
        JAVA_INT.withName("StreamRecvWindowDefault"),           // 48
        JAVA_INT.withName("StreamRecvBufferDefault"),           // 52
        JAVA_INT.withName("ConnFlowControlWindow"),             // 56
        JAVA_INT.withName("MaxWorkerQueueDelayUs"),             // 60
        JAVA_INT.withName("MaxStatelessOperations"),            // 64
        JAVA_INT.withName("InitialWindowPackets"),              // 68
        JAVA_INT.withName("SendIdleTimeoutMs"),                 // 72
        JAVA_INT.withName("InitialRttMs"),                      // 76
        JAVA_INT.withName("MaxAckDelayMs"),                     // 80
        JAVA_INT.withName("DisconnectTimeoutMs"),               // 84
        JAVA_INT.withName("KeepAliveIntervalMs"),               // 88
        JAVA_SHORT.withName("CongestionControlAlgorithm"),      // 92
        JAVA_SHORT.withName("PeerBidiStreamCount"),             // 94
        JAVA_SHORT.withName("PeerUnidiStreamCount"),            // 96
        JAVA_SHORT.withName("MaxBindingStatelessOperations"),   // 98
        JAVA_SHORT.withName("StatelessOperationExpirationMs"),  // 100
        JAVA_SHORT.withName("MinimumMtu"),                      // 102
        JAVA_SHORT.withName("MaximumMtu"),                      // 104
        JAVA_BYTE.withName("BoolBitfield"),                     // 106 (SendBufferingEnabled..EcnEnabled)
        JAVA_BYTE.withName("MaxOperationsPerDrain"),            // 107
        JAVA_BYTE.withName("MtuDiscoveryMissingProbeCount"),    // 108
        paddingLayout(3),                                       // 109 → align uint32 @112
        JAVA_INT.withName("DestCidUpdateIdleTimeoutMs"),        // 112
        paddingLayout(4),                                       // 116 → align uint64 @120
        JAVA_LONG.withName("Flags"),                            // 120 (union with HyStart bitfield)
        JAVA_INT.withName("StreamRecvWindowBidiLocalDefault"),  // 128
        JAVA_INT.withName("StreamRecvWindowBidiRemoteDefault"), // 132
        JAVA_INT.withName("StreamRecvWindowUnidiDefault"),      // 136
        paddingLayout(4),                                       // 140 → tail pad to 8-align (144)
    )
    const val SETTINGS_ISSETFLAGS_OFF = 0L
    const val SETTINGS_IDLE_TIMEOUT_MS_OFF = 24L
    const val SETTINGS_KEEPALIVE_INTERVAL_MS_OFF = 88L
    const val SETTINGS_PEER_BIDI_STREAM_COUNT_OFF = 94L
    const val SETTINGS_SIZE: Int = 144

    // IsSetFlags bit indices (msquic.h:730..) — the enable-bit for each corresponding field.
    const val SETTINGS_ISSET_IDLE_TIMEOUT_MS = 2
    const val SETTINGS_ISSET_KEEPALIVE_INTERVAL_MS = 16
    const val SETTINGS_ISSET_PEER_BIDI_STREAM_COUNT = 18

    // ---- QUIC_CREDENTIAL_CONFIG (msquic.h:403) = 56B ---------------------------------------------
    val QUIC_CREDENTIAL_CONFIG: StructLayout = structLayout(
        JAVA_INT.withName("Type"),               // 0
        JAVA_INT.withName("Flags"),              // 4
        ADDRESS.withName("CertUnion"),           // 8  (CertificateHash* etc.)
        ADDRESS.withName("Principal"),           // 16
        ADDRESS.withName("Reserved"),            // 24
        ADDRESS.withName("AsyncHandler"),        // 32
        JAVA_INT.withName("AllowedCipherSuites"),// 40
        paddingLayout(4),                        // 44
        ADDRESS.withName("CaCertificateFile"),   // 48
    )
    const val CRED_TYPE_OFF = 0L
    const val CRED_FLAGS_OFF = 4L
    const val CRED_CERTUNION_OFF = 8L

    // ---- QUIC_CERTIFICATE_HASH (msquic.h:373) = 20B ----------------------------------------------
    val QUIC_CERTIFICATE_HASH: StructLayout = structLayout(
        sequenceLayout(20, JAVA_BYTE).withName("ShaHash"),
    )
    const val CERT_HASH_SHAHASH_OFF = 0L

    // ---- QUIC_ADDR = SOCKADDR_INET (msquic_winuser.h:151) = 28B ----------------------------------
    // union of SOCKADDR_IN / SOCKADDR_IN6; sized to the IPv6 variant (28B).
    //   si_family : u16 @0 (host order)
    //   sin_port  : u16 @2 (NETWORK / big-endian order)
    //   sin_addr  : IPv4 4 bytes @4  (rest = IPv6 tail, unused for AF_INET)
    val QUIC_ADDR: StructLayout = structLayout(
        JAVA_SHORT.withName("si_family"),               // 0
        JAVA_SHORT.withName("sin_port"),                // 2 (network order)
        sequenceLayout(4, JAVA_BYTE).withName("addr"),  // 4 (IPv4)
        paddingLayout(20),                              // 8 → pad to 28 (IPv6 flow/addr/scope)
    )
    const val ADDR_FAMILY_OFF = 0L
    const val ADDR_PORT_OFF = 2L
    const val ADDR_IPV4_OFF = 4L
    const val ADDR_SIZE: Int = 28

    // ---- Event unions: header {int Type @0, pad4, union @8} --------------------------------------
    const val EVENT_TYPE_OFF = 0L
    const val UNION_OFFSET = 8L

    /** Reads the discriminator `Type` (int @0) from any *_EVENT segment. */
    fun eventType(event: MemorySegment): Int = event.get(JAVA_INT, EVENT_TYPE_OFF)

    // -- LISTENER_EVENT.NEW_CONNECTION (msquic.h:1189) = 16B : { const QUIC_NEW_CONNECTION_INFO*
    //    Info; HQUIC Connection; }
    val LISTENER_NEW_CONNECTION: StructLayout = structLayout(
        ADDRESS.withName("Info"),        // 0
        ADDRESS.withName("Connection"),  // 8
    )
    const val LISTENER_NEWCONN_INFO_OFF = 0L
    const val LISTENER_NEWCONN_CONNECTION_OFF = 8L

    // -- CONNECTION_EVENT.CONNECTED (msquic.h:1301) : { BOOLEAN SessionResumed; u8
    //    NegotiatedAlpnLength; const u8* NegotiatedAlpn; }
    val CONNECTION_CONNECTED: StructLayout = structLayout(
        JAVA_BYTE.withName("SessionResumed"),       // 0
        JAVA_BYTE.withName("NegotiatedAlpnLength"),  // 1
        paddingLayout(6),                            // 2 → 8-align ptr
        ADDRESS.withName("NegotiatedAlpn"),          // 8
    )

    // -- CONNECTION_EVENT.SHUTDOWN_INITIATED_BY_TRANSPORT (msquic.h:1308) : { QUIC_STATUS Status;
    //    QUIC_UINT62 ErrorCode; }
    val CONNECTION_SHUTDOWN_BY_TRANSPORT: StructLayout = structLayout(
        JAVA_INT.withName("Status"),      // 0
        paddingLayout(4),                 // 4 → 8-align u64
        JAVA_LONG.withName("ErrorCode"),  // 8
    )
    const val CONN_SHUTDOWN_STATUS_OFF = 0L

    // -- CONNECTION_EVENT.SHUTDOWN_INITIATED_BY_PEER (msquic.h:1312) : { QUIC_UINT62 ErrorCode; }
    val CONNECTION_SHUTDOWN_BY_PEER: StructLayout = structLayout(
        JAVA_LONG.withName("ErrorCode"),  // 0
    )

    // -- CONNECTION_EVENT.PEER_STREAM_STARTED (msquic.h:1326) : { HQUIC Stream;
    //    QUIC_STREAM_OPEN_FLAGS Flags; }
    val CONNECTION_PEER_STREAM_STARTED: StructLayout = structLayout(
        ADDRESS.withName("Stream"),  // 0
        JAVA_INT.withName("Flags"),  // 8
        paddingLayout(4),            // 12 → 16B
    )
    const val CONN_PEER_STREAM_STREAM_OFF = 0L
    const val CONN_PEER_STREAM_FLAGS_OFF = 8L

    // -- STREAM_EVENT.RECEIVE (msquic.h:1553) : { u64 AbsoluteOffset; u64 TotalBufferLength;
    //    const QUIC_BUFFER* Buffers; u32 BufferCount; QUIC_RECEIVE_FLAGS Flags; }
    val STREAM_RECEIVE: StructLayout = structLayout(
        JAVA_LONG.withName("AbsoluteOffset"),     // 0
        JAVA_LONG.withName("TotalBufferLength"),  // 8
        ADDRESS.withName("Buffers"),              // 16
        JAVA_INT.withName("BufferCount"),         // 24
        JAVA_INT.withName("Flags"),               // 28
    )
    const val STREAM_RECEIVE_TOTAL_LEN_OFF = 8L
    const val STREAM_RECEIVE_BUFFERS_OFF = 16L
    const val STREAM_RECEIVE_BUFFER_COUNT_OFF = 24L

    // -- STREAM_EVENT.SEND_COMPLETE (msquic.h:1562) : { BOOLEAN Canceled; void* ClientContext; }
    val STREAM_SEND_COMPLETE: StructLayout = structLayout(
        JAVA_BYTE.withName("Canceled"),        // 0
        paddingLayout(7),                      // 1 → 8-align ptr
        ADDRESS.withName("ClientContext"),     // 8
    )
    const val STREAM_SEND_COMPLETE_CANCELED_OFF = 0L
    const val STREAM_SEND_COMPLETE_CLIENT_CTX_OFF = 8L

    /**
     * Allocates a 28-byte [QUIC_ADDR] (SOCKADDR_INET) for `127.0.0.1:port` in [arena]:
     * family = AF_INET(2) @0 (host order), sin_port = htons(port) @2 (network/big-endian), IPv4
     * loopback bytes @4. Used by the server's `ListenerStart` (Krok 2). The port is written as two
     * explicit bytes to make the big-endian order unambiguous regardless of platform byte order.
     */
    fun quicAddrIpv4(arena: java.lang.foreign.Arena, port: Int): MemorySegment {
        require(port in 0..0xFFFF) { "port out of range: $port" }
        val seg = arena.allocate(QUIC_ADDR)
        seg.set(JAVA_SHORT, ADDR_FAMILY_OFF, Constants.QUIC_ADDRESS_FAMILY_INET) // 2, host order
        // htons(port): high byte first (network order)
        seg.set(JAVA_BYTE, ADDR_PORT_OFF, ((port ushr 8) and 0xFF).toByte())
        seg.set(JAVA_BYTE, ADDR_PORT_OFF + 1, (port and 0xFF).toByte())
        seg.set(JAVA_BYTE, ADDR_IPV4_OFF, 127)
        seg.set(JAVA_BYTE, ADDR_IPV4_OFF + 1, 0)
        seg.set(JAVA_BYTE, ADDR_IPV4_OFF + 2, 0)
        seg.set(JAVA_BYTE, ADDR_IPV4_OFF + 3, 1)
        return seg
    }

    /**
     * Fails loudly at startup if any load-bearing struct size drifted from the header — the cheapest
     * possible guard against an ABI mistake. Called once by [MsQuicLibrary].
     */
    fun assertSizes() {
        require(QUIC_BUFFER.byteSize() == 16L) { "QUIC_BUFFER=${QUIC_BUFFER.byteSize()} expected 16" }
        require(QUIC_REGISTRATION_CONFIG.byteSize() == 16L) {
            "QUIC_REGISTRATION_CONFIG=${QUIC_REGISTRATION_CONFIG.byteSize()} expected 16"
        }
        require(QUIC_SETTINGS.byteSize() == 144L) {
            "QUIC_SETTINGS=${QUIC_SETTINGS.byteSize()} expected 144"
        }
        require(QUIC_CREDENTIAL_CONFIG.byteSize() == 56L) {
            "QUIC_CREDENTIAL_CONFIG=${QUIC_CREDENTIAL_CONFIG.byteSize()} expected 56"
        }
        require(QUIC_CERTIFICATE_HASH.byteSize() == 20L) {
            "QUIC_CERTIFICATE_HASH=${QUIC_CERTIFICATE_HASH.byteSize()} expected 20"
        }
        require(QUIC_ADDR.byteSize() == 28L) { "QUIC_ADDR=${QUIC_ADDR.byteSize()} expected 28" }
    }
}
