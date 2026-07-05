package edge

/**
 * ONE client connection's bidirectional frame stream, abstracted to the minimum: blocking
 * [receive] (null on close) and fire-and-forget [send]. Deliberately NOT reactive — a future
 * `MsQuicConnection` implements exactly these three methods over a QUIC bidirectional stream and
 * the rest of the core is unchanged.
 */
interface EdgeConnection {
    /** Blocks for the next inbound frame; returns null when the peer closed the stream. */
    fun receive(): ByteArray?

    /** Sends one frame to the peer. */
    fun send(frame: ByteArray)

    /** Closes this side of the stream (unblocks the peer's [receive] with null). */
    fun close()
}

/**
 * A server that accepts client connections. [serve] registers the callback the transport invokes
 * once per new [EdgeConnection]. The loopback transport calls it in-JVM; a QUIC transport would call
 * it from its accept loop — [EdgeServer] doesn't care which.
 */
interface EdgeTransport {
    fun serve(onConnection: (EdgeConnection) -> Unit)
}
