package edge

import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertThrows
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import java.util.concurrent.CompletableFuture
import java.util.concurrent.LinkedBlockingQueue
import java.util.concurrent.TimeUnit
import java.util.concurrent.TimeoutException

/**
 * Pure-unit tests of [EdgeClient] over a fake in-JVM [EdgeConnection] (no loopback server, no native).
 * Locks the CURRENT behavior of the demux/correlation core, including two known-buggy edges that the
 * deferred concurrency fix (§Bugs #2) will later change: a duplicate cid overwrites the first pending
 * future (which then never completes), and closing the connection does NOT unblock an in-flight request.
 */
class EdgeClientTest {

    private val codec = EdgeCodec()

    /** A controllable [EdgeConnection]: `sent` captures outbound frames; [deliver] feeds inbound ones. */
    private class FakeConnection : EdgeConnection {
        val sent = LinkedBlockingQueue<ByteArray>()
        private val inbound = LinkedBlockingQueue<ByteArray>()

        fun deliver(frame: ByteArray) {
            inbound.put(frame)
        }

        override fun receive(): ByteArray? {
            val frame = inbound.take()
            return if (frame === CLOSED) null else frame
        }

        override fun send(frame: ByteArray) {
            sent.put(frame)
        }

        override fun close() {
            inbound.put(CLOSED)
        }

        private companion object {
            val CLOSED = ByteArray(0)
        }
    }

    private fun waitUntilSentCount(conn: FakeConnection, n: Int) {
        val deadline = System.nanoTime() + 2_000_000_000L
        while (conn.sent.size < n) {
            check(System.nanoTime() < deadline) { "timed out waiting for $n sent frames" }
            Thread.sleep(5)
        }
    }

    /** Spawns a daemon that answers the next outbound Request with [reply]'s Response, then delivers it. */
    private fun respondOnce(conn: FakeConnection, reply: (Request) -> Response) {
        Thread {
            val req = codec.decode(conn.sent.take()) as Request
            conn.deliver(codec.encode(reply(req)))
        }.apply {
            isDaemon = true
            start()
        }
    }

    @Test
    fun `a duplicate cid overwrites the first pending future which then never completes`() {
        val conn = FakeConnection()
        EdgeClient(conn, codec).also { it.start() }.let { client ->
            val cid = 7L
            val first = CompletableFuture.supplyAsync {
                runCatching { client.requestWithCid(cid, "m", ListCharactersRequest("a"), timeoutMs = 300) }
            }
            waitUntilSentCount(conn, 1)
            val second = CompletableFuture.supplyAsync {
                runCatching { client.requestWithCid(cid, "m", ListCharactersRequest("b"), timeoutMs = 300) }
            }
            waitUntilSentCount(conn, 2)

            // ONE response for cid 7 completes only the CURRENT pending future (the second call).
            conn.deliver(codec.encode(Response(cid, ok = true, payload = ByteArray(0))))

            val r2 = second.get(2, TimeUnit.SECONDS)
            val r1 = first.get(2, TimeUnit.SECONDS)
            assertTrue(r2.isSuccess, "the current (second) future should complete")
            assertTrue(r1.isFailure, "the overwritten (first) future never completes")
            assertTrue(r1.exceptionOrNull() is TimeoutException, "current behavior: overwritten future times out")
        }
    }

    @Test
    fun `request times out with a TimeoutException when no reply arrives`() {
        val conn = FakeConnection()
        EdgeClient(conn, codec).also { it.start() }.let { client ->
            assertThrows(TimeoutException::class.java) {
                client.request("m", ListCharactersRequest("p"), timeoutMs = 150)
            }
        }
    }

    @Test
    fun `nextPush returns null when no push arrives before the timeout`() {
        val conn = FakeConnection()
        EdgeClient(conn, codec).also { it.start() }.let { client ->
            assertNull(client.nextPush(timeoutMs = 120))
        }
    }

    @Test
    fun `call throws IllegalArgumentException with the default message on ok=false and null error`() {
        val conn = FakeConnection()
        EdgeClient(conn, codec).also { it.start() }.let { client ->
            respondOnce(conn) { req -> Response(req.cid, ok = false, payload = ByteArray(0), error = null) }
            val ex = assertThrows(IllegalArgumentException::class.java) {
                client.call("m", ListCharactersRequest("p"), CharactersReply::class.java)
            }
            assertTrue(requireNotNull(ex.message).contains("<no error message>"))
        }
    }

    @Test
    fun `closing the connection does NOT unblock a pending request today (locks buggy behavior)`() {
        // §Bugs #2 (DEFERRED FIX): a request in flight when the connection dies SHOULD fail fast. Today
        // the reader thread just sees receive()==null and exits WITHOUT failing pending futures, so the
        // call hangs until its own timeout. This test pins that current behavior; the fix is a separate
        // reviewed step (do NOT "fix" it here).
        val conn = FakeConnection()
        EdgeClient(conn, codec).also { it.start() }.let { client ->
            val timeoutMs = 300L
            val started = System.nanoTime()
            val call = CompletableFuture.supplyAsync {
                runCatching { client.request("slow.method", ListCharactersRequest("p"), timeoutMs) }
            }
            waitUntilSentCount(conn, 1)
            Thread.sleep(50) // close ~50ms in — a would-be fix should unblock here; today it does not
            conn.close()

            val result = call.get(2, TimeUnit.SECONDS)
            val elapsedMs = (System.nanoTime() - started) / 1_000_000
            assertTrue(result.isFailure)
            assertTrue(
                result.exceptionOrNull() is TimeoutException,
                "current behavior: the pending call times out rather than failing on close",
            )
            assertTrue(elapsedMs >= timeoutMs - 60, "close at ~50ms must NOT unblock early; elapsed=$elapsedMs ms")
        }
    }
}
