package edge

import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertNotNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

/**
 * Tests the [EdgeServer] per-connection reader's decode guard (§Bugs #3). A malformed/corrupt frame
 * used to throw UNCAUGHT inside the daemon reader thread, silently killing that connection's whole
 * service loop. The fix logs the bad frame and CONTINUES serving, so a later valid request on the
 * SAME connection is still answered — proving the loop did not die.
 */
class EdgeServerTest {

    private val codec = EdgeCodec()

    @Test
    fun `a malformed frame is dropped and the connection keeps serving`() {
        val router = EdgeRouter().also { EdgeDemo.register(it, codec) }
        val transport = LoopbackTransport()
        EdgeServer(router, transport, codec).start()

        // Raw client-side connection: send bytes directly, bypassing EdgeClient's own decode guard.
        val conn = transport.connect()

        // A frame that is NOT a valid msgpack `Frame` object — decode must throw, be logged, and skipped.
        conn.send("this is not a valid frame".toByteArray())

        // A valid Request on the SAME connection AFTER the garbage: if the reader loop survived, it is answered.
        val cid = 99L
        conn.send(codec.encode(Request(cid, "characters.list", codec.encodePayload(ListCharactersRequest("p")))))

        val replyBytes = conn.receive()
        assertNotNull(replyBytes, "server must still answer a valid request after a malformed one")
        val resp = codec.decode(requireNotNull(replyBytes)) as Response
        assertEquals(cid, resp.cid, "the surviving loop answered the valid request with the right cid")
        assertTrue(resp.ok, "expected ok response, got error=${resp.error ?: "<none>"}")

        val reply = codec.decodePayload(resp.payload, CharactersReply::class.java)
        assertEquals(listOf("Aria", "Borin", "Cael"), reply.names)
    }
}
