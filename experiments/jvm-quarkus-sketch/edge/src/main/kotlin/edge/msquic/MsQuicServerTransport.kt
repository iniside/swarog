package edge.msquic

import edge.EdgeConnection
import edge.EdgeTransport
import java.lang.foreign.Arena
import java.lang.foreign.MemorySegment
import java.lang.foreign.ValueLayout.ADDRESS
import java.lang.foreign.ValueLayout.JAVA_BYTE
import java.util.HexFormat

/**
 * A real-QUIC [EdgeTransport] server. [serve] stands up an msquic listener; each accepted connection's
 * single peer-opened bidirectional stream becomes one [MsQuicConnection] handed to `onConnection`, so
 * an unchanged [edge.EdgeServer] runs over QUIC.
 *
 * TLS is schannel: the server presents a machine/user certificate identified by its SHA-1 thumbprint
 * ([certThumbprintHex], 40 hex chars → 20 bytes) via a `CERTIFICATE_HASH` credential. Provisioning the
 * cert is Krok 4; this class only consumes the thumbprint.
 *
 * Arena strategy: upcall stubs on `Arena.global` (via [Upcalls]); the registration/configuration/
 * listener handles and the ALPN/settings/credential native memory live on the transport-scoped [arena]
 * for the whole server lifetime; each connection gets its own arena (released after SHUTDOWN_COMPLETE).
 */
class MsQuicServerTransport(
    private val port: Int,
    certThumbprintHex: String,
    private val alpn: String = "edge",
) : EdgeTransport, AutoCloseable {

    private val thumbprint: ByteArray = HexFormat.of().parseHex(certThumbprintHex.trim()).also {
        require(it.size == CERT_HASH_LEN) { "cert thumbprint must be $CERT_HASH_LEN bytes, got ${it.size}" }
    }

    private val lib = MsQuicLibrary()
    val api: MsQuicApi get() = lib.api

    private val arena = Arena.ofShared()

    @Volatile private var connectionHandler: ((EdgeConnection) -> Unit)? = null
    @Volatile private var registration: MemorySegment = MemorySegment.NULL
    @Volatile private var configuration: MemorySegment = MemorySegment.NULL
    @Volatile private var listener: MemorySegment = MemorySegment.NULL

    // Three singleton upcall stubs (Arena.global). Each resolves its owner from CallbackRegistry by
    // the ctx-id smuggled through void* — so one stub serves every connection/stream.
    private val listenerStub: MemorySegment = Upcalls.stub(::onListenerEvent)
    private val connStub: MemorySegment = Upcalls.stub { handle, ctx, event -> dispatchConnEvent(handle, ctx, event) }
    val streamStub: MemorySegment = Upcalls.stub { handle, ctx, event -> dispatchStreamEvent(handle, ctx, event) }

    override fun serve(onConnection: (EdgeConnection) -> Unit) {
        this.connectionHandler = onConnection

        // RegistrationOpen(NULL → default LOW_LATENCY profile).
        val regOut = arena.allocate(ADDRESS)
        check(Constants.succeeded(api.registrationOpen(MemorySegment.NULL, regOut))) {
            "RegistrationOpen failed"
        }
        registration = regOut.get(ADDRESS, 0)

        // ConfigurationOpen(ALPN "edge" + 144B SETTINGS).
        val alpnBuf = MsQuicSupport.buildAlpn(arena, alpn)
        val settings = MsQuicSupport.buildSettings(arena)
        val cfgOut = arena.allocate(ADDRESS)
        val cfgStatus = api.configurationOpen(
            registration, alpnBuf, 1, settings, Layouts.SETTINGS_SIZE, MemorySegment.NULL, cfgOut,
        )
        check(Constants.succeeded(cfgStatus)) { "ConfigurationOpen failed: 0x%08x".format(cfgStatus) }
        configuration = cfgOut.get(ADDRESS, 0)

        // LoadCredential: server CERTIFICATE_HASH from the 20-byte thumbprint.
        val certHash = arena.allocate(Layouts.QUIC_CERTIFICATE_HASH)
        MemorySegment.copy(thumbprint, 0, certHash, JAVA_BYTE, Layouts.CERT_HASH_SHAHASH_OFF, CERT_HASH_LEN)
        val cred = arena.allocate(Layouts.QUIC_CREDENTIAL_CONFIG) // zeroed
        cred.set(java.lang.foreign.ValueLayout.JAVA_INT, Layouts.CRED_TYPE_OFF, Constants.QUIC_CREDENTIAL_TYPE_CERTIFICATE_HASH)
        cred.set(java.lang.foreign.ValueLayout.JAVA_INT, Layouts.CRED_FLAGS_OFF, Constants.QUIC_CREDENTIAL_FLAG_NONE)
        cred.set(ADDRESS, Layouts.CRED_CERTUNION_OFF, certHash)
        val credStatus = api.configurationLoadCredential(configuration, cred)
        check(Constants.succeeded(credStatus)) { "LoadCredential(cert hash) failed: 0x%08x".format(credStatus) }

        // ListenerOpen + ListenerStart on 127.0.0.1:port.
        val listenerCtxId = CallbackRegistry.register(this)
        val listenerOut = arena.allocate(ADDRESS)
        val lsOpen = api.listenerOpen(registration, listenerStub, CallbackRegistry.contextFor(listenerCtxId), listenerOut)
        check(Constants.succeeded(lsOpen)) { "ListenerOpen failed: 0x%08x".format(lsOpen) }
        listener = listenerOut.get(ADDRESS, 0)
        val localAddr = Layouts.quicAddrIpv4(arena, port)
        val lsStart = api.listenerStart(listener, alpnBuf, 1, localAddr)
        check(Constants.succeeded(lsStart)) { "ListenerStart failed: 0x%08x".format(lsStart) }
    }

    internal fun fireConnection(conn: EdgeConnection) {
        connectionHandler?.invoke(conn)
    }

    // ---- Listener callback ------------------------------------------------------------------------

    private fun onListenerEvent(handle: MemorySegment, ctx: MemorySegment, event: MemorySegment): Int {
        val ev = MsQuicSupport.reinterpretEvent(event)
        if (Layouts.eventType(ev) != Constants.QUIC_LISTENER_EVENT_NEW_CONNECTION) {
            return Constants.QUIC_STATUS_SUCCESS
        }
        val conn = ev.get(ADDRESS, Layouts.UNION_OFFSET + Layouts.LISTENER_NEWCONN_CONNECTION_OFF)

        // Register a per-connection context FIRST so no connection event can arrive un-routed, then wire
        // the connection callback + configuration and accept by returning SUCCESS.
        val serverConn = ServerConnectionContext(this)
        val connCtxId = CallbackRegistry.register(serverConn)
        serverConn.connCtxId = connCtxId
        api.setCallbackHandler(conn, connStub, CallbackRegistry.contextFor(connCtxId))
        val setCfg = api.connectionSetConfiguration(conn, configuration)
        check(Constants.succeeded(setCfg)) { "ConnectionSetConfiguration failed: 0x%08x".format(setCfg) }
        return Constants.QUIC_STATUS_SUCCESS
    }

    private fun dispatchConnEvent(handle: MemorySegment, ctx: MemorySegment, event: MemorySegment): Int {
        val serverConn = CallbackRegistry.resolve(ctx) as? ServerConnectionContext
            ?: return Constants.QUIC_STATUS_SUCCESS
        return serverConn.handle(handle, MsQuicSupport.reinterpretEvent(event))
    }

    private fun dispatchStreamEvent(handle: MemorySegment, ctx: MemorySegment, event: MemorySegment): Int {
        val conn = CallbackRegistry.resolve(ctx) as? MsQuicConnection
            ?: return Constants.QUIC_STATUS_SUCCESS
        return conn.handleStreamEvent(handle, MsQuicSupport.reinterpretEvent(event))
    }

    override fun close() {
        // Shut down all connections owned by the registration FIRST, then close it: RegistrationClose
        // blocks until every connection has drained, so without an active shutdown a still-connected
        // peer would hang teardown until the idle timeout (~90s). RegistrationShutdown operates on the
        // registration handle (never the per-connection handles the callbacks may be concurrently
        // freeing), so it is safe here regardless of how far along any connection's shutdown is —
        // avoiding the use-after-free of touching individual connection handles from close().
        runCatching {
            if (registration.address() != 0L) {
                api.registrationShutdown(registration, Constants.QUIC_CONNECTION_SHUTDOWN_FLAG_NONE, 0L)
            }
        }
        runCatching { if (listener.address() != 0L) api.listenerClose(listener) }
        runCatching { if (configuration.address() != 0L) api.configurationClose(configuration) }
        runCatching { if (registration.address() != 0L) api.registrationClose(registration) }
        runCatching { arena.close() }
        runCatching { lib.close() }
    }

    private companion object {
        const val CERT_HASH_LEN = 20
    }
}

