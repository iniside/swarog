package edge.msquic

import edge.EdgeConnection
import java.lang.foreign.Arena
import java.lang.foreign.MemorySegment
import java.lang.foreign.ValueLayout.ADDRESS
import java.lang.foreign.ValueLayout.JAVA_BYTE
import java.lang.foreign.ValueLayout.JAVA_INT
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.LinkedBlockingQueue
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicLong

/**
 * One [EdgeConnection] over ONE persistent QUIC bidirectional stream — the single type both the server
 * ([MsQuicServerTransport], stream arrives via PEER_STREAM_STARTED) and the client
 * ([MsQuicClientTransport], stream opened on CONNECTED) hand back to the unchanged edge RPC core.
 *
 * Framing is a 4-byte big-endian length + payload. [send] issues exactly ONE `StreamSend` carrying a
 * single contiguous `[len(4B BE) || frame]` region, so concurrent senders never interleave a frame.
 * [receive] returns exactly one reassembled payload (or null once a CLOSE sentinel is enqueued by the
 * stream/connection shutdown callbacks).
 *
 * Native-memory lifetimes (the segfault surface):
 *  - each `StreamSend`'s buffer+bytes live in a per-send [Arena] registered in [sends] under the sendId
 *    smuggled through the send's `void* ClientContext`; freed ONLY in the SEND_COMPLETE callback (the
 *    bytes must stay pinned until msquic is done with them).
 *  - [arena] is the per-connection shared arena for connection-lifetime memory; it and any straggler
 *    send arenas are released by [disposeNativeMemory], called ONLY after the connection's
 *    SHUTDOWN_COMPLETE, when no further callback can touch this stream.
 *
 * Stream events are delivered here by the transport's stream upcall (which resolves this object from
 * [CallbackRegistry] by ctx-id) via [handleStreamEvent].
 */
