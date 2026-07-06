package gateway

import edge.EdgeCodec
import edge.EdgeRouter
import edge.EdgeServer
import edge.msquic.MsQuicServerTransport
import io.quarkus.runtime.ShutdownEvent
import io.quarkus.runtime.StartupEvent
import jakarta.enterprise.context.ApplicationScoped
import jakarta.enterprise.event.Observes
import java.util.Optional
import org.eclipse.microprofile.config.inject.ConfigProperty
import platform.RoleConfig

/**
 * The external QUIC front door: an [EdgeServer] whose router prefix-forwards each method family to the
 * owning service. `characters.*` relays to the characters service and `inventory.*` to the inventory
 * service, each via a cached outbound [RoutedBackend]. It byte-relays (never decodes a domain DTO), so
 * the gateway links no feature impl — recomposing the `edge` stack is its whole implementation.
 *
 * Routing table = PLAIN host:port config (NOT Stork): `gateway.route.characters.target` /
 * `gateway.route.inventory.target`, defaulting to the same `CHARACTERS_EDGE_ADDR`/`INVENTORY_EDGE_ADDR`
 * envs the split already uses. Programmatic Stork is used nowhere in this repo; the QUIC seam resolves
 * targets by plain host:port, and the gateway reuses exactly that.
 *
 * Role-gated like the other edge servers: only a process whose role IS `gateway` (and never the
 * monolith) stands up the listener. Needs a schannel cert (EDGE_CERT_THUMBPRINT) for its QUIC server,
 * the same constraint as [characters.CharactersEdgeServer]; absent/empty ⇒ loud startup failure.
 *
 * HONEST LIMIT (v1): [EdgeServer] dispatches inline on the per-connection thread and [RoutedBackend]
 * BLOCKS on the outbound round-trip, so within ONE inbound connection there is no pipelining
 * (head-of-line). Different players (different QUIC connections) don't block each other. v1 proves
 * routing CORRECTNESS, not gateway CONCURRENCY — a worker-pool [EdgeServer] is future work.
 */
@ApplicationScoped
class GatewayEdgeServer(
    private val roleConfig: RoleConfig,
    @param:ConfigProperty(name = "gateway.quic.port") private val port: Int,
    @param:ConfigProperty(name = "gateway.route.characters.target") private val charactersTarget: String,
    @param:ConfigProperty(name = "gateway.route.inventory.target") private val inventoryTarget: String,
    @param:ConfigProperty(name = "edge.server.cert-thumbprint") private val certThumbprint: Optional<String>,
) {
    @Volatile private var transport: MsQuicServerTransport? = null
    @Volatile private var backends: List<RoutedBackend> = emptyList()

    fun start(@Observes ev: StartupEvent) {
        if (roleConfig.isMonolith() || !roleConfig.isActive("gateway")) return
        val thumbprint = certThumbprint.filter { it.isNotBlank() }.orElseThrow {
            IllegalStateException(
                "edge.server.cert-thumbprint (EDGE_CERT_THUMBPRINT) is required to host the gateway QUIC server",
            )
        }

        val characters = RoutedBackend(charactersTarget)
        val inventory = RoutedBackend(inventoryTarget)
        backends = listOf(characters, inventory)

        val codec = EdgeCodec()
        val router = EdgeRouter().apply {
            registerPrefix("characters.", characters)   // MethodForward overload — forwards the real method
            registerPrefix("inventory.", inventory)
        }

        val t = MsQuicServerTransport(port, thumbprint)
        EdgeServer(router, t, codec).start()
        transport = t
        println(
            "[gateway] edge QUIC router listening on port $port " +
                "(characters.* -> $charactersTarget, inventory.* -> $inventoryTarget)",
        )
    }

    fun stop(@Observes ev: ShutdownEvent) {
        transport?.let { runCatching { it.close() } }
        backends.forEach { runCatching { it.close() } }
    }

    /** Test-only view of the QUIC transport: null until [start] stands up the listener, so a role-gating
     *  test can assert the monolith/non-gateway skip returned before any native setup. */
    internal fun transportForTest(): MsQuicServerTransport? = transport
}
