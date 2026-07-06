package characters

import characters.charactersapi.OwnerOfReply
import characters.charactersapi.OwnerOfRequest
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
 * The server side of the `ownerOf` capability over the edge RPC core on REAL QUIC — the msquic
 * replacement for the deleted gRPC service. It exposes the SAME [LocalPlayerCharacters] lookup on the
 * wire so a process that does NOT host `characters` (the split `inventory` process) can still ask, via
 * the [EdgeRemotePlayerCharacters] client adapter in [PlayerCharactersProvider].
 *
 * ONLY a split process that actually hosts `characters` stands up the QUIC listener: the monolith
 * (roles=all) takes the in-process local branch and never needs a cert/QUIC, and a process without the
 * characters role has no lookup to serve. If active but no cert thumbprint is configured, startup fails
 * loudly rather than silently serving nothing (schannel needs the CurrentUser-store cert).
 *
 * Shutdown ordering (the msquic close hazard): [MsQuicServerTransport.close] issues a
 * RegistrationShutdown (drains all connections via the registration handle) BEFORE the blocking
 * RegistrationClose, so a plain `transport.close()` here is safe and cannot hang the JVM ~90s on
 * shutdown even with a peer still connected.
 */
@ApplicationScoped
class CharactersEdgeServer(
    private val roleConfig: RoleConfig,
    private val local: LocalPlayerCharacters,
    @param:ConfigProperty(name = "edge.server.characters.port") private val port: Int,
    // Optional: EVERY process instantiates this bean (single artifact), but only a split characters
    // process sets EDGE_CERT_THUMBPRINT. `${EDGE_CERT_THUMBPRINT:}` resolves to empty in the others,
    // which SmallRye rejects for a plain String injection — Optional maps absent/empty to empty().
    @param:ConfigProperty(name = "edge.server.cert-thumbprint") private val certThumbprint: Optional<String>,
) {
    @Volatile private var transport: MsQuicServerTransport? = null

    fun start(@Observes ev: StartupEvent) {
        if (roleConfig.isMonolith() || !roleConfig.isActive("characters")) return
        val thumbprint = certThumbprint.filter { it.isNotBlank() }.orElseThrow {
            IllegalStateException(
                "edge.server.cert-thumbprint (EDGE_CERT_THUMBPRINT) is required to host the characters QUIC server",
            )
        }

        val codec = EdgeCodec()
        val router = EdgeRouter().apply {
            register(
                "characters.ownerOf",
                codec.typedHandler<OwnerOfRequest, OwnerOfReply> { req ->
                    // The handler runs on the EdgeServer's per-connection worker thread, which has no
                    // ambient transaction/CDI request context — Panache (Character.findById) needs an
                    // active Hibernate session, so wrap the read in a programmatic transaction.
                    val owner = QuarkusTransaction.requiringNew().call { local.ownerOf(req.characterId) }
                    OwnerOfReply(found = owner != null, ownerId = owner?.toString())
                },
            )
        }

        val t = MsQuicServerTransport(port, thumbprint)
        EdgeServer(router, t, codec).start()
        transport = t
        println("[characters] edge QUIC server for characters.ownerOf listening on port $port")
    }

    fun stop(@Observes ev: ShutdownEvent) {
        transport?.let { runCatching { it.close() } }
    }

    /** Test-only view of the QUIC transport: null until [start] actually stands up the listener, so a
     *  role-gating test can assert the monolith/non-hosting skip returned before any native setup. */
    internal fun transportForTest(): MsQuicServerTransport? = transport
}
