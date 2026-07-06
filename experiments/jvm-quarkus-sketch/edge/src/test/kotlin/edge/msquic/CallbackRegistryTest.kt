package edge.msquic

import org.junit.jupiter.api.Assertions.assertNotEquals
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertSame
import org.junit.jupiter.api.Test
import java.lang.foreign.MemorySegment

/**
 * Pure-map tests of [CallbackRegistry] — no native memory, no msquic. Locks the smuggled-id contract:
 * register→resolve round-trips the object, unregister drops it, ids are never 0 (so a genuine NULL
 * `void*` context can never collide with a live registration), and an unknown/NULL context resolves null.
 */
class CallbackRegistryTest {

    @Test
    fun `register then resolve returns the same object, and unregister drops it`() {
        val obj = Any()
        val id = CallbackRegistry.register(obj)

        assertSame(obj, CallbackRegistry.resolve(CallbackRegistry.contextFor(id)))

        CallbackRegistry.unregister(id)
        assertNull(CallbackRegistry.resolve(CallbackRegistry.contextFor(id)))
    }

    @Test
    fun `registration ids are never zero`() {
        repeat(5) { assertNotEquals(0L, CallbackRegistry.register(Any())) }
    }

    @Test
    fun `resolving an unknown or NULL context returns null`() {
        assertNull(CallbackRegistry.resolve(CallbackRegistry.contextFor(Long.MAX_VALUE / 2)))
        assertNull(CallbackRegistry.resolve(MemorySegment.NULL))
    }
}
