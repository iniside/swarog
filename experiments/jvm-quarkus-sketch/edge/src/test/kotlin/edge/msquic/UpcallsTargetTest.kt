package edge.msquic

import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Test
import java.lang.foreign.MemorySegment

/**
 * Pure-unit tests of [Upcalls.Target.invoke] — the native-boundary guard. `Target` is unit-constructable
 * WITHOUT any [MsQuicApi]/native handle (only [Upcalls.stub] touches the [java.lang.foreign.Linker]), so
 * this is a critical safety net testable directly: any [Throwable] escaping a [Upcalls.Handler] must be
 * swallowed and turned into `QUIC_STATUS_INTERNAL_ERROR`, never propagated across the would-be native
 * frame; a successful result passes through unchanged.
 */
class UpcallsTargetTest {

    private val nullSeg: MemorySegment = MemorySegment.NULL

    @Test
    fun `a handler throwing an Exception becomes QUIC_STATUS_INTERNAL_ERROR`() {
        val target = Upcalls.Target { _, _, _ -> throw IllegalStateException("handler boom") }

        assertEquals(Constants.QUIC_STATUS_INTERNAL_ERROR, target.invoke(nullSeg, nullSeg, nullSeg))
    }

    @Test
    fun `a handler throwing a non-Exception Throwable is still swallowed (Throwable guard, not just Exception)`() {
        val target = Upcalls.Target { _, _, _ -> throw AssertionError("fatal-ish") }

        assertEquals(Constants.QUIC_STATUS_INTERNAL_ERROR, target.invoke(nullSeg, nullSeg, nullSeg))
    }

    @Test
    fun `a successful handler result passes through unchanged`() {
        val target = Upcalls.Target { _, _, _ -> Constants.QUIC_STATUS_SUCCESS }

        assertEquals(Constants.QUIC_STATUS_SUCCESS, target.invoke(nullSeg, nullSeg, nullSeg))
    }
}
