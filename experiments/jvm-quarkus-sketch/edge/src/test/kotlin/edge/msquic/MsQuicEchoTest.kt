package edge.msquic

import edge.CharactersReply
import edge.EdgeClient
import edge.EdgeCodec
import edge.EdgeRouter
import edge.EdgeServer
import edge.ListCharactersRequest
import edge.typedHandler
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNotNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Assumptions.assumeTrue
import org.junit.jupiter.api.Test

/**
 * Krok 5 milestone: prove the edge RPC core runs over REAL QUIC/UDP end-to-end. This is where all the
 * compile-only callback/handshake/framing code from Kroki 2-3 executes live for the first time:
 *   MsQuicServerTransport (schannel CERTIFICATE_HASH) + EdgeServer  ←UDP/QUIC→  MsQuicClientTransport
 *   (NONE|NO_VALIDATION) + EdgeClient, a real request/response round-trip on localhost.
 *
 * Gated on a self-signed cert being present in `cert:\CurrentUser\My` (thumbprint via system property
 * `edge.test.cert-thumbprint`, defaulted to the provisioned GameBackend-Edge cert). A future CI without
 * a cert sets the property blank and the test is skipped rather than failing.
 */
class MsQuicEchoTest {

    private val thumbprint: String? =
        System.getProperty("edge.test.cert-thumbprint")?.trim()?.takeIf { it.isNotEmpty() }

    @Test
    fun echoRoundTripOverRealQuic() {
        assumeTrue(thumbprint != null, "no server cert thumbprint — skipping live QUIC test")

        val port = 9443
        val codec = EdgeCodec()

        // A router with the demo characters.list handler (canned roster) — no DB, no characters module.
        val router = EdgeRouter().apply {
            register(
                "characters.list",
                codec.typedHandler<ListCharactersRequest, CharactersReply> { req ->
                    require(req.playerId.isNotBlank()) { "playerId is required" }
                    CharactersReply(names = listOf("Aria", "Borin", "Cael"))
                },
            )
        }

        val serverTransport = MsQuicServerTransport(port, thumbprint!!)
        var clientTransport: MsQuicClientTransport? = null
        var connection: edge.EdgeConnection? = null
        try {
            EdgeServer(router, serverTransport, codec).start()

            clientTransport = MsQuicClientTransport()
            val conn = clientTransport.connect("localhost", port)
            connection = conn
            val client = EdgeClient(conn, codec).apply { start() }

            // --- happy path: typed round-trip over QUIC -----------------------------------------------
            val reply = client.call(
                "characters.list", ListCharactersRequest("player-1"), CharactersReply::class.java,
            )
            assertEquals(listOf("Aria", "Borin", "Cael"), reply.names, "echoed roster over QUIC")

            // --- cid correlation: the Response must carry the Request's cid --------------------------
            val chosenCid = 42L
            val resp = client.requestWithCid(chosenCid, "characters.list", ListCharactersRequest("player-2"))
            assertEquals(chosenCid, resp.cid, "Response.cid must match the sent Request.cid")
            assertTrue(resp.ok, "characters.list should succeed")
            val decoded = client.decode(resp.payload, CharactersReply::class.java)
            assertEquals(listOf("Aria", "Borin", "Cael"), decoded.names)

            // --- error path: unknown method → ok=false with an error, not a hang/throw ---------------
            val err = client.requestWithCid(99L, "characters.nope", ListCharactersRequest("player-3"))
            assertEquals(99L, err.cid)
            assertFalse(err.ok, "unknown method must not report ok")
            assertNotNull(err.error, "unknown method must carry an error")
            assertTrue(err.error!!.contains("no such method"), "error was: ${err.error}")
        } finally {
            // Close the connection FIRST: the transports' close() calls msquic RegistrationClose, which
            // blocks until every connection has drained — so a still-open connection would hang teardown.
            // connection.close() triggers the graceful stream/connection shutdown that drains it.
            runCatching { connection?.close() }
            runCatching { clientTransport?.close() }
            runCatching { serverTransport.close() }
        }
    }
}
