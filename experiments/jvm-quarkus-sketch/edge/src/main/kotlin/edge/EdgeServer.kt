package edge

/**
 * The server view of one connection, handed to [EdgeServer.onConnect]. Wraps the raw
 * [EdgeConnection] with the codec so a handler or an external caller can [push] a typed object down
 * the stream at any time — the server-initiated half of the protocol.
 */
class EdgeServerConnection(
    private val connection: EdgeConnection,
    private val codec: EdgeCodec,
) {
    /** Sends an unsolicited [Push] (cid 0) carrying a msgpack-encoded typed object. */
    fun <T> push(topic: String, obj: T) {
        connection.send(codec.encode(Push(topic = topic, payload = codec.encodePayload(obj))))
    }
}

/**
 * The transport-agnostic server: for each connection the [transport] accepts, run a per-connection
 * loop — receive a frame, decode it, and if it's a [Request], dispatch it through the [router] and
 * send the [Response] back. Any decoded [Response]/[Push] arriving from the peer is ignored (the
 * client is the requester; the server only answers and pushes).
 *
 * [onConnect] fires once per new connection with an [EdgeServerConnection], so a handler owner or a
 * test can hold onto it and server-push later. Each connection is serviced on its own daemon thread
 * (blocking receive; no reactive framework) — a QUIC transport swaps in without touching this class.
 */
class EdgeServer(
    private val router: EdgeRouter,
    private val transport: EdgeTransport,
    private val codec: EdgeCodec = EdgeCodec(),
    private val onConnect: (EdgeServerConnection) -> Unit = {},
) {
    fun start() {
        transport.serve { connection ->
            onConnect(EdgeServerConnection(connection, codec))
            Thread {
                while (true) {
                    val frame = connection.receive() ?: break
                    val msg = decodeOrDrop(frame) ?: continue
                    when (msg) {
                        is Request -> connection.send(codec.encode(router.dispatch(msg)))
                        is Response, is Push -> Unit // server does not act on peer responses/pushes
                    }
                }
            }.apply {
                name = "edge-server-conn"
                isDaemon = true
                start()
            }
        }
    }

    @Suppress("TooGenericExceptionCaught") // deliberate frame-boundary guard: codec.decode over a
    // malformed/corrupt frame from a client throws various Jackson/IO/IAE types. Previously that
    // escaped UNCAUGHT into the daemon reader thread, silently killing this connection's whole
    // service loop with zero diagnostics. Frames are independent (the transport hands whole messages),
    // so we LOG the bad frame and CONTINUE serving the connection rather than tear it down over one
    // corrupt message — a single garbage frame must not become a per-connection DoS. Logging (not a
    // silent swallow) keeps the failure observable.
    private fun decodeOrDrop(frame: ByteArray): EdgeMessage? =
        try {
            codec.decode(frame)
        } catch (e: Exception) {
            System.err.println("[edge-server-conn] dropping undecodable frame (${frame.size} bytes): $e")
            null
        }
}
