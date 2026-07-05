package edge.msquic

import edge.EdgeConnection
import java.lang.foreign.Arena
import java.lang.foreign.MemorySegment
import java.lang.foreign.ValueLayout.ADDRESS
import java.lang.foreign.ValueLayout.JAVA_INT
import java.util.concurrent.CompletableFuture
import java.util.concurrent.TimeUnit

/**
 * A real-QUIC client. [connect] dials an [MsQuicServerTransport], performs the QUIC handshake, opens
 * the single persistent bidirectional stream, and returns an [MsQuicConnection] the unchanged
 * [edge.EdgeClient] drives.
 *
 * TLS: a client credential of Type=NONE with `CLIENT | NO_CERTIFICATE_VALIDATION` (0x5) — localhost/
 * self-signed, no chain validation.
 *
 * The registration + configuration are opened once (lazily on first [connect]) on the transport-scoped
 * [arena]; each [connect] gets its own connection arena (released after that connection's
 * SHUTDOWN_COMPLETE). Upcall stubs are singletons on `Arena.global` (via [Upcalls]).
 */
class MsQuicClientTransport(private val alpn: String = "edge") : AutoCloseable {

    private val lib = MsQuicLibrary()
    val api: MsQuicApi get() = lib.api

    private val arena = Arena.ofShared()

    @Volatile private var registration: MemorySegment = MemorySegment.NULL
    @Volatile private var configuration: MemorySegment = MemorySegment.NULL
    private val initLock = Any()

    private val connStub: MemorySegment = Upcalls.stub { handle, ctx, event -> dispatchConnEvent(handle, ctx, event) }
    val streamStub: MemorySegment = Upcalls.stub { handle, ctx, event -> dispatchStreamEvent(handle, ctx, event) }

    /**
     * Dials `host:port` and blocks (up to [CONNECT_TIMEOUT_SECONDS]) until the QUIC connection is up
     * and its bidi stream open. Throws if the connection is shut down before it connects, or on timeout.
     */
    fun connect(host: String, port: Int): EdgeConnection {
        ensureConfigured()

        val connArena = Arena.ofShared()
        val future = CompletableFuture<MsQuicConnection>()
        val clientConn = ClientConnectionContext(this, connArena, future)
        val connCtxId = CallbackRegistry.register(clientConn)
        clientConn.connCtxId = connCtxId

        // connOut only needs to survive ConnectionOpen; put it on the per-connection arena so it is
        // reclaimed with that connection (not accumulated on the transport arena across reconnects).
        val connOut = connArena.allocate(ADDRESS)
        val open = api.connectionOpen(registration, connStub, CallbackRegistry.contextFor(connCtxId), connOut)
        check(Constants.succeeded(open)) { "ConnectionOpen failed: 0x%08x".format(open) }
        clientConn.connection = connOut.get(ADDRESS, 0)

        // serverName lives on the per-connection arena (referenced through the handshake).
        val serverName = connArena.allocateFrom(host)
        val start = api.connectionStart(
            clientConn.connection, configuration, Constants.QUIC_ADDRESS_FAMILY_UNSPEC, serverName, port.toShort(),
        )
        check(Constants.succeeded(start)) { "ConnectionStart failed: 0x%08x".format(start) }

        return try {
            future.get(CONNECT_TIMEOUT_SECONDS, TimeUnit.SECONDS)
        } catch (e: java.util.concurrent.ExecutionException) {
            throw IllegalStateException("QUIC connect to $host:$port failed", e.cause ?: e)
        } catch (e: java.util.concurrent.TimeoutException) {
            throw IllegalStateException("QUIC connect to $host:$port timed out", e)
        }
    }

    private fun ensureConfigured() {
        if (registration.address() != 0L) return
        synchronized(initLock) {
            if (registration.address() != 0L) return
            val regOut = arena.allocate(ADDRESS)
            check(Constants.succeeded(api.registrationOpen(MemorySegment.NULL, regOut))) { "RegistrationOpen failed" }
            val reg = regOut.get(ADDRESS, 0)

            val alpnBuf = MsQuicSupport.buildAlpn(arena, alpn)
            val settings = MsQuicSupport.buildSettings(arena)
            val cfgOut = arena.allocate(ADDRESS)
            val cfgStatus = api.configurationOpen(
                reg, alpnBuf, 1, settings, Layouts.SETTINGS_SIZE, MemorySegment.NULL, cfgOut,
            )
            check(Constants.succeeded(cfgStatus)) { "ConfigurationOpen failed: 0x%08x".format(cfgStatus) }
            val cfg = cfgOut.get(ADDRESS, 0)

            // Client credential: Type=NONE, Flags = CLIENT | NO_CERTIFICATE_VALIDATION (0x5).
            val cred = arena.allocate(Layouts.QUIC_CREDENTIAL_CONFIG) // zeroed
            cred.set(JAVA_INT, Layouts.CRED_TYPE_OFF, Constants.QUIC_CREDENTIAL_TYPE_NONE)
            cred.set(JAVA_INT, Layouts.CRED_FLAGS_OFF, Constants.QUIC_CREDENTIAL_FLAGS_CLIENT_NO_VALIDATION)
            val credStatus = api.configurationLoadCredential(cfg, cred)
            check(Constants.succeeded(credStatus)) { "LoadCredential(client) failed: 0x%08x".format(credStatus) }

            configuration = cfg
            registration = reg // published last: gates the double-checked init above
        }
    }

