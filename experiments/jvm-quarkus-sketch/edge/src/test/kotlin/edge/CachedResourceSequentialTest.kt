package edge

import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertNotSame
import org.junit.jupiter.api.Assertions.assertSame
import org.junit.jupiter.api.Assertions.assertThrows
import org.junit.jupiter.api.Test
import java.util.concurrent.atomic.AtomicInteger

/**
 * Sequential (single-thread) tests of [CachedResource]. The concurrency contract (at-most-one live
 * value, no torn double-check) is covered by [CachedResourceModelCheckingTest] via Lincheck — this
 * suite deliberately does NOT re-assert the get-twice-same-instance tautology and instead locks the
 * failure-path behavior: create failing then recovering, and invalidate clearing the cache even when
 * close throws.
 */
class CachedResourceSequentialTest {

    @Test
    fun `get rebuilds after create throws once (the cache is not poisoned)`() {
        val resource = Any()
        val calls = AtomicInteger(0)
        val cache = CachedResource(
            create = { if (calls.getAndIncrement() == 0) throw IllegalStateException("boom") else resource },
            close = { },
        )

        assertThrows(IllegalStateException::class.java) { cache.get() } // first build fails
        assertSame(resource, cache.get()) // retried — cache was not poisoned by the failure
        assertSame(resource, cache.get()) // now cached — no third create
        assertEquals(2, calls.get())
    }

    @Test
    fun `invalidate clears the cache even when close throws so the next get rebuilds`() {
        val built = mutableListOf<Any>()
        val cache = CachedResource(
            create = { Any().also { built.add(it) } },
            close = { throw IllegalStateException("close boom") },
        )

        val first = cache.get()
        cache.invalidate() // close throws — runCatching swallows it, but the cache must still clear
        val second = cache.get()

        assertNotSame(first, second)
        assertEquals(2, built.size)
    }

    @Test
    fun `invalidate on a never-built cache is a no-op and never calls close`() {
        val closeCalls = AtomicInteger(0)
        val cache = CachedResource<Any>(
            create = { throw IllegalStateException("create must not run") },
            close = { closeCalls.incrementAndGet() },
        )

        cache.invalidate()

        assertEquals(0, closeCalls.get())
    }
}
