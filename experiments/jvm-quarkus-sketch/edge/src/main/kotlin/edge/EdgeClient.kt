package edge

import java.util.concurrent.CompletableFuture
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.LinkedBlockingQueue
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicLong

/**
 * The client half of the protocol over any [EdgeConnection]. A single reader thread demultiplexes
 * inbound frames: a [Response] completes the pending call keyed by its [Response.cid]; a [Push]
 * lands on a queue for [nextPush]. [request] correlates a Response to its Request by cid and blocks
 * for it. Transport-agnostic — swap the loopback connection for a QUIC one and this is unchanged.
 */
class EdgeClient(
    private val connection: EdgeConnection,
    private val codec: EdgeCodec = EdgeCodec(),
) {
    private val cidGen = AtomicLong(1)
    private val pending = ConcurrentHashMap<Long, CompletableFuture<Response>>()
    private val pushes = LinkedBlockingQueue<Push>()

    private val reader = Thread {
        while (true) {
            val frame = connection.receive() ?: break
            when (val msg = codec.decode(frame)) {
                is Response -> pending.remove(msg.cid)?.complete(msg)
                is Push -> pushes.put(msg)
                is Request -> Unit // client does not serve requests in this core
            }
        }
    }.apply {
        name = "edge-client-reader"
        isDaemon = true
    }

    fun start() = reader.start()

    /** Sends a Request with a fresh cid and blocks for the matching Response. */
    fun request(method: String, payloadObj: Any, timeoutMs: Long = 2_000): Response =
        requestWithCid(cidGen.getAndIncrement(), method, payloadObj, timeoutMs)

    /** As [request] but with a caller-chosen cid — lets a test assert Response.cid == the sent cid. */
    fun requestWithCid(cid: Long, method: String, payloadObj: Any, timeoutMs: Long = 2_000): Response {
        val future = CompletableFuture<Response>()
        pending[cid] = future
        connection.send(codec.encode(Request(cid, method, codec.encodePayload(payloadObj))))
        return future.get(timeoutMs, TimeUnit.MILLISECONDS)
    }

    /** Typed convenience: [request] then msgpack-decode the reply. Throws on `ok=false`. */
    fun <Resp> call(method: String, payloadObj: Any, respType: Class<Resp>): Resp {
        val resp = request(method, payloadObj)
        require(resp.ok) { "edge call '$method' failed: ${resp.error}" }
        return codec.decodePayload(resp.payload, respType)
    }

    /** Decode a received Push (or any payload) into its typed object. */
    fun <T> decode(bytes: ByteArray, type: Class<T>): T = codec.decodePayload(bytes, type)

    /** Blocks up to [timeoutMs] for the next server Push; null if none arrives. */
    fun nextPush(timeoutMs: Long = 2_000): Push? = pushes.poll(timeoutMs, TimeUnit.MILLISECONDS)
}