class MsQuicConnection(
    private val stream: MemorySegment,
    private val connection: MemorySegment,
    private val api: MsQuicApi,
    private val arena: Arena,
) : EdgeConnection {

    private val queue = LinkedBlockingQueue<ByteArray>()
    private val reassembler = FrameReassembler { queue.put(it) }

    private val sends = ConcurrentHashMap<Long, Arena>()
    private val sendIdGen = AtomicLong(1)

    private val closeSignaled = AtomicBoolean(false)
    private val shutdownRequested = AtomicBoolean(false)

    // ---- EdgeConnection ---------------------------------------------------------------------------

    override fun receive(): ByteArray? {
        val frame = queue.take()
        return if (frame === CLOSE_SENTINEL) null else frame
    }

    override fun send(frame: ByteArray) {
        val sendId = sendIdGen.getAndIncrement()
        val total = LENGTH_PREFIX + frame.size
        // Per-send arena: the QUIC_BUFFER + the [len||payload] bytes must outlive send() and stay
        // pinned until SEND_COMPLETE. Registered BEFORE StreamSend so the callback can never race ahead
        // of the map insert.
        val sendArena = Arena.ofShared()
        val data = sendArena.allocate(total.toLong())
        data.set(JAVA_BYTE, 0L, (frame.size ushr 24).toByte())
        data.set(JAVA_BYTE, 1L, (frame.size ushr 16).toByte())
        data.set(JAVA_BYTE, 2L, (frame.size ushr 8).toByte())
        data.set(JAVA_BYTE, 3L, frame.size.toByte())
        if (frame.isNotEmpty()) {
            MemorySegment.copy(frame, 0, data, JAVA_BYTE, LENGTH_PREFIX.toLong(), frame.size)
        }
        val qbuf = sendArena.allocate(Layouts.QUIC_BUFFER)
        qbuf.set(JAVA_INT, Layouts.BUFFER_LENGTH_OFF, total)
        qbuf.set(ADDRESS, Layouts.BUFFER_BUFFER_OFF, data)

        sends[sendId] = sendArena
        val status = api.streamSend(
            stream, qbuf, 1, Constants.QUIC_SEND_FLAG_NONE, CallbackRegistry.contextFor(sendId),
        )
        if (!Constants.succeeded(status)) {
            // No SEND_COMPLETE will fire for a failed send, so free the buffer here to avoid a leak.
            sends.remove(sendId)?.close()
            error("StreamSend failed: 0x%08x".format(status))
        }
    }

    override fun close() {
        // Trigger a graceful teardown; the actual StreamClose/ConnectionClose + arena release happen in
        // the SHUTDOWN_COMPLETE callbacks (calling Close here would double-close them). Idempotent.
        if (shutdownRequested.compareAndSet(false, true)) {
            runCatching {
                api.streamShutdown(stream, Constants.QUIC_STREAM_SHUTDOWN_FLAG_GRACEFUL, 0L)
            }
            runCatching {
                api.connectionShutdown(connection, Constants.QUIC_CONNECTION_SHUTDOWN_FLAG_NONE, 0L)
            }
        }
        signalClose()
    }

    // ---- Stream callback dispatch (called from the transport's stream upcall) ---------------------

    /**
     * Dispatches one STREAM_EVENT. Reads `Type` at offset 0, branches on the union at [Layouts.UNION_OFFSET].
     * RECEIVE bytes are valid ONLY for this call, so they are copied into the reassembler immediately;
     * we consume everything and return SUCCESS. [event] is already widened by the upcall.
     */
    fun handleStreamEvent(handle: MemorySegment, event: MemorySegment): Int {
        when (Layouts.eventType(event)) {
            Constants.QUIC_STREAM_EVENT_RECEIVE -> onReceive(event)
            Constants.QUIC_STREAM_EVENT_SEND_COMPLETE -> onSendComplete(event)
            Constants.QUIC_STREAM_EVENT_PEER_SEND_SHUTDOWN -> signalClose()
            Constants.QUIC_STREAM_EVENT_SHUTDOWN_COMPLETE -> {
                signalClose()
                runCatching { api.streamClose(handle) }
            }
            else -> Unit
        }
        return Constants.QUIC_STATUS_SUCCESS
    }

    private fun onReceive(event: MemorySegment) {
        val bufferCount = event.get(JAVA_INT, Layouts.UNION_OFFSET + Layouts.STREAM_RECEIVE_BUFFER_COUNT_OFF)
        if (bufferCount <= 0) return
        val buffersPtr = event.get(ADDRESS, Layouts.UNION_OFFSET + Layouts.STREAM_RECEIVE_BUFFERS_OFF)
        // The Buffers pointer comes back zero-length; widen to cover BufferCount × 16-byte QUIC_BUFFERs.
        val buffers = buffersPtr.reinterpret(bufferCount.toLong() * Layouts.QUIC_BUFFER.byteSize())
        for (i in 0 until bufferCount) {
            val base = i.toLong() * Layouts.QUIC_BUFFER.byteSize()
            val len = buffers.get(JAVA_INT, base + Layouts.BUFFER_LENGTH_OFF)
            if (len <= 0) continue
            val dataPtr = buffers.get(ADDRESS, base + Layouts.BUFFER_BUFFER_OFF)
            val data = dataPtr.reinterpret(len.toLong())
            val chunk = ByteArray(len)
            MemorySegment.copy(data, JAVA_BYTE, 0L, chunk, 0, len)
            reassembler.feed(chunk)
        }
    }

    private fun onSendComplete(event: MemorySegment) {
        val clientCtx = event.get(ADDRESS, Layouts.UNION_OFFSET + Layouts.STREAM_SEND_COMPLETE_CLIENT_CTX_OFF)
        val sendId = clientCtx.address()
        sends.remove(sendId)?.close()
    }

    // ---- lifecycle helpers used by the connection callbacks ---------------------------------------

    /** Enqueues the CLOSE sentinel exactly once, so a blocked [receive] returns null. */
    fun signalClose() {
        if (closeSignaled.compareAndSet(false, true)) {
            queue.put(CLOSE_SENTINEL)
        }
    }

    /**
     * Releases ALL native memory this connection owns: any send arenas whose SEND_COMPLETE never fired
     * (abrupt shutdown), then the per-connection arena. MUST be called only after the connection's
     * SHUTDOWN_COMPLETE, when msquic can no longer reference this stream.
     */
    fun disposeNativeMemory() {
        sends.values.forEach { runCatching { it.close() } }
        sends.clear()
        runCatching { arena.close() }
    }

    private companion object {
        const val LENGTH_PREFIX = 4

        /** Distinct instance so a genuine empty frame is never mistaken for close (identity compare). */
        val CLOSE_SENTINEL = ByteArray(0)
    }
}