/**
 * Per-connection server state, resolved from [CallbackRegistry] by the connection callback. Owns the
 * per-connection arena and the [MsQuicConnection] created when the peer opens its stream.
 */
private class ServerConnectionContext(private val transport: MsQuicServerTransport) {

    var connCtxId: Long = 0L
    private var streamCtxId: Long = 0L
    private var connArena: Arena? = null
    private var edgeConn: MsQuicConnection? = null

    fun handle(handle: MemorySegment, event: MemorySegment): Int {
        when (Layouts.eventType(event)) {
            Constants.QUIC_CONNECTION_EVENT_PEER_STREAM_STARTED -> onPeerStreamStarted(handle, event)
            Constants.QUIC_CONNECTION_EVENT_SHUTDOWN_COMPLETE -> onShutdownComplete(handle)
            else -> Unit
        }
        return Constants.QUIC_STATUS_SUCCESS
    }

    private fun onPeerStreamStarted(connection: MemorySegment, event: MemorySegment) {
        val stream = event.get(ADDRESS, Layouts.UNION_OFFSET + Layouts.CONN_PEER_STREAM_STREAM_OFF)
        val arena = Arena.ofShared()
        val conn = MsQuicConnection(stream, connection, transport.api, arena)
        connArena = arena
        edgeConn = conn
        streamCtxId = CallbackRegistry.register(conn)
        transport.api.setCallbackHandler(stream, transport.streamStub, CallbackRegistry.contextFor(streamCtxId))
        // Hand the connection to EdgeServer; it spawns its own daemon reader thread and returns fast,
        // so the msquic worker thread is not blocked.
        transport.fireConnection(conn)
    }

    private fun onShutdownComplete(connection: MemorySegment) {
        edgeConn?.signalClose()
        runCatching { transport.api.connectionClose(connection) }
        edgeConn?.disposeNativeMemory()
        CallbackRegistry.unregister(connCtxId)
        if (streamCtxId != 0L) CallbackRegistry.unregister(streamCtxId)
    }
}
