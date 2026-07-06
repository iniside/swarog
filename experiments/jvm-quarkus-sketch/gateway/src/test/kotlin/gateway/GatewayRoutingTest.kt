package gateway

import edge.EdgeClient
import edge.EdgeCodec
import edge.EdgeConnection
import edge.EdgeRouter
import edge.EdgeServer
import edge.LoopbackTransport
import edge.typedHandler
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.Timeout
import java.util.concurrent.TimeUnit

/** Test wire types — the gateway byte-relays these end to end without ever decoding them itself. */
private data class ListReq(val who: String)

private data class ListReply(val items: List<String>)

/**
 * The marquee proof for Step 3, driven over the in-JVM loopback transport (no native QUIC) so the
 * ROUTING LOGIC is proven independently of msquic:
 *
 *   test PLAYER client → GatewayEdgeServer's router ([RoutedBackend] prefix forwarders)
 *        → characters backend  (a REAL typedHandler for `characters.*`)
 *        → inventory backend   (a REAL typedHandler for `inventory.*`)
 *
 * The two prefixes route to DIFFERENT downstream services, and each returns its own data — that is the
 * whole point (an external front door that fans BOTH method families to the owning service). The
 * downstreams are TYPED handlers registered under the EXACT method names, so a reply only comes back if
 * [RoutedBackend] (a) selected the right backend by prefix and (b) forwarded the ORIGINAL method
 * (method-transparency), byte-identically (requestRaw). A down backend must degrade to a clean
 * `ok=false`, never a hang.
 */
class GatewayRoutingTest {

    private val codec = EdgeCodec()

    private class Fixture(val client: EdgeClient)

    /** A backend EdgeServer over its own loopback transport; returns a connect-fake dialing it. */
    private fun backend(register: EdgeRouter.() -> Unit): Pair<LoopbackTransport, (String, Int) -> EdgeConnection> {
        val transport = LoopbackTransport()
        EdgeServer(EdgeRouter().apply(register), transport, codec).start()
        return transport to { _, _ -> transport.connect() }
    }

    private fun standUp(): Fixture {
        // Characters backend: two DISTINCT methods so method-transparency is observable — a fixed-method
        // forwarder would collapse both to one and one of them would 404.
        val (_, charsConnect) = backend {
            register(
                "characters.list",
                codec.typedHandler<ListReq, ListReply> { req -> ListReply(listOf("chars-list:${req.who}")) },
            )
            register(
                "characters.whoami",
                codec.typedHandler<ListReq, ListReply> { req -> ListReply(listOf("chars-whoami:${req.who}")) },
            )
        }
        // Inventory backend: a DIFFERENT service, its own method + data.
        val (_, invConnect) = backend {
            register(
                "inventory.list",
                codec.typedHandler<ListReq, ListReply> { req -> ListReply(listOf("inv-list:${req.who}")) },
            )
        }

        val gatewayRouter = EdgeRouter().apply {
            registerPrefix("characters.", RoutedBackend("chars:1", connect = charsConnect))
            registerPrefix("inventory.", RoutedBackend("inv:1", connect = invConnect))
        }
        val gatewayTransport = LoopbackTransport()
        EdgeServer(gatewayRouter, gatewayTransport, codec).start()

        return Fixture(EdgeClient(gatewayTransport.connect(), codec).apply { start() })
    }

    @Test
    @Timeout(value = 10, unit = TimeUnit.SECONDS, threadMode = Timeout.ThreadMode.SEPARATE_THREAD)
    fun `the gateway prefix-routes each family to a DIFFERENT downstream service`() {
        val fx = standUp()

        val chars = fx.client.call("characters.list", ListReq("player-7"), ListReply::class.java)
        val inv = fx.client.call("inventory.list", ListReq("player-7"), ListReply::class.java)

        assertEquals(listOf("chars-list:player-7"), chars.items, "characters.* must route to the characters backend")
        assertEquals(listOf("inv-list:player-7"), inv.items, "inventory.* must route to the inventory backend")
    }

    @Test
    @Timeout(value = 10, unit = TimeUnit.SECONDS, threadMode = Timeout.ThreadMode.SEPARATE_THREAD)
    fun `a prefix forwarder preserves the ORIGINAL method (method-transparency), not a fixed one`() {
        val fx = standUp()

        // Both methods share the `characters.` prefix and the SAME forwarder; each must reach its own
        // downstream handler — proof the inbound method name survived the relay.
        val list = fx.client.call("characters.list", ListReq("p"), ListReply::class.java)
        val who = fx.client.call("characters.whoami", ListReq("p"), ListReply::class.java)

        assertEquals(listOf("chars-list:p"), list.items)
        assertEquals(listOf("chars-whoami:p"), who.items)
    }

    @Test
    @Timeout(value = 10, unit = TimeUnit.SECONDS, threadMode = Timeout.ThreadMode.SEPARATE_THREAD)
    fun `a down backend degrades to a clean ok=false, not a hang`() {
        // Connect fake that always fails to dial — models a downstream service that is not up.
        val downConnect: (String, Int) -> EdgeConnection = { _, _ -> error("backend down") }
        val gatewayRouter = EdgeRouter().apply {
            registerPrefix("inventory.", RoutedBackend("inv:1", budgetMs = 200, connect = downConnect))
        }
        val gatewayTransport = LoopbackTransport()
        EdgeServer(gatewayRouter, gatewayTransport, codec).start()
        val client = EdgeClient(gatewayTransport.connect(), codec).apply { start() }

        val resp = client.request("inventory.list", ListReq("player-7"))

        assertFalse(resp.ok, "a down backend must surface as ok=false")
        assertTrue(
            resp.error?.contains("forward 'inventory.list'") == true,
            "was: ${resp.error ?: "<none>"}",
        )
    }
}
