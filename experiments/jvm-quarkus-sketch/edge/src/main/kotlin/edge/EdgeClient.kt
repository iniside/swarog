package edge

import java.util.concurrent.CompletableFuture
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.ExecutionException
import java.util.concurrent.LinkedBlockingQueue
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicLong

/**
 * Thrown by an in-flight [EdgeClient.request] when the underlying connection dies (the reader saw
 * `receive()==null`) or the client is [EdgeClient.close]d while the call is pending. Deliberately
 * DISTINCT from a `TimeoutException` (which still means "connection alive, but the peer never
 * answered in time") so callers can tell "the link is gone, retry/reconnect" from "the peer is slow".
 */
class ConnectionClosedException(message: String, cause: Throwable? = null) : RuntimeException(message, cause)

/**
 * The client half of the protocol over any [EdgeConnection]. A single reader thread demultiplexes
 * inbound frames: a [Response] completes the pending call keyed by its [Response.cid]; a [Push]
 * lands on a queue for [nextPush]. [request] correlates a Response to its Request by cid and blocks
 * for it. Transport-agnostic — swap the loopback connection for a QUIC one and this is unchanged.
 *
 * **Connection-death handling (race-safe).** When the reader exits (peer closed, or [close] was
 * called) every still-pending call is failed with a [ConnectionClosedException] instead of hanging
 * until its own timeout. The tricky part is the *add-after-drain* race: a [requestWithCid] that
 * inserts into [pending] concurrently with the drain must never be left live-but-unfailed. This is
 * closed with a single [lifecycleLock]: BOTH the terminal transition (set [closedCause] + snapshot &
 * clear [pending]) AND every insert (check [closedCause] + put) run inside that lock, so the lock's
 * total order over the two critical sections leaves no interleaving in between. See [onClosed] /
 * [requestRawWithCid] (the primitive both [requestWithCid] and [requestRaw] funnel through) for the
 * happens-before argument.
 */
