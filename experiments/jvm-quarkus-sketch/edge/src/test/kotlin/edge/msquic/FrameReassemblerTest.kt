package edge.msquic

import org.junit.jupiter.api.Assertions.assertArrayEquals
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertThrows
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

/**
 * Pure-unit tests of [FrameReassembler] — the QUIC-stream de-chunker (4-byte big-endian length prefix +
 * payload). No native, no stream. Locks: a frame split across feeds is buffered then emitted once
 * complete; several frames in one feed all emit in order; a partial trailing frame is carried forward;
 * and a corrupt length header (outside `0..MAX`) throws instead of allocating wildly.
 */
class FrameReassemblerTest {

    private fun framed(payload: ByteArray): ByteArray {
        val out = ByteArray(LENGTH_PREFIX + payload.size)
        out[0] = (payload.size ushr 24).toByte()
        out[1] = (payload.size ushr 16).toByte()
        out[2] = (payload.size ushr 8).toByte()
        out[3] = payload.size.toByte()
        System.arraycopy(payload, 0, out, LENGTH_PREFIX, payload.size)
        return out
    }

    @Test
    fun `a frame split across two feeds is reassembled once fully arrived`() {
        val frames = mutableListOf<ByteArray>()
        val reassembler = FrameReassembler { frames.add(it) }
        val wire = framed("hello world".toByteArray())

        reassembler.feed(wire.copyOfRange(0, 3)) // only 3 of the 4 header bytes
        assertTrue(frames.isEmpty(), "no frame should emit before the header + payload are complete")

        reassembler.feed(wire.copyOfRange(3, wire.size))
        assertEquals(1, frames.size)
        assertArrayEquals("hello world".toByteArray(), frames[0])
    }

    @Test
    fun `several frames in one feed are all delivered in order`() {
        val frames = mutableListOf<ByteArray>()
        val reassembler = FrameReassembler { frames.add(it) }

        reassembler.feed(framed("aaa".toByteArray()) + framed("bb".toByteArray()) + framed("c".toByteArray()))

        assertEquals(3, frames.size)
        assertArrayEquals("aaa".toByteArray(), frames[0])
        assertArrayEquals("bb".toByteArray(), frames[1])
        assertArrayEquals("c".toByteArray(), frames[2])
    }

    @Test
    fun `a partial trailing frame is buffered until the rest arrives`() {
        val frames = mutableListOf<ByteArray>()
        val reassembler = FrameReassembler { frames.add(it) }
        val f1 = framed("first".toByteArray())
        val f2 = framed("second".toByteArray())

        reassembler.feed(f1 + f2.copyOfRange(0, 2)) // whole f1 + 2 header bytes of f2
        assertEquals(1, frames.size)
        assertArrayEquals("first".toByteArray(), frames[0])

        reassembler.feed(f2.copyOfRange(2, f2.size))
        assertEquals(2, frames.size)
        assertArrayEquals("second".toByteArray(), frames[1])
    }

    @Test
    fun `a corrupt length header outside the legal range throws`() {
        // len = -1 (all 0xFF) is < 0.
        assertThrows(IllegalArgumentException::class.java) {
            FrameReassembler { }.feed(byteArrayOf(0xFF.toByte(), 0xFF.toByte(), 0xFF.toByte(), 0xFF.toByte()))
        }
        // len = 0x7FFFFFFF (~2 GiB) is > MAX_FRAME_BYTES (64 MiB).
        assertThrows(IllegalArgumentException::class.java) {
            FrameReassembler { }.feed(byteArrayOf(0x7F, 0xFF.toByte(), 0xFF.toByte(), 0xFF.toByte()))
        }
    }

    private companion object {
        const val LENGTH_PREFIX = 4
    }
}
