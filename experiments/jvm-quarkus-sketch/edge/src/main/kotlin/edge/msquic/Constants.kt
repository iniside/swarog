package edge.msquic

/**
 * msquic constants, each verified against `G:\tmp\msquic\msquic.h` (v2.5.9) and `msquic_winuser.h`.
 * Only the values Kroki 1-3 consume are listed; add more additively as the transport grows.
 */
object Constants {

    // --- QUIC_STATUS (msquic_winuser.h; HRESULT) --------------------------------------------------
    // Windows: success is a NON-NEGATIVE signed int (S_OK=0, PENDING=0x703E5 both >= 0);
    // failures set the high bit and read as negative. So the universal test is `status >= 0`.
    const val QUIC_STATUS_SUCCESS: Int = 0
    const val QUIC_STATUS_PENDING: Int = 0x703E5
    const val QUIC_STATUS_INTERNAL_ERROR: Int = 0x80410003.toInt() // returned from upcalls on Throwable

    /** The one status predicate used everywhere (Windows/schannel only — see plan Ryzyka). */
    fun succeeded(status: Int): Boolean = status >= 0

    // --- QUIC_EXECUTION_PROFILE (msquic.h:83) ------------------------------------------------------
    const val QUIC_EXECUTION_PROFILE_LOW_LATENCY: Int = 0

    // --- QUIC_CREDENTIAL_TYPE (msquic.h:116) -------------------------------------------------------
    const val QUIC_CREDENTIAL_TYPE_NONE: Int = 0
    const val QUIC_CREDENTIAL_TYPE_CERTIFICATE_HASH: Int = 1

    // --- QUIC_CREDENTIAL_FLAGS (msquic.h:126) ------------------------------------------------------
    const val QUIC_CREDENTIAL_FLAG_NONE: Int = 0x00000000
    const val QUIC_CREDENTIAL_FLAG_CLIENT: Int = 0x00000001
    const val QUIC_CREDENTIAL_FLAG_NO_CERTIFICATE_VALIDATION: Int = 0x00000004
    /** Convenience: the client credential used to skip server-cert validation on localhost = 0x5. */
    const val QUIC_CREDENTIAL_FLAGS_CLIENT_NO_VALIDATION: Int =
        QUIC_CREDENTIAL_FLAG_CLIENT or QUIC_CREDENTIAL_FLAG_NO_CERTIFICATE_VALIDATION

    // --- QUIC_STREAM_OPEN_FLAGS / START_FLAGS / SEND_FLAGS -----------------------------------------
    const val QUIC_STREAM_OPEN_FLAG_NONE: Int = 0   // bidirectional
    const val QUIC_STREAM_START_FLAG_NONE: Int = 0
    const val QUIC_SEND_FLAG_NONE: Int = 0

    // --- QUIC_STREAM_SHUTDOWN_FLAGS (msquic.h:221) / QUIC_CONNECTION_SHUTDOWN_FLAGS (msquic.h:170) --
    const val QUIC_STREAM_SHUTDOWN_FLAG_GRACEFUL: Int = 0x0001 // cleanly closes the send path
    const val QUIC_CONNECTION_SHUTDOWN_FLAG_NONE: Int = 0x0000

    // --- QUIC_ADDRESS_FAMILY (msquic_winuser.h:159) ------------------------------------------------
    const val QUIC_ADDRESS_FAMILY_UNSPEC: Short = 0
    const val QUIC_ADDRESS_FAMILY_INET: Short = 2   // AF_INET

    // --- QUIC_LISTENER_EVENT_TYPE (msquic.h:1180) --------------------------------------------------
    const val QUIC_LISTENER_EVENT_NEW_CONNECTION: Int = 0
    const val QUIC_LISTENER_EVENT_STOP_COMPLETE: Int = 1
    const val QUIC_LISTENER_EVENT_DOS_MODE_CHANGED: Int = 2

    // --- QUIC_CONNECTION_EVENT_TYPE (msquic.h:1274) ------------------------------------------------
    const val QUIC_CONNECTION_EVENT_CONNECTED: Int = 0
    const val QUIC_CONNECTION_EVENT_SHUTDOWN_INITIATED_BY_TRANSPORT: Int = 1
    const val QUIC_CONNECTION_EVENT_SHUTDOWN_INITIATED_BY_PEER: Int = 2
    const val QUIC_CONNECTION_EVENT_SHUTDOWN_COMPLETE: Int = 3
    const val QUIC_CONNECTION_EVENT_PEER_STREAM_STARTED: Int = 6

    // --- QUIC_STREAM_EVENT_TYPE (msquic.h:1527) ----------------------------------------------------
    const val QUIC_STREAM_EVENT_START_COMPLETE: Int = 0
    const val QUIC_STREAM_EVENT_RECEIVE: Int = 1
    const val QUIC_STREAM_EVENT_SEND_COMPLETE: Int = 2
    const val QUIC_STREAM_EVENT_PEER_SEND_SHUTDOWN: Int = 3
    const val QUIC_STREAM_EVENT_SHUTDOWN_COMPLETE: Int = 7

    // --- QUIC api version passed to MsQuicOpenVersion ----------------------------------------------
    const val QUIC_API_VERSION_2: Int = 2
}
