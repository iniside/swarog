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
                    when (val msg = codec.decode(frame)) {
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
}
