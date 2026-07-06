package edge

import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNotNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.BeforeEach
import org.junit.jupiter.api.Test

/**
 * Exercises the transport-agnostic RPC core over the in-JVM loopback transport — REAL round-trips,
 * not a compile check. Proves request/response (with cid correlation), handler-error mapping, and
 * server-push, all carried as schema-less MessagePack.
 */
class EdgeLoopbackTest {

    private val codec = EdgeCodec()
    private lateinit var client: EdgeClient
    private lateinit var serverConn: EdgeServerConnection

    @BeforeEach
    fun setUp() {
        val router = EdgeRouter().also { EdgeDemo.register(it, codec) }
        val transport = LoopbackTransport()
        var captured: EdgeServerConnection? = null
        EdgeServer(router, transport, codec, onConnect = { captured = it }).start()

        val connection = transport.connect() // triggers onConnect on the server side
        serverConn = requireNotNull(captured) { "server did not observe the connection" }
        client = EdgeClient(connection, codec).also { it.start() }
    }

    @Test
    fun `request response round-trip returns the reply with a matching cid`() {
        val cid = 42L
        val resp = client.requestWithCid(cid, "characters.list", ListCharactersRequest("player-1"))

        assertTrue(resp.ok, "expected ok response, got error=${resp.error ?: "<none>"}")
        assertEquals(cid, resp.cid, "Response cid must match the Request cid")

        val reply = codec.decodePayload(resp.payload, CharactersReply::class.java)
        assertEquals(listOf("Aria", "Borin", "Cael"), reply.names)
    }

    @Suppress("NullableToStringCall") // false positive: `resp.error` is `String?`, but by this point
    // in the statement it has already been forced non-null (via `assertNotNull` / the preceding
    // `resp.error!!` in the same expression) — K2 smart-casts the re-read to non-null for this
    // stable `val` property, so the interpolation can never actually print "null". Detekt's own
    // (older, separately embedded) resolution doesn't see that smart-cast.
    @Test
    fun `handler exception becomes an error response`() {
        val resp = client.request("characters.boom", ListCharactersRequest("player-1"))

        assertFalse(resp.ok, "throwing handler must yield ok=false")
        assertNotNull(resp.error)
        assertTrue(
            resp.error!!.contains("character service unavailable"),
            "error should carry the handler message, was: ${resp.error}",
        )
    }

    @Suppress("NullableToStringCall") // same detekt/K2 smart-cast mismatch as the test above —
    // `resp.error!!` earlier in the same statement already forces it non-null.
    @Test
    fun `unknown method yields an error response`() {
        val resp = client.request("does.not.exist", ListCharactersRequest("player-1"))

        assertFalse(resp.ok)
        assertTrue(resp.error!!.contains("no such method"), "was: ${resp.error}")
    }

    @Test
    fun `server push is received and decoded by the client`() {
        serverConn.push("characters.created", CharacterCreatedPush("player-1", "Dax"))

        val push = client.nextPush()
        assertNotNull(push, "client did not receive the server push")
        assertEquals(0L, push!!.cid, "an unsolicited push carries cid 0")
        assertEquals("characters.created", push.topic)

        val decoded = client.decode(push.payload, CharacterCreatedPush::class.java)
        assertEquals("player-1", decoded.playerId)
        assertEquals("Dax", decoded.name)
    }
}
