package edge.msquic

/**
 * Per-stream frame de-chunker. QUIC is a byte pipe: one logical frame can arrive split across several
 * RECEIVE events, and one RECEIVE can carry several frames back-to-back (plus a partial trailing one).
 * The wire framing is a **4-byte big-endian length prefix + that many payload bytes**; [feed] appends
 * the bytes copied out of a RECEIVE and emits — via [onFrame] — every payload that has become complete,
 * carrying any partial trailing bytes forward to the next [feed].
 *
 * NOT thread-safe by design: msquic delivers a single stream's RECEIVE events serially on one worker,
 * so exactly one thread ever calls [feed] for a given stream. [onFrame] typically enqueues onto the
 * connection's [java.util.concurrent.BlockingQueue].
 */
class FrameReassembler(private val onFrame: (ByteArray) -> Unit) {

    private var buf = ByteArray(INITIAL_CAPACITY)
    private var count = 0 // number of valid bytes at the front of [buf]

    fun feed(bytes: ByteArray) {
        if (bytes.isEmpty()) return
        ensureCapacity(count + bytes.size)
        System.arraycopy(bytes, 0, buf, count, bytes.size)
        count += bytes.size

        var offset = 0
        // As long as a 4-byte header AND its full payload are buffered, cut and emit a frame.
        while (count - offset >= LENGTH_PREFIX) {
            val len = ((buf[offset].toInt() and 0xFF) shl 24) or
                ((buf[offset + 1].toInt() and 0xFF) shl 16) or
                ((buf[offset + 2].toInt() and 0xFF) shl 8) or
                (buf[offset + 3].toInt() and 0xFF)
            require(len in 0..MAX_FRAME_BYTES) { "frame length $len out of bounds (corrupt stream?)" }
            if (count - offset - LENGTH_PREFIX < len) break // payload not fully arrived yet
            val start = offset + LENGTH_PREFIX
            onFrame(buf.copyOfRange(start, start + len))
            offset = start + len
        }

        // Shift the unconsumed tail (a partial frame) back to the front for the next feed.
        if (offset > 0) {
            System.arraycopy(buf, offset, buf, 0, count - offset)
            count -= offset
        }
    }

    private fun ensureCapacity(needed: Int) {
        if (needed <= buf.size) return
        var newSize = buf.size
        while (newSize < needed) newSize = newSize shl 1
        buf = buf.copyOf(newSize)
    }

    private companion object {
        const val LENGTH_PREFIX = 4
        const val INITIAL_CAPACITY = 4 * 1024
        const val MAX_FRAME_BYTES = 64 * 1024 * 1024 // guard against a corrupt length triggering OOM
    }
}
