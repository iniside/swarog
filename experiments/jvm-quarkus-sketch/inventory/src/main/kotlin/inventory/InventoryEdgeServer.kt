package inventory

import edge.EdgeCodec
import edge.EdgeRouter
import edge.EdgeServer
import edge.msquic.MsQuicServerTransport
import edge.typedHandler
import io.quarkus.narayana.jta.QuarkusTransaction
import io.quarkus.runtime.ShutdownEvent
import io.quarkus.runtime.StartupEvent
import jakarta.enterprise.context.ApplicationScoped
import jakarta.enterprise.event.Observes
import java.util.Optional
import org.eclipse.microprofile.config.inject.ConfigProperty
import platform.RoleConfig

/**
 * The server side of the PLAYER-FACING `inventory.list` capability over the edge RPC core on REAL QUIC —
 * the inventory twin of [characters.CharactersEdgeServer]. `inventory` had no edge server before: its
 * only edge role was as a CLIENT (dialing characters' `ownerOf`). This adds a SERVER so a game client
 * can read a player's/character's holdings over QUIC — and so the gateway has an `inventory.*` prefix to
 * route to. inventory-service is now BOTH a QUIC server (this) AND a QUIC client (its `ownerOf` dial) in
 * one process; Step 1's smoke proved server+client in one JVM is fine.
 *
 * Gated like [characters.CharactersEdgeServer]: ONLY a split process that actually hosts `inventory`
 * stands up the listener (the monolith takes the in-process path and never needs a cert/QUIC; a process
 * without the inventory role has nothing to serve).
 *
 * ONE deliberate difference from the characters server's cert handling: an inventory-active process with
 * NO cert configured SKIPS (loud warning) rather than throwing. The cert's PRESENCE is the real
 * intent-to-serve signal — the production inventory-service sets EDGE_CERT_THUMBPRINT, whereas an in-JVM
 * `@QuarkusTest` may activate `inventory` (as a split simulation, e.g. the admin-degrade test) WITHOUT a
 * cert and must still boot. A hard throw there would break an unrelated test; skip+warn keeps the QUIC
 * server opt-in on cert presence while still serving whenever it is properly configured.
 *
 * Shutdown ordering is the same msquic close-hazard contract as the characters server — a plain
 * `transport.close()` drains connections via RegistrationShutdown before RegistrationClose, so it
 * cannot hang teardown.
 */
@ApplicationScoped
class InventoryEdgeServer(
    private val roleConfig: RoleConfig,
    private val inventory: InventoryModule,
    @param:ConfigProperty(name = "edge.server.inventory.port") private val port: Int,
    // Optional for the SAME reason as CharactersEdgeServer: every process instantiates this bean, but
    // only a split inventory process sets EDGE_CERT_THUMBPRINT; `${EDGE_CERT_THUMBPRINT:}` is empty
    // elsewhere, which a plain String injection would reject — Optional maps absent/empty to empty().
    @param:ConfigProperty(name = "edge.server.cert-thumbprint") private val certThumbprint: Optional<String>,
) {
    @Volatile private var transport: MsQuicServerTransport? = null

    fun start(@Observes ev: StartupEvent) {
        if (roleConfig.isMonolith() || !roleConfig.isActive("inventory")) return
        val thumbprint = certThumbprint.filter { it.isNotBlank() }.orElse(null)
        if (thumbprint == null) {
            System.err.println(
                "[inventory] WARNING: inventory is active but edge.server.cert-thumbprint " +
                    "(EDGE_CERT_THUMBPRINT) is unset — the inventory.list QUIC server will NOT start",
            )
            return
        }

        val codec = EdgeCodec()
        val router = EdgeRouter().apply {
            register(
                "inventory.list",
                codec.typedHandler<ListHoldingsRequest, ListHoldingsReply> { req ->
                    // Same as the characters server: the handler runs on a per-connection worker with no
                    // ambient transaction/CDI context, so wrap the Panache read in a programmatic tx.
                    val holdings = QuarkusTransaction.requiringNew().call {
                        inventory.holdings(Owner(OwnerType.valueOf(req.ownerType), req.ownerId))
                    }
                    ListHoldingsReply(holdings.map { (item, qty) -> HoldingLine(item, qty) })
                },
            )
        }

        val t = MsQuicServerTransport(port, thumbprint)
        EdgeServer(router, t, codec).start()
        transport = t
        println("[inventory] edge QUIC server for inventory.list listening on port $port")
    }

    fun stop(@Observes ev: ShutdownEvent) {
        transport?.let { runCatching { it.close() } }
    }

    /** Test-only view of the QUIC transport: null until [start] stands up the listener, so a role-gating
     *  test can assert the monolith/non-hosting skip returned before any native setup. */
    internal fun transportForTest(): MsQuicServerTransport? = transport
}
