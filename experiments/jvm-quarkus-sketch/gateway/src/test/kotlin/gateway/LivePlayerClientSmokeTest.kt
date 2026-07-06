package gateway

import edge.EdgeClient
import edge.EdgeCodec
import edge.EdgeConnection
import edge.msquic.MsQuicClientTransport
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Assumptions.assumeTrue
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.Timeout
import java.util.concurrent.TimeUnit

/**
 * Wire DTOs for the LIVE player-client smoke — deliberately DEFINED HERE rather than imported. The
 * gateway test source set links only `edge` + `platform` (never a feature impl), so it has no access to
 * `characters-api`'s `ListCharactersRequest`/`ListCharactersReply` or `inventory`'s
 * `ListHoldingsRequest`/`ListHoldingsReply`. MessagePack is schemaless (Jackson reflects field name →
 * value), so a structurally-identical data class round-trips against the real handler byte-for-byte —
 * exactly the pattern [inventory.EdgeDtos]' doc endorses ("a test player client defines its own
 * structurally-identical types"). If these drift from the server DTOs the decode fails loudly.
 */
private data class ListCharactersRequest(val playerId: String)

private data class CharacterSummary(val id: Long, val name: String)

private data class ListCharactersReply(val characters: List<CharacterSummary>)

private data class ListHoldingsRequest(val ownerType: String, val ownerId: String)

private data class HoldingLine(val item: String, val qty: Int)

private data class ListHoldingsReply(val holdings: List<HoldingLine>)

/**
 * THE MARQUEE, driven LIVE over REAL QUIC against a running 3-process split (install.ps1 -Mode
 * microservices). Unlike [GatewayRoutingTest] (in-JVM loopback, routing logic only) this dials the
 * ACTUAL gateway process's external QUIC port (:9200) as a game client would, and asserts each method
 * family is routed to a DIFFERENT backend process:
 *
 *   player (this) --QUIC--> gateway-service :9200
 *                              characters.* --QUIC--> characters-service :9100  (returns a seeded char)
 *                              inventory.*  --QUIC--> inventory-service  :9101  (returns a granted holding)
 *
 * Self-skips (Assumptions.assumeTrue) unless `gateway.smoke.host` is set — so it is INERT in the normal
 * `./gradlew test` sweep (no live split there) and only fires when the driver passes the live coordinates
 * (host + the seeded playerId/characterId) as -D system properties, forwarded by gateway/build.gradle.kts.
 *
 * Two legs, each its own test so the driver runs one, kills a backend, then runs the other:
 *  - [bothFamiliesRouteThroughGatewayLive]  — the all-up proof (both prefixes route, correct data).
 *  - [charactersDownStillLetsInventoryThroughLive] — degradation: characters-service killed ⇒ the
 *    gateway returns a clean ok=false for characters.* (NOT a hang), while inventory.* still works.
 */
class LivePlayerClientSmokeTest {

    private val codec = EdgeCodec()

    private fun optionalProp(name: String): String? =
        System.getProperty(name)?.trim()?.takeIf { it.isNotEmpty() }

    private fun requireProp(name: String): String =
        requireNotNull(optionalProp(name)) { "live smoke requires -D$name" }

    private fun gatewayHostOrSkip(): String {
        val host = optionalProp("gateway.smoke.host")
        assumeTrue(host != null, "gateway.smoke.host unset — skipping LIVE gateway smoke")
        return requireNotNull(host)
    }

    private val port: Int get() = optionalProp("gateway.smoke.port")?.toInt() ?: DEFAULT_GATEWAY_QUIC_PORT

    /** Dials the live gateway QUIC port, runs [body] with a started client, tears the connection down. */
    private fun <T> withPlayerClient(host: String, body: (EdgeClient) -> T): T {
        val transport = MsQuicClientTransport()
        var conn: EdgeConnection? = null
        try {
            val c = transport.connect(host, port)
            conn = c
            return body(EdgeClient(c, codec).apply { start() })
        } finally {
            runCatching { conn?.close() }
            runCatching { transport.close() }
        }
    }

    @Test
    @Timeout(value = 60, unit = TimeUnit.SECONDS, threadMode = Timeout.ThreadMode.SEPARATE_THREAD)
    fun bothFamiliesRouteThroughGatewayLive() {
        val host = gatewayHostOrSkip()
        val playerId = requireProp("gateway.smoke.playerId")
        val expectedName = requireProp("gateway.smoke.characterName")
        val characterId = requireProp("gateway.smoke.characterId")

        withPlayerClient(host) { client ->
            // characters.* -> characters-service :9100 -----------------------------------------------
            val chars = client.call(
                "characters.list", ListCharactersRequest(playerId), ListCharactersReply::class.java,
            )
            val names = chars.characters.map { it.name }
            println("[live-smoke] characters.list($playerId) via gateway -> $names")
            assertTrue(
                names.contains(expectedName),
                "characters.list must route to characters-service and return the seeded '$expectedName'; got $names",
            )

            // inventory.* -> inventory-service :9101 (a DIFFERENT backend) ----------------------------
            val inv = client.call(
                "inventory.list", ListHoldingsRequest("CHARACTER", characterId), ListHoldingsReply::class.java,
            )
            val items = inv.holdings.map { it.item }
            println("[live-smoke] inventory.list(CHARACTER,$characterId) via gateway -> ${inv.holdings}")
            assertTrue(
                items.contains("starter_sword"),
                "inventory.list must route to inventory-service and return the granted starter_sword; got $items",
            )
        }
    }

    @Test
    @Timeout(value = 60, unit = TimeUnit.SECONDS, threadMode = Timeout.ThreadMode.SEPARATE_THREAD)
    fun charactersDownStillLetsInventoryThroughLive() {
        val host = gatewayHostOrSkip()
        assumeTrue(
            optionalProp("gateway.smoke.charactersDown") != null,
            "gateway.smoke.charactersDown unset — skipping the degradation leg",
        )
        val playerId = requireProp("gateway.smoke.playerId")
        val characterId = requireProp("gateway.smoke.characterId")

        withPlayerClient(host) { client ->
            // characters-service is DOWN: the gateway's forward fails and must surface a clean ok=false,
            // never a hang. A generous player timeout lets the gateway's bounded reconnect run to its
            // clean failure rather than the player itself timing out first.
            val down = client.requestRaw(
                "characters.list", codec.encodePayload(ListCharactersRequest(playerId)), DEGRADED_TIMEOUT_MS,
            )
            println("[live-smoke] characters.list with characters-service DOWN -> ok=${down.ok} err=${down.error}")
            assertFalse(down.ok, "a killed characters-service must surface as ok=false, not a hang")

            // The gateway itself stays UP and only the killed prefix fails: inventory.* still works.
            val inv = client.call(
                "inventory.list", ListHoldingsRequest("CHARACTER", characterId), ListHoldingsReply::class.java,
            )
            println("[live-smoke] inventory.list still works with characters DOWN -> ${inv.holdings}")
            assertTrue(
                inv.holdings.map { it.item }.contains("starter_sword"),
                "inventory.* must keep working when only characters-service is down",
            )
        }
    }

    private companion object {
        const val DEFAULT_GATEWAY_QUIC_PORT = 9200
        const val DEGRADED_TIMEOUT_MS = 20_000L
    }
}
