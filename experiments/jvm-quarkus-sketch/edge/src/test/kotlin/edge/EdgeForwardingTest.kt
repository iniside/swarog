package edge

import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertThrows
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.Timeout
import java.util.concurrent.LinkedBlockingQueue
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicReference

/**
 * The byte-relay crux of the gateway (Step 2). Drives the full gateway shape over the in-JVM loopback
 * transport — NO native QUIC — so the routing/relay logic is proven independently of msquic:
 *
 *   test client → GATEWAY EdgeServer (an [EdgeRouter] whose `characters.` PREFIX is a [ForwardingHandler])
 *              → downstream EdgeServer (a REAL `codec.typedHandler<ListCharactersRequest, CharactersReply>`)
 *              → reply back the same path.
 *
 * The downstream is a TYPED handler on purpose (reviewer #1): it decodes the forwarded payload into a
 * `ListCharactersRequest` and its reply is DERIVED FROM the decoded field. So the assertions only pass
 * if [ForwardingHandler] delivered the inbound payload bytes to the downstream byte-identically — i.e.
 * only if it used [EdgeClient.requestRaw]. Route the payload through [EdgeClient.request] instead and
 * the downstream typed decode fails (msgpack-bin-wrapped blob ≠ a `ListCharactersRequest` map) → the
 * downstream answers `ok=false` → the whole call fails. That RED-vs-GREEN is the double-encode proof.
 */
class EdgeForwardingTest {

    private val codec = EdgeCodec()

    /** A downstream that never answers — [receive] blocks forever, [send] is swallowed. */
    private class SilentConnection : EdgeConnection {
        private val inbound = LinkedBlockingQueue<ByteArray>()
        override fun receive(): ByteArray? = inbound.take()
        override fun send(frame: ByteArray) = Unit
        override fun close() {
            inbound.put(ByteArray(0)) // unblock the reader; a genuine empty frame just gets dropped
        }
    }

    /**
     * Stands up gateway + typed downstream over two loopback transports and returns a client dialed at
     * the gateway plus the [AtomicReference] into which the downstream records the request it decoded.
     */
    private class Fixture(
        val client: EdgeClient,
        val downstreamSaw: AtomicReference<ListCharactersRequest>,
    )

    private fun standUp(gatewayMethod: String): Fixture {
        val downstreamSaw = AtomicReference<ListCharactersRequest>()

        // Downstream service: a REAL typed handler. It decodes the forwarded bytes into a
        // ListCharactersRequest (recording it) and returns a reply keyed off the decoded playerId, so
        // a correct decode is observable in BOTH the recorded request and the reply contents.
        val downstreamRouter = EdgeRouter().apply {
            register(
                "characters.list",
                codec.typedHandler<ListCharactersRequest, CharactersReply> { req ->
                    downstreamSaw.set(req)
                    CharactersReply(names = listOf("char-of-${req.playerId}"))
                },
            )
        }
        val downstreamTransport = LoopbackTransport()
        EdgeServer(downstreamRouter, downstreamTransport, codec).start()

        // The gateway's cached outbound leg dialing the downstream.
        val outboundClient = EdgeClient(downstreamTransport.connect(), codec).apply { start() }

        // Gateway: a PREFIX forwarder relaying inbound `characters.*` to the downstream via requestRaw.
        val gatewayRouter = EdgeRouter().apply {
            registerPrefix("characters.", ForwardingHandler(gatewayMethod, { outboundClient }))
        }
        val gatewayTransport = LoopbackTransport()
        EdgeServer(gatewayRouter, gatewayTransport, codec).start()

        val client = EdgeClient(gatewayTransport.connect(), codec).apply { start() }
        return Fixture(client, downstreamSaw)
    }

    @Test
    @Timeout(value = 10, unit = TimeUnit.SECONDS, threadMode = Timeout.ThreadMode.SEPARATE_THREAD)
    fun `the gateway prefix-forwards a typed request byte-identically and relays the typed reply`() {
        val fx = standUp(gatewayMethod = "characters.list")

        val reply = fx.client.call("characters.list", ListCharactersRequest("player-7"), CharactersReply::class.java)

        // The downstream typed decoder accepted the forwarded bytes — proof requestRaw delivered them
        // verbatim (a bin-wrapped blob would have failed the decode before this could be set).
        val saw = requireNotNull(fx.downstreamSaw.get()) { "downstream typed handler never decoded a request" }
        assertEquals("player-7", saw.playerId, "downstream must decode the ORIGINAL request bytes")
        assertEquals(listOf("char-of-player-7"), reply.names, "the typed reply must flow back through the gateway")
    }

    @Test
    @Timeout(value = 10, unit = TimeUnit.SECONDS, threadMode = Timeout.ThreadMode.SEPARATE_THREAD)
    fun `a forward to a CLOSED downstream becomes a clean ok=false, not a hang`() {
        // Outbound client whose connection is dead: requestRaw fails fast with ConnectionClosedException,
        // ForwardingHandler maps it to a ForwardFailedException, dispatch → ok=false.
        val deadOutbound = EdgeClient(SilentConnection(), codec).apply { start() }.also { it.close() }
        val gatewayRouter = EdgeRouter().apply {
            registerPrefix("characters.", ForwardingHandler("characters.list", { deadOutbound }))
        }
        val gatewayTransport = LoopbackTransport()
        EdgeServer(gatewayRouter, gatewayTransport, codec).start()
        val client = EdgeClient(gatewayTransport.connect(), codec).apply { start() }

        val resp = client.request("characters.list", ListCharactersRequest("player-7"))

        assertFalse(resp.ok, "a down downstream must surface as ok=false")
        assertTrue(
            resp.error?.contains("forward 'characters.list' failed") == true,
            "was: ${resp.error ?: "<none>"}",
        )
    }

    @Test
    @Timeout(value = 10, unit = TimeUnit.SECONDS, threadMode = Timeout.ThreadMode.SEPARATE_THREAD)
    fun `ForwardingHandler maps a non-answering downstream to ForwardFailedException within its budget`() {
        // A live-but-mute downstream: the outbound requestRaw times out after the SHORT budget, which the
        // handler must convert to a ForwardFailedException (not leak a bare TimeoutException, not hang).
        val muteOutbound = EdgeClient(SilentConnection(), codec).apply { start() }
        val handler = ForwardingHandler("characters.list", { muteOutbound }, budgetMs = 150)

        val ex = assertThrows(ForwardFailedException::class.java) {
            handler.handle(codec.encodePayload(ListCharactersRequest("player-7")))
        }
        assertTrue(
            requireNotNull(ex.message).contains("did not answer"),
            "a downstream timeout must map to a clear forward-failure message, was: ${ex.message}",
        )
    }
}
