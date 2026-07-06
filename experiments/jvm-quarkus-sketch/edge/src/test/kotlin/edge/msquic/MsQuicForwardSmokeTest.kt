package edge.msquic

import edge.EdgeClient
import edge.EdgeCodec
import edge.EdgeConnection
import edge.EdgeRouter
import edge.EdgeServer
import edge.typedHandler
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assumptions.assumeTrue
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.Timeout
import java.util.concurrent.TimeUnit

/** Wire types for the forward smoke — the gateway relays one of these end to end. */
data class EchoRequest(val text: String)

data class EchoReply(val text: String)

/**
 * Gateway de-risk smoke (Step 1 of the quarkus-gateway plan): prove that ONE JVM can act as a QUIC
 * server which, INSIDE a request handler, forwards through a QUIC client to a SECOND QUIC server and
 * relays the reply — the single unproven native assumption the gateway rests on.
 *
 * Three transports stand up concurrently in this one process:
 *   Client (MsQuicClientTransport) → Gateway (MsQuicServerTransport, its handler forwards via a
 *   MsQuicClientTransport) → Backend (MsQuicServerTransport, answers `svc.echo`) → back again.
 * Each transport builds its OWN [MsQuicLibrary] (the current per-transport model — deliberately NOT
 * hoisted/shared; that is out of scope for this smoke). So this exercises (a) two server transports +
 * two client transports (four msquic registrations) alive in one process, and (b) forwarding through
 * a client from inside a server handler — the gateway's core move.
 *
 * HONEST GAP: this de-risks "can one process forward server→client at all, and tear down cleanly
 * across multiple registrations". It does NOT cover N concurrent inbound connections, config-driven
 * routing, or cached-client reuse under load — those are the Step-3 concurrency risks, not proven here.
 *
 * Gated on a self-signed cert in `cert:\CurrentUser\My` (thumbprint via `edge.test.cert-thumbprint`),
 * exactly like [MsQuicEchoTest]: without a cert it SKIPS rather than fails. The whole test (including
 * teardown) runs under a SEPARATE-THREAD [Timeout] so a shutdown DEADLOCK across two registrations
 * FAILS the test at the deadline instead of hanging CI — teardown-does-not-hang is a hard assertion.
 */
class MsQuicForwardSmokeTest {

    private val thumbprint: String? =
        System.getProperty("edge.test.cert-thumbprint")?.trim()?.takeIf { it.isNotEmpty() }

    @Test
    @Timeout(value = 60, unit = TimeUnit.SECONDS, threadMode = Timeout.ThreadMode.SEPARATE_THREAD)
    fun forwardRoundTripThroughGatewayOverRealQuic() {
        assumeTrue(thumbprint != null, "no server cert thumbprint — skipping live QUIC forward smoke")

        val backendPort = 9444
        val gatewayPort = 9445
        val codec = EdgeCodec()

        // Backend: answers svc.echo with a known transform, so we can prove the reply travelled the
        // full path client → gateway → backend → gateway → client rather than being faked at the gateway.
        val backendRouter = EdgeRouter().apply {
            register(
                "svc.echo",
                codec.typedHandler<EchoRequest, EchoReply> { req -> EchoReply("echo:${req.text}") },
            )
        }
        val backendTransport = MsQuicServerTransport(backendPort, thumbprint!!)

        // The gateway's OUTBOUND leg: its own client transport + EdgeClient dialing the backend, created
        // once and reused by every inbound request — the cached-forwarder shape (single connection here).
        var gatewayOutTransport: MsQuicClientTransport? = null
        var gatewayOutConn: EdgeConnection? = null
        val gatewayTransport = MsQuicServerTransport(gatewayPort, thumbprint)

        var clientTransport: MsQuicClientTransport? = null
        var clientConn: EdgeConnection? = null
        try {
            EdgeServer(backendRouter, backendTransport, codec).start()

            gatewayOutTransport = MsQuicClientTransport()
            val outConn = gatewayOutTransport.connect("localhost", backendPort)
            gatewayOutConn = outConn
            val outboundClient = EdgeClient(outConn, codec).apply { start() }

            // Gateway server: its handler FORWARDS through the outbound client mid-request (blocks on the
            // downstream round-trip on the per-connection thread — the gateway's core, serial move).
            val gatewayRouter = EdgeRouter().apply {
                register(
                    "svc.echo",
                    codec.typedHandler<EchoRequest, EchoReply> { req ->
                        outboundClient.call("svc.echo", req, EchoReply::class.java)
                    },
                )
            }
            EdgeServer(gatewayRouter, gatewayTransport, codec).start()

            clientTransport = MsQuicClientTransport()
            val cConn = clientTransport.connect("localhost", gatewayPort)
            clientConn = cConn
            val client = EdgeClient(cConn, codec).apply { start() }

            // A few iterations to shake out registration/threading flakiness across the two registrations.
            repeat(FORWARD_ITERATIONS) { i ->
                val reply = client.call("svc.echo", EchoRequest("msg-$i"), EchoReply::class.java)
                assertEquals("echo:msg-$i", reply.text, "reply must flow client→gateway→backend→client (iter $i)")
            }
        } finally {
            // Close connections FIRST, then transports (RegistrationClose blocks until connections drain).
            // Order: client hop, then the gateway's outbound hop, then the two server transports. Under
            // the SEPARATE-THREAD @Timeout above, any deadlock here FAILS the test rather than hanging.
            runCatching { clientConn?.close() }
            runCatching { clientTransport?.close() }
            runCatching { gatewayOutConn?.close() }
            runCatching { gatewayOutTransport?.close() }
            runCatching { gatewayTransport.close() }
            runCatching { backendTransport.close() }
        }
    }

    private companion object {
        const val FORWARD_ITERATIONS = 5
    }
}
