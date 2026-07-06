package edge

import org.junit.jupiter.api.Assertions.assertArrayEquals
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

/**
 * Pure-unit tests of [EdgeRouter] in isolation — no transport, no client/server, no native. Locks the
 * dispatch contract: a registered handler's bytes become an ok [Response] carrying the request cid; an
 * unknown method and any handler throw become `ok=false` error responses (never propagate); the
 * null-message throw falls back to `e.toString()`; and a re-register is last-wins.
 */
class EdgeRouterTest {

    @Test
    fun `dispatch runs the registered handler and wraps its bytes in an ok Response with the request cid`() {
        val reply = byteArrayOf(7, 7)
        val router = EdgeRouter()
        router.register("m") { _ -> reply }

        val resp = router.dispatch(Request(3L, "m", byteArrayOf(1)))

        assertTrue(resp.ok)
        assertEquals(3L, resp.cid)
        assertArrayEquals(reply, resp.payload)
    }

    @Test
    fun `an unknown method yields an error Response`() {
        val resp = EdgeRouter().dispatch(Request(1L, "nope", ByteArray(0)))

        assertFalse(resp.ok)
        assertEquals(1L, resp.cid)
        assertTrue(resp.error?.contains("no such method: nope") == true, "was: ${resp.error ?: "<none>"}")
    }

    @Test
    fun `a handler throwing with a message yields that message as the error`() {
        val router = EdgeRouter()
        router.register("boom") { throw IllegalStateException("kaboom") }

        val resp = router.dispatch(Request(1L, "boom", ByteArray(0)))

        assertFalse(resp.ok)
        assertEquals("kaboom", resp.error)
    }

    @Suppress("ThrowingExceptionsWithoutMessageOrCause") // deliberate: exercises the `e.message ?: e.toString()`
    // fallback branch — the thrown exception MUST have a null message for the test to bite.
    @Test
    fun `a handler throwing with a null message falls back to the exception toString`() {
        val router = EdgeRouter()
        router.register("boom") { throw IllegalStateException() }

        val resp = router.dispatch(Request(1L, "boom", ByteArray(0)))

        assertFalse(resp.ok)
        assertEquals("java.lang.IllegalStateException", resp.error)
    }

    @Test
    fun `registering the same method twice is last-wins`() {
        val router = EdgeRouter()
        router.register("m") { _ -> byteArrayOf(1) }
        router.register("m") { _ -> byteArrayOf(2) }

        val resp = router.dispatch(Request(1L, "m", ByteArray(0)))

        assertArrayEquals(byteArrayOf(2), resp.payload)
    }

    @Test
    fun `a prefix handler serves any method starting with the prefix`() {
        val router = EdgeRouter()
        router.registerPrefix("characters.") { _ -> byteArrayOf(1) }

        val resp = router.dispatch(Request(1L, "characters.list", ByteArray(0)))

        assertTrue(resp.ok)
        assertArrayEquals(byteArrayOf(1), resp.payload)
    }

    @Test
    fun `an exact registration wins over a matching prefix`() {
        val router = EdgeRouter()
        router.registerPrefix("characters.") { _ -> byteArrayOf(9) }
        router.register("characters.list") { _ -> byteArrayOf(1) }

        val resp = router.dispatch(Request(1L, "characters.list", ByteArray(0)))

        assertArrayEquals(byteArrayOf(1), resp.payload, "exact must shadow the prefix")
    }

    @Test
    fun `the longest matching prefix wins`() {
        val router = EdgeRouter()
        router.registerPrefix("characters.") { _ -> byteArrayOf(1) }
        router.registerPrefix("characters.admin.") { _ -> byteArrayOf(2) }

        val resp = router.dispatch(Request(1L, "characters.admin.purge", ByteArray(0)))

        assertArrayEquals(byteArrayOf(2), resp.payload, "the more specific (longer) prefix must win")
    }

    @Test
    fun `a method matching no exact entry and no prefix still yields the no-such-method error`() {
        val router = EdgeRouter()
        router.registerPrefix("characters.") { _ -> byteArrayOf(1) }

        val resp = router.dispatch(Request(1L, "inventory.list", ByteArray(0)))

        assertFalse(resp.ok)
        assertTrue(
            resp.error?.contains("no such method: inventory.list") == true,
            "was: ${resp.error ?: "<none>"}",
        )
    }
}