    private fun dispatchConnEvent(handle: MemorySegment, ctx: MemorySegment, event: MemorySegment): Int {
        val clientConn = CallbackRegistry.resolve(ctx) as? ClientConnectionContext
            ?: return Constants.QUIC_STATUS_SUCCESS
        return clientConn.handle(handle, MsQuicSupport.reinterpretEvent(event))
    }

    private fun dispatchStreamEvent(handle: MemorySegment, ctx: MemorySegment, event: MemorySegment): Int {
        val conn = CallbackRegistry.resolve(ctx) as? MsQuicConnection
            ?: return Constants.QUIC_STATUS_SUCCESS
        return conn.handleStreamEvent(handle, MsQuicSupport.reinterpretEvent(event))
    }

    override fun close() {
        runCatching { if (configuration.address() != 0L) api.configurationClose(configuration) }
        runCatching { if (registration.address() != 0L) api.registrationClose(registration) }
        runCatching { arena.close() }
        runCatching { lib.close() }
    }

    companion object {
        const val CONNECT_TIMEOUT_SECONDS = 10L
    }
}

/**
 * Per-connection client state resolved from [CallbackRegistry] by the connection callback. On CONNECTED
 * it opens the bidi stream and completes [future] with the [MsQuicConnection]; a shutdown before
 * CONNECTED completes [future] exceptionally so [MsQuicClientTransport.connect] throws instead of
 * blocking forever.
 */
private class ClientConnectionContext(
    private val transport: MsQuicClientTransport,
    private val arena: Arena,
    private val future: CompletableFuture<MsQuicConnection>,
) {
    var connCtxId: Long = 0L
    var connection: MemorySegment = MemorySegment.NULL
    private var streamCtxId: Long = 0L
    private var edgeConn: MsQuicConnection? = null

    fun handle(handle: MemorySegment, event: MemorySegment): Int {
        when (Layouts.eventType(event)) {
            Constants.QUIC_CONNECTION_EVENT_CONNECTED -> onConnected(handle)
            Constants.QUIC_CONNECTION_EVENT_SHUTDOWN_INITIATED_BY_TRANSPORT -> onShutdownByTransport(event)
            Constants.QUIC_CONNECTION_EVENT_SHUTDOWN_INITIATED_BY_PEER ->
                future.completeExceptionally(IllegalStateException("connection shut down by peer before CONNECTED"))
            Constants.QUIC_CONNECTION_EVENT_SHUTDOWN_COMPLETE -> onShutdownComplete(handle)
            else -> Unit
        }
        return Constants.QUIC_STATUS_SUCCESS
    }

    private fun onConnected(connectionHandle: MemorySegment) {
        // Open + start the single bidi stream. StreamOpen needs a callback+context up front; we set a
        // placeholder then immediately SetCallbackHandler with the real ctx-id (no stream event can fire
        // before we return from this connection callback).
        val streamOut = arena.allocate(ADDRESS)
        val open = transport.api.streamOpen(
            connectionHandle, Constants.QUIC_STREAM_OPEN_FLAG_NONE, transport.streamStub, MemorySegment.NULL, streamOut,
        )
        check(Constants.succeeded(open)) { "StreamOpen failed: 0x%08x".format(open) }
        val stream = streamOut.get(ADDRESS, 0)

        val conn = MsQuicConnection(stream, connectionHandle, transport.api, arena)
        edgeConn = conn
        streamCtxId = CallbackRegistry.register(conn)
        transport.api.setCallbackHandler(stream, transport.streamStub, CallbackRegistry.contextFor(streamCtxId))

        val start = transport.api.streamStart(stream, Constants.QUIC_STREAM_START_FLAG_NONE)
        check(Constants.succeeded(start)) { "StreamStart failed: 0x%08x".format(start) }

        future.complete(conn)
    }

    private fun onShutdownByTransport(event: MemorySegment) {
        val status = event.get(JAVA_INT, Layouts.UNION_OFFSET + Layouts.CONN_SHUTDOWN_STATUS_OFF)
        future.completeExceptionally(
            IllegalStateException("connection shut down by transport before CONNECTED: 0x%08x".format(status)),
        )
    }

    private fun onShutdownComplete(connectionHandle: MemorySegment) {
        // If we never connected, unblock connect() with a failure (no-op if already completed).
        future.completeExceptionally(IllegalStateException("connection shut down before CONNECTED"))
        edgeConn?.signalClose()
        runCatching { transport.api.connectionClose(connectionHandle) }
        edgeConn?.disposeNativeMemory()
        CallbackRegistry.unregister(connCtxId)
        if (streamCtxId != 0L) CallbackRegistry.unregister(streamCtxId)
    }
}