@Suppress("TooManyFunctions") // the client's surface is the protocol's surface: two request paths
// (typed [request]/[requestWithCid] + raw byte-relay [requestRaw]/[requestRawWithCid]), plus [call],
// [decode], [nextPush], [start], [close]. Each is a distinct, minimal protocol verb — collapsing any
// pair would hide the typed-vs-raw distinction that is the whole point of the byte-relay addition.
class EdgeClient(
    private val connection: EdgeConnection,
    private val codec: EdgeCodec = EdgeCodec(),
) {
    private val cidGen = AtomicLong(1)
    private val pending = ConcurrentHashMap<Long, CompletableFuture<Response>>()
    private val pushes = LinkedBlockingQueue<Push>()

    /**
     * Guards the terminal transition against inserts. The invariant: an insert and the close-drain
     * are each wrapped in `synchronized(lifecycleLock)`, so they are TOTALLY ORDERED. For any future
     * F and the close C: either F's insert block runs before C's (⇒ F is in [pending] when C
     * snapshots, so C fails it), or C runs before F's insert block (⇒ the insert observes
     * [closedCause] != null and throws without ever adding F). No third interleaving exists — the
     * add-after-drain hole is gone.
     */
    private val lifecycleLock = Any()

    /** Set exactly once, under [lifecycleLock], when the reader exits or [close] is called. */
    @Volatile
    private var closedCause: ConnectionClosedException? = null

    private val reader = Thread {
        while (true) {
            val frame = connection.receive() ?: break
            val msg = decodeOrDrop(frame) ?: continue
            when (msg) {
                is Response -> pending.remove(msg.cid)?.complete(msg)
                is Push -> pushes.put(msg)
                is Request -> Unit // client does not serve requests in this core
            }
        }
        // receive()==null ⇒ the peer closed the stream. Fail every in-flight call (idempotent if
        // close() already ran the same transition).
        onClosed(ConnectionClosedException("edge connection closed by peer"))
    }.apply {
        name = "edge-client-reader"
        isDaemon = true
    }

    @Suppress("TooGenericExceptionCaught") // deliberate frame-boundary guard (mirrors EdgeServer):
    // codec.decode over an arbitrary/corrupt inbound frame can throw various Jackson/IO/IAE types.
    // A single undecodable frame must NOT kill the reader (which would silently strand every pending
    // call); the transport hands whole, independently-framed messages, so dropping one and continuing
    // is isolated. Log so the failure is observable (SwallowedException would hide it).
    private fun decodeOrDrop(frame: ByteArray): EdgeMessage? =
        try {
            codec.decode(frame)
        } catch (e: Exception) {
            System.err.println("[edge-client] dropping undecodable inbound frame (${frame.size} bytes): $e")
            null
        }

    fun start() = reader.start()

    /**
     * Terminal transition: mark the client closed and fail every pending call. Runs the state-set +
     * map snapshot/clear under [lifecycleLock] so it is serialized against [requestWithCid]'s insert;
     * the futures are completed OUTSIDE the lock (once removed from [pending] no inserter can re-add
     * them, and `completeExceptionally` is idempotent), avoiding running CompletableFuture callbacks
     * under the lock. Idempotent: only the first caller (reader exit vs [close]) performs the drain.
     */
    private fun onClosed(cause: ConnectionClosedException) {
        val toFail: List<CompletableFuture<Response>>
        synchronized(lifecycleLock) {
            if (closedCause != null) return
            closedCause = cause
            toFail = ArrayList(pending.values)
            pending.clear()
        }
        toFail.forEach { it.completeExceptionally(cause) }
    }

    /**
     * Closes the client: fails all pending calls with a [ConnectionClosedException], then closes the
     * underlying connection (which also unblocks the reader's `receive()`; its own `onClosed` is a
     * no-op then, being idempotent).
     */
    fun close() {
        onClosed(ConnectionClosedException("edge client closed"))
        connection.close()
    }

    /** Sends a Request with a fresh cid and blocks for the matching Response. */
    fun request(method: String, payloadObj: Any, timeoutMs: Long = 2_000): Response =
        requestWithCid(cidGen.getAndIncrement(), method, payloadObj, timeoutMs)

    /** As [request] but with a caller-chosen cid — lets a test assert Response.cid == the sent cid. */
    fun requestWithCid(cid: Long, method: String, payloadObj: Any, timeoutMs: Long = 2_000): Response =
        requestRawWithCid(cid, method, codec.encodePayload(payloadObj), timeoutMs)

    /**
     * The byte-relay path: sends a Request whose wire payload is [payloadBytes] VERBATIM, WITHOUT
     * running them through [EdgeCodec.encodePayload]. Everything else (fresh cid, pending correlation,
     * timeout, [ConnectionClosedException] on a dead link) is identical to [request].
     *
     * Why a distinct method: [request]/[requestWithCid] call `codec.encodePayload(payloadObj)`, which
     * msgpack-encodes the object. Handing them an already-encoded `ByteArray` would emit a msgpack
     * **bin** (a length-prefixed byte blob), NOT the raw bytes — so a gateway relaying an inbound
     * payload blob through [request] would DOUBLE-encode it and the downstream `typedHandler` decode
     * would fail. [requestRaw] puts the exact bytes on the wire so the downstream typed decoder sees
     * the same blob the original caller produced.
     */
    fun requestRaw(method: String, payloadBytes: ByteArray, timeoutMs: Long = 2_000): Response =
        requestRawWithCid(cidGen.getAndIncrement(), method, payloadBytes, timeoutMs)

    /** As [requestRaw] but with a caller-chosen cid. This is the primitive both request paths funnel through. */
    fun requestRawWithCid(cid: Long, method: String, payloadBytes: ByteArray, timeoutMs: Long = 2_000): Response {
        val future = CompletableFuture<Response>()
        // Insert under the lock and RE-CHECK the terminal state in the same critical section. If a
        // close already committed, we never add F and fail fast here; otherwise F is in [pending]
        // before any future close can snapshot it, so close() is guaranteed to fail F. (See
        // [lifecycleLock] doc for the two-case ordering argument that closes the add-after-drain race.)
        synchronized(lifecycleLock) {
            closedCause?.let { throw ConnectionClosedException("edge connection closed", it) }
            pending[cid] = future
        }
        connection.send(codec.encode(Request(cid, method, payloadBytes)))
        return try {
            future.get(timeoutMs, TimeUnit.MILLISECONDS)
        } catch (e: ExecutionException) {
            // Unwrap so callers see the ConnectionClosedException directly, not an ExecutionException.
            throw e.cause ?: e
        }
    }

    /** Typed convenience: [request] then msgpack-decode the reply. Throws on `ok=false`. */
    fun <Resp> call(method: String, payloadObj: Any, respType: Class<Resp>): Resp {
        val resp = request(method, payloadObj)
        require(resp.ok) { "edge call '$method' failed: ${resp.error ?: "<no error message>"}" }
        return codec.decodePayload(resp.payload, respType)
    }

    /** Decode a received Push (or any payload) into its typed object. */
    fun <T> decode(bytes: ByteArray, type: Class<T>): T = codec.decodePayload(bytes, type)

    /** Blocks up to [timeoutMs] for the next server Push; null if none arrives. */
    fun nextPush(timeoutMs: Long = 2_000): Push? = pushes.poll(timeoutMs, TimeUnit.MILLISECONDS)
}
