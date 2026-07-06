package charactersclient

import characters.charactersapi.CharactersUnavailableException
import characters.charactersapi.OwnerOfReply
import characters.charactersapi.OwnerOfRequest
import edge.EdgeCodec
import edge.EdgeConnection
import edge.EdgeRouter
import edge.EdgeServer
import edge.LoopbackTransport
import edge.typedHandler
import java.util.UUID
import java.util.concurrent.atomic.AtomicInteger
import org.junit.jupiter.api.Assertions.assertDoesNotThrow
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertSame
import org.junit.jupiter.api.Assertions.assertThrows
import org.junit.jupiter.api.Test

/**
 * P1-CLIENT: [EdgeRemotePlayerCharacters]'s ownerOf semantics driven through seam #1 — the injectable
 * `connect` boundary. The happy/reconnect paths dial a REAL in-JVM [LoopbackTransport]-backed
 * [EdgeServer] whose router answers `characters.ownerOf`, so the full msgpack request/response round
 * trip over [edge.EdgeClient] is exercised with zero network and zero native QUIC. The failure paths
 * feed `connect` an exception schedule to prove the bounded single-retry contract and the DISTINCT
 * [CharactersUnavailableException] (vs null "no such character").
 *
 * Note: constructing the adapter still eagerly builds its retained native `MsQuicClientTransport`
 * (the injected `connect` only replaces the DIAL, not the field) — hence the module's test task grants
 * `--enable-native-access=ALL-UNNAMED`. The fake `connect` means that transport is never dialed.
 */
class EdgeRemoteOwnerOfTest {

    /** A serving loopback whose `characters.ownerOf` handler returns [reply]. Each `connect()` opens a
     *  fresh in-JVM connection to the same server (so a reconnect dials a genuinely new connection). */
    private class OwnerOfServer(reply: OwnerOfReply) {
        private val codec = EdgeCodec()
        private val transport = LoopbackTransport()

        init {
            val router = EdgeRouter()
            router.register("characters.ownerOf", codec.typedHandler<OwnerOfRequest, OwnerOfReply> { reply })
            EdgeServer(router, transport, codec).start()
        }

        fun connect(): EdgeConnection = transport.connect()
    }

    @Test
    fun `ownerOf returns the reply UUID on the happy path`() {
        val ownerId = UUID.randomUUID()
        val server = OwnerOfServer(OwnerOfReply(found = true, ownerId = ownerId.toString()))
        val remote = EdgeRemotePlayerCharacters("chars-host:9100") { _, _ -> server.connect() }

        assertEquals(ownerId, remote.ownerOf(123L))
    }

    @Test
    fun `ownerOf returns null when the reply carries a null ownerId - no such character`() {
        val server = OwnerOfServer(OwnerOfReply(found = false, ownerId = null))
        val remote = EdgeRemotePlayerCharacters("chars-host:9100") { _, _ -> server.connect() }

        // A reachable provider that answers "not owned" is null, NOT an exception.
        assertNull(remote.ownerOf(999L))
    }

    @Test
    fun `a first-dial failure is recovered by the single reconnect and returns the reply`() {
        val ownerId = UUID.randomUUID()
        val server = OwnerOfServer(OwnerOfReply(found = true, ownerId = ownerId.toString()))
        val dials = AtomicInteger(0)
        // 1st dial fails (dead connection), 2nd dial succeeds — proves the single invalidate + retry.
        val remote = EdgeRemotePlayerCharacters("chars-host:9100") { _, _ ->
            if (dials.getAndIncrement() == 0) error("connection refused (first dial)") else server.connect()
        }

        assertEquals(ownerId, remote.ownerOf(7L))
        assertEquals(2, dials.get(), "exactly two dials: the failed first + the successful retry")
    }

    @Test
    fun `two consecutive dial failures surface CharactersUnavailableException, never null`() {
        val remote = EdgeRemotePlayerCharacters("chars-host:9100") { _, _ -> error("provider down") }

        assertThrows(CharactersUnavailableException::class.java) { remote.ownerOf(1L) }
    }

    @Test
    fun `the unavailable exception chains the retry as cause and suppresses the first failure`() {
        val first = IllegalStateException("dial failure #1")
        val second = IllegalStateException("dial failure #2")
        val dials = AtomicInteger(0)
        val remote = EdgeRemotePlayerCharacters("chars-host:9100") { _, _ ->
            throw if (dials.getAndIncrement() == 0) first else second
        }

        val ex = assertThrows(CharactersUnavailableException::class.java) { remote.ownerOf(1L) }

        // The post-reconnect (second) failure is the cause; the production code attaches the original
        // (first) failure as suppressed ON THAT CAUSE (`retry.addSuppressed(e)`), so the root-cause
        // reason is never lost. The thrown exception itself carries no suppressed entries.
        val cause = ex.cause
        assertSame(second, cause, "cause must be the retry (second) failure")
        assertEquals(0, ex.suppressed.size, "the suppressed failure hangs off the cause, not the wrapper")
        assertEquals(1, cause!!.suppressed.size)
        assertSame(first, cause.suppressed[0], "the first failure is preserved as the cause's suppressed[0]")
    }

    // --- target parse edges beyond the existing 3 (EdgeRemotePlayerCharactersTest) ------------------

    @Test
    fun `a non-numeric port throws NumberFormatException in init before touching the transport`() {
        // idx = lastIndexOf(':') passes the require, then "abc".toInt() fails — thrown BEFORE the
        // native `transport` field initializes (same fast-fail the existing parse tests rely on).
        assertThrows(NumberFormatException::class.java) { EdgeRemotePlayerCharacters("host:abc") }
    }

    @Test
    fun `a negative port is currently unvalidated and parses - locked`() {
        // "-1".toInt() == -1 is a valid Int, so construction SUCCEEDS today (no range validation).
        // Pin the current behavior so a future validation change is a deliberate, visible edit.
        assertDoesNotConstructFail("host:-1")
    }

    @Test
    fun `an IPv6-ish target parses on the last colon - locked`() {
        // lastIndexOf(':') picks the colon before "9100", so host="::1", port=9100 — it parses.
        assertDoesNotConstructFail("::1:9100")
    }

    private fun assertDoesNotConstructFail(target: String) {
        // Constructs the real (never-dialed) native transport, hence the module's native-access flag.
        assertDoesNotThrow { EdgeRemotePlayerCharacters(target) }
    }
}
