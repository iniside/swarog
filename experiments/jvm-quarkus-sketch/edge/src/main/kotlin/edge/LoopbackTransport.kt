package edge

import java.util.concurrent.BlockingQueue
import java.util.concurrent.LinkedBlockingQueue

/**
 * An in-JVM [EdgeTransport] that wires a client and a server side with two queues (client→server and
 * server→client). Proves the whole RPC path end-to-end with zero network — the stand-in until
 * `MsQuicTransport` arrives. [connect] opens one connection: it hands the SERVER side to the served
 * callback and returns the CLIENT side to the caller.
 */
class LoopbackTransport : EdgeTransport {
    @Volatile
    private var onConnection: ((EdgeConnection) -> Unit)? = null

    override fun serve(onConnection: (EdgeConnection) -> Unit) {
        this.onConnection = onConnection
    }

    /** Opens a client connection; triggers the server's onConnection callback. */
    fun connect(): EdgeConnection {
        val handler = onConnection ?: error("LoopbackTransport is not serving yet")
        val clientToServer = LinkedBlockingQueue<ByteArray>()
        val serverToClient = LinkedBlockingQueue<ByteArray>()
        handler(LoopbackConnection(incoming = clientToServer, outgoing = serverToClient))
        return LoopbackConnection(incoming = serverToClient, outgoing = clientToServer)
    }
}

/**
 * One end of a loopback connection: reads its [incoming] queue, writes its [outgoing] queue. Close
 * enqueues a private sentinel; [receive] returns null on that exact instance (identity check, so a
 * genuine empty-payload frame is never mistaken for close).
 */
class LoopbackConnection(
    private val incoming: BlockingQueue<ByteArray>,
    private val outgoing: BlockingQueue<ByteArray>,
) : EdgeConnection {

    override fun receive(): ByteArray? {
        val frame = incoming.take()
        return if (frame === CLOSED) null else frame
    }

    override fun send(frame: ByteArray) {
        outgoing.put(frame)
    }

    override fun close() {
        outgoing.put(CLOSED)
    }

    private companion object {
        val CLOSED = ByteArray(0)
    }
}
