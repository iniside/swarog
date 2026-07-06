package edge

import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertThrows
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import java.util.concurrent.CompletableFuture
import java.util.concurrent.CyclicBarrier
import java.util.concurrent.LinkedBlockingQueue
import java.util.concurrent.TimeUnit
import java.util.concurrent.TimeoutException

/**
 * Pure-unit tests of [EdgeClient] over a fake in-JVM [EdgeConnection] (no loopback server, no native).
 * Locks the demux/correlation core AND the §Bugs #2 connection-death fix: a call in flight when the
 * connection dies now fails FAST with a [ConnectionClosedException] (not a slow [TimeoutException]),
 * and a `requestWithCid` racing a concurrent `close()` always resolves — never orphaned. One remaining
 * known edge is still pinned: a duplicate cid overwrites the first pending future (that overwritten
 * future is no longer in the map, so the death-drain can't reach it, and it times out as before).
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
    fun `a pending request fails FAST with ConnectionClosedException when the peer closes`() {
        // §Bugs #2 (FIXED): a request in flight when the connection dies must fail fast, NOT hang until
        // its own timeout. The reader sees receive()==null, sets the terminal state and drains pending,
        // so the call unblocks within milliseconds of the close — well under the 2s call timeout.
        val conn = FakeConnection()
        EdgeClient(conn, codec).also { it.start() }.let { client ->
            val timeoutMs = 2_000L
            val started = System.nanoTime()
            val call = CompletableFuture.supplyAsync {
                runCatching { client.request("slow.method", ListCharactersRequest("p"), timeoutMs) }
            }
            waitUntilSentCount(conn, 1)
            Thread.sleep(50) // close ~50ms in — the fix must unblock here, long before the 2s timeout
            conn.close()

            val result = call.get(2, TimeUnit.SECONDS)
            val elapsedMs = (System.nanoTime() - started) / 1_000_000
            assertTrue(result.isFailure, "a call in flight at close must fail, not return a Response")
            assertTrue(
                result.exceptionOrNull() is ConnectionClosedException,
                "must fail with ConnectionClosedException, not ${result.exceptionOrNull()}",
            )
            assertTrue(
                elapsedMs < 500,
                "close at ~50ms must unblock the call FAST (not wait out the 2s timeout); elapsed=$elapsedMs ms",
            )
        }
    }

    @Test
    fun `close() fails an in-flight request FAST with ConnectionClosedException`() {
        val conn = FakeConnection()
        EdgeClient(conn, codec).also { it.start() }.let { client ->
            val started = System.nanoTime()
            val call = CompletableFuture.supplyAsync {
                runCatching { client.request("slow.method", ListCharactersRequest("p"), timeoutMs = 2_000) }
            }
            waitUntilSentCount(conn, 1)
            client.close()

            val result = call.get(2, TimeUnit.SECONDS)
            val elapsedMs = (System.nanoTime() - started) / 1_000_000
            assertTrue(result.isFailure)
            assertTrue(result.exceptionOrNull() is ConnectionClosedException, "was ${result.exceptionOrNull()}")
            assertTrue(elapsedMs < 500, "close() must unblock the pending call fast; elapsed=$elapsedMs ms")
        }
    }

    @Test
    fun `a request racing a concurrent close ALWAYS resolves and never hangs`() {
        // The add-after-drain guard's proof. Each iteration races a requestWithCid against close() from
        // two threads released together. A correct client fails the call FAST with
        // ConnectionClosedException; the orphan bug (insert slips past the drain unfailed) would instead
        // let the call sit until its own timeout — which we treat as FAILURE here. The per-call timeout
        // is generous (1.5s) precisely so that under the fix it is NEVER reached (close fails the call in
        // microseconds); a regression therefore either times out (→ TimeoutException, asserted-against)
        // or hangs past the 4s join (→ test fails). No inbound reply is ever delivered, so the ONLY
        // correct outcome is the connection-closed failure.
        val iterations = 300
        repeat(iterations) { i ->
            val conn = FakeConnection()
            val client = EdgeClient(conn, codec).also { it.start() }
            val barrier = CyclicBarrier(2)
            val outcome = CompletableFuture<Result<Response>>()

            val caller = Thread {
                barrier.await()
                outcome.complete(runCatching { client.request("m", ListCharactersRequest("p"), timeoutMs = 1_500) })
            }.apply { isDaemon = true }
            val closer = Thread {
                barrier.await()
                client.close()
            }.apply { isDaemon = true }
            caller.start()
            closer.start()

            val result = try {
                outcome.get(4, TimeUnit.SECONDS)
            } catch (e: TimeoutException) {
                throw AssertionError("iteration $i HUNG: the racing call never resolved within 4s", e)
            }
            val err = result.exceptionOrNull()
            assertTrue(
                result.isFailure && err is ConnectionClosedException,
                "iteration $i must fail with ConnectionClosedException (orphaned call would time out); " +
                    "was ${err?.toString() ?: "null"}",
            )
        }
    }
}
