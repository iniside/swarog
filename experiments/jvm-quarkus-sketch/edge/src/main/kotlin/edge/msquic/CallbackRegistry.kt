package edge.msquic

import java.lang.foreign.MemorySegment
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.atomic.AtomicLong

/**
 * Maps a msquic callback `void* Context` to a JVM object WITHOUT any native memory cell (plan N2).
 *
 * The context pointer we hand to msquic is NOT a real address — it is a small monotonic id smuggled
 * through `void*` via [MemorySegment.ofAddress]. msquic never dereferences it; it only echoes it back
 * to our upcall, where [resolve] turns it back into the registered object. This sidesteps every
 * arena-lifetime / use-after-free hazard that a real native `Context` cell would introduce.
 *
 * Ids start at 1 so a genuine NULL context (address 0) can never collide with a live registration.
 */
object CallbackRegistry {
    private val entries = ConcurrentHashMap<Long, Any>()
    private val seq = AtomicLong(1)

    /** Registers [obj] and returns its id — pass [contextFor] of this id as the msquic `void* ctx`. */
    fun register(obj: Any): Long {
        val id = seq.getAndIncrement()
        entries[id] = obj
        return id
    }

    /** The opaque pointer to hand msquic for [id]. Never dereferenced by native code. */
    fun contextFor(id: Long): MemorySegment = MemorySegment.ofAddress(id)

    /** Resolves the object for a context pointer echoed back by msquic, or null if unknown/NULL. */
    fun resolve(ctx: MemorySegment): Any? = entries[ctx.address()]

    /** Drops a registration (e.g. after SHUTDOWN_COMPLETE) so its object can be GC'd. */
    fun unregister(id: Long) {
        entries.remove(id)
    }
}
